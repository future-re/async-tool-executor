//! `glob` tool — list files matching a glob pattern under the workspace.

use async_trait::async_trait;
use executor_core::{ExecutionError, Tool, ToolContext, ToolDefinition, ToolOutput};
use serde_json::{Value, json};

use std::path::{Path, PathBuf};

use crate::shared::{canonicalize_within_cwd, display_relative, execution_error, required_string};

/// Built-in glob tool. Read-only; safe to run concurrently.
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Glob".into(),
            description: "List files matching a glob pattern under the current working directory."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
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
        let pattern = required_string(&arguments, "pattern")?;
        // Cap results so a pathological `**/*` doesn't dump millions of paths into the context.
        let max_results = arguments
            .get("max_results")
            .and_then(|value| value.as_u64())
            .unwrap_or(200) as usize;
        let full_pattern = if Path::new(pattern).is_absolute() {
            let anchor = absolute_glob_anchor(pattern);
            canonicalize_within_cwd(&context.cwd, &anchor)
                .await
                .map_err(|_| {
                    execution_error(format!(
                        "absolute glob pattern must stay under cwd: {pattern}"
                    ))
                })?;
            pattern.to_string()
        } else {
            // Anchor relative patterns at cwd so the model can write concise globs.
            context.cwd.join(pattern).to_string_lossy().to_string()
        };
        let mut matches = Vec::new();
        for entry in glob::glob(&full_pattern).map_err(|err| ExecutionError::ToolValidation {
            tool: "Glob".into(),
            message: err.to_string(),
        })? {
            if matches.len() >= max_results {
                break;
            }
            if let Ok(path) = entry {
                // Defensive: `../foo` style patterns can still resolve outside cwd, and
                // a symlink inside cwd may point outside. Follow symlinks and reject
                // any file whose canonical location is not under cwd.
                if canonicalize_within_cwd(&context.cwd, &path).await.is_err() {
                    continue;
                }
                // Display paths relative to cwd; absolute paths are noisy and leak the host layout.
                matches.push(display_relative(&context.cwd, &path));
            }
        }
        Ok(ToolOutput::json(json!({ "matches": matches })))
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
    async fn rejects_absolute_pattern_outside_cwd() {
        let dir = std::env::temp_dir().join("ate_glob_absolute_cwd_test");
        let outside = std::env::temp_dir().join("ate_glob_absolute_outside_test");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let tool = GlobTool;
        let pattern = outside.join("*").to_string_lossy().to_string();
        let result = tool
            .invoke(json!({ "pattern": pattern }), context(dir.clone()))
            .await;
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn accepts_absolute_pattern_under_cwd() {
        let dir = std::env::temp_dir().join("ate_glob_absolute_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sample.txt"), "x").unwrap();

        let tool = GlobTool;
        let pattern = dir.join("*.txt").to_string_lossy().to_string();
        let output = tool
            .invoke(json!({ "pattern": pattern }), context(dir.clone()))
            .await
            .unwrap();
        let matches = output.content["matches"].as_array().unwrap();
        assert_eq!(matches, &vec![json!("sample.txt")]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escape() {
        let dir = std::env::temp_dir().join("ate_glob_symlink_test");
        let outside = std::env::temp_dir().join("ate_glob_symlink_outside");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "x").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), dir.join("link.txt")).unwrap();

        let tool = GlobTool;
        let output = tool
            .invoke(json!({ "pattern": "**/*" }), context(dir.clone()))
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
