//! `grep` tool — substring search over files matched by a glob.
//!
//! Matches are *literal* substring searches (no regex). For each hit we emit
//! the path, 1-indexed line number, and the matched line — enough context for
//! the model to follow up with [`FileReadTool`](crate::FileReadTool).

use async_trait::async_trait;
use executor_core::{ExecutionError, Tool, ToolContext, ToolDefinition, ToolOutput};
use serde_json::{Value, json};

use std::path::{Path, PathBuf};

use crate::shared::{
    DEFAULT_MAX_FILE_READ_BYTES, canonicalize_within_cwd, display_relative, execution_error,
    required_string,
};

/// Built-in grep tool. Read-only; safe to run concurrently.
pub struct GrepTool {
    /// Skip files larger than this many bytes when searching.
    pub max_file_read_bytes: usize,
}

impl Default for GrepTool {
    fn default() -> Self {
        Self {
            max_file_read_bytes: DEFAULT_MAX_FILE_READ_BYTES,
        }
    }
}

impl GrepTool {
    pub fn new(max_file_read_bytes: usize) -> Self {
        Self {
            max_file_read_bytes,
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Grep".into(),
            description: "Search UTF-8 files for a literal text pattern.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "glob": { "type": "string" },
                    "max_results": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn is_concurrency_safe(&self, _arguments: &Value) -> bool {
        true
    }

    fn invocation_detail(&self, arguments: &Value) -> String {
        required_string(arguments, "pattern")
            .unwrap_or("")
            .to_string()
    }

    async fn validate(
        &self,
        arguments: &Value,
        _context: &ToolContext,
    ) -> Result<(), ExecutionError> {
        required_string(arguments, "pattern").map(|_| ())
    }

    async fn invoke(
        &self,
        arguments: Value,
        context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        let pattern = required_string(&arguments, "pattern")?.to_string();
        // Default to a recursive glob so plain "grep for X" works out of the box.
        let file_glob = arguments
            .get("glob")
            .and_then(|value| value.as_str())
            .unwrap_or("**/*");
        let max_results = arguments
            .get("max_results")
            .and_then(|value| value.as_u64())
            .unwrap_or(200) as usize;
        let full_pattern = if Path::new(file_glob).is_absolute() {
            let anchor = absolute_glob_anchor(file_glob);
            canonicalize_within_cwd(&context.cwd, &anchor)
                .await
                .map_err(|_| {
                    execution_error(format!(
                        "absolute glob pattern must stay under cwd: {file_glob}"
                    ))
                })?;
            file_glob.to_string()
        } else {
            context.cwd.join(file_glob).to_string_lossy().to_string()
        };
        let mut results = Vec::new();
        for entry in glob::glob(&full_pattern).map_err(|err| ExecutionError::ToolValidation {
            tool: "Grep".into(),
            message: err.to_string(),
        })? {
            if results.len() >= max_results {
                break;
            }
            let Ok(path) = entry else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            // Defensive: `../foo` style globs can still resolve outside cwd, and
            // a symlink inside cwd may point outside. Follow symlinks and reject
            // any file whose canonical location is not under cwd.
            let canonical_path = match canonicalize_within_cwd(&context.cwd, &path).await {
                Ok(p) => p,
                Err(_) => continue,
            };
            // Skip files that exceed the configured read budget.
            if let Ok(metadata) = tokio::fs::metadata(&canonical_path).await
                && metadata.len() > self.max_file_read_bytes as u64
            {
                continue;
            }
            // Silently skip files we can't read as UTF-8 (binary, permissions, etc.).
            let Ok(content) = tokio::fs::read_to_string(&canonical_path).await else {
                continue;
            };
            for (idx, line) in content.lines().enumerate() {
                if line.contains(&pattern) {
                    results.push(json!({
                        "path": display_relative(&context.cwd, &path),
                        "line": idx + 1,
                        "text": line,
                    }));
                    if results.len() >= max_results {
                        break;
                    }
                }
            }
        }
        Ok(ToolOutput::json(json!({ "matches": results })))
    }
}

fn absolute_glob_anchor(pattern: &str) -> PathBuf {
    let mut anchor = PathBuf::new();
    for component in Path::new(pattern).components() {
        let text = component.as_os_str().to_string_lossy();
        if text.contains('*') || text.contains('?') || text.contains('[') {
            break;
        }
        anchor.push(component.as_os_str());
    }
    anchor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::context;
    use serde_json::json;

    #[tokio::test]
    async fn rejects_absolute_glob() {
        let dir = std::env::temp_dir().join("ate_grep_absolute_cwd_test");
        let outside = std::env::temp_dir().join("ate_grep_absolute_outside_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let tool = GrepTool::default();
        let file_glob = outside.join("*").to_string_lossy().to_string();
        let result = tool
            .invoke(
                json!({ "pattern": "root", "glob": file_glob }),
                context(dir.clone()),
            )
            .await;
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn accepts_absolute_glob_under_cwd() {
        let dir = std::env::temp_dir().join("ate_grep_absolute_under_cwd_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sample.txt"), "alpha\nneedle\n").unwrap();

        let tool = GrepTool::default();
        let file_glob = dir.join("*.txt").to_string_lossy().to_string();
        let output = tool
            .invoke(
                json!({ "pattern": "needle", "glob": file_glob }),
                context(dir.clone()),
            )
            .await
            .unwrap();
        let matches = output.content["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["path"], "sample.txt");
        assert_eq!(matches[0]["line"], 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escape() {
        let dir = std::env::temp_dir().join("ate_grep_symlink_test");
        let outside = std::env::temp_dir().join("ate_grep_symlink_outside");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "match").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), dir.join("link.txt")).unwrap();

        let tool = GrepTool::default();
        let output = tool
            .invoke(json!({ "pattern": "match" }), context(dir.clone()))
            .await
            .unwrap();
        let matches = output.content["matches"].as_array().unwrap();
        assert!(
            matches.is_empty(),
            "symlink escape should produce no matches"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
