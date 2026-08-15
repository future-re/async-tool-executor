//! `Edit` tool — exact-match find-and-replace on a single UTF-8 text file.
//!
//! Requires the `old` string to occur **exactly once** in the file. This is a
//! deliberate guard against ambiguous edits — if the model wants to replace a
//! common token it must include enough surrounding context to make the match
//! unique.

use async_trait::async_trait;
use executor_core::{ExecutionError, Tool, ToolContext, ToolDefinition, ToolOutput};
use serde_json::{Value, json};

use crate::shared::{
    FileReadRecord, FileReadState, canonicalize_within_cwd, ensure_file_was_read_and_unchanged,
    modified_timestamp_ms, optional_bool, required_string_any, resolve_workspace_path,
};

/// Built-in file-edit tool. Performs a single, unambiguous string replacement.
pub struct FileEditTool {
    read_file_state: FileReadState,
}

impl FileEditTool {
    pub fn new(read_file_state: FileReadState) -> Self {
        Self { read_file_state }
    }
}

#[async_trait]
impl Tool for FileEditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Edit".into(),
            description: "Replace an exact string in a UTF-8 text file. Existing files must be read first with Read."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the file to edit, absolute or relative to cwd." },
                    "old_string": { "type": "string", "description": "Exact string to replace. Use an empty string only to create a new file." },
                    "new_string": { "type": "string", "description": "Replacement string." },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence. Defaults to false." }
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        }
    }

    fn invocation_detail(&self, arguments: &Value) -> String {
        required_string_any(arguments, &["file_path", "path"])
            .unwrap_or("")
            .to_string()
    }

    async fn validate(
        &self,
        arguments: &Value,
        _context: &ToolContext,
    ) -> Result<(), ExecutionError> {
        required_string_any(arguments, &["file_path", "path"])?;
        required_string_any(arguments, &["old_string", "old"])?;
        required_string_any(arguments, &["new_string", "new"])?;
        Ok(())
    }

    async fn invoke(
        &self,
        arguments: Value,
        context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        let input_path = required_string_any(&arguments, &["file_path", "path"])?;
        let path = resolve_workspace_path(&context.cwd, input_path)?;
        let path = canonicalize_within_cwd(&context.cwd, &path).await?;
        if path.extension().and_then(|ext| ext.to_str()) == Some("ipynb") {
            return Err(ExecutionError::ToolExecution {
                tool: "Edit".into(),
                message: "File is a Jupyter Notebook. Use a notebook-aware edit tool instead."
                    .into(),
            });
        }
        let old = required_string_any(&arguments, &["old_string", "old"])?;
        let new = required_string_any(&arguments, &["new_string", "new"])?;
        if old == new {
            return Err(ExecutionError::ToolExecution {
                tool: "Edit".into(),
                message: "No changes to make: old_string and new_string are exactly the same."
                    .into(),
            });
        }
        let replace_all = optional_bool(&arguments, "replace_all", false);

        // Capture pre-existing permissions so we can restore them after writing.
        let existing_metadata = tokio::fs::metadata(&path).await.ok();
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && old.is_empty() => {
                String::new()
            }
            Err(err) => {
                return Err(ExecutionError::ToolExecution {
                    tool: "Edit".into(),
                    message: err.to_string(),
                });
            }
        };

        if !old.is_empty() {
            ensure_file_was_read_and_unchanged("Edit", &self.read_file_state, &path, &content)
                .await?;
        } else if !content.trim().is_empty() {
            return Err(ExecutionError::ToolExecution {
                tool: "Edit".into(),
                message: "Cannot create new file - file already exists.".into(),
            });
        }

        // Reject ambiguous (0 or >1) matches so the model is forced to widen
        // the snippet rather than silently editing the wrong location.
        let count = if old.is_empty() {
            1
        } else {
            content.matches(old).count()
        };
        if count == 0 {
            return Err(ExecutionError::ToolExecution {
                tool: "Edit".into(),
                message: format!("String to replace not found in file.\nString: {old}"),
            });
        }
        if count > 1 && !replace_all {
            return Err(ExecutionError::ToolExecution {
                tool: "Edit".into(),
                message: format!(
                    "Found {count} matches of the string to replace, but replace_all is false. Provide more context to uniquely identify the instance or set replace_all to true.\nString: {old}"
                ),
            });
        }
        let updated = if old.is_empty() {
            new.to_string()
        } else if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                ExecutionError::ToolExecution {
                    tool: "Edit".into(),
                    message: err.to_string(),
                }
            })?;
        }
        tokio::fs::write(&path, updated)
            .await
            .map_err(|err| ExecutionError::ToolExecution {
                tool: "Edit".into(),
                message: err.to_string(),
            })?;
        // Preserve the original mode bits when editing an existing file.
        if let Some(metadata) = existing_metadata
            && let Err(err) = tokio::fs::set_permissions(&path, metadata.permissions()).await
        {
            return Err(ExecutionError::ToolExecution {
                tool: "Edit".into(),
                message: format!("failed to restore file permissions: {err}"),
            });
        }
        let updated_content = tokio::fs::read_to_string(&path).await.map_err(|err| {
            ExecutionError::ToolExecution {
                tool: "Edit".into(),
                message: err.to_string(),
            }
        })?;
        let timestamp_ms = modified_timestamp_ms(&path).await?;
        self.read_file_state
            .lock()
            .expect("read-file-state lock poisoned")
            .insert(
                path.clone(),
                FileReadRecord {
                    content: updated_content,
                    timestamp_ms,
                    is_partial_view: false,
                    offset: None,
                    limit: None,
                },
            );
        Ok(ToolOutput::json(json!({
            "file_path": input_path,
            "path": path,
            "replaced": true,
            "replace_all": replace_all,
        })))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::context;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn preserves_file_permissions() {
        let dir = std::env::temp_dir().join("ate_edit_perms_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("script.sh");
        std::fs::write(&file, "#!/bin/sh\necho old\n").unwrap();
        // Set an unusual mode so we can detect if it is lost.
        let mut perms = std::fs::metadata(&file).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&file, perms).unwrap();

        // Prime the read-file state so the edit is allowed.
        let state = FileReadState::default();
        state.lock().unwrap().insert(
            file.clone(),
            FileReadRecord {
                content: "#!/bin/sh\necho old\n".into(),
                timestamp_ms: 0,
                is_partial_view: false,
                offset: None,
                limit: None,
            },
        );

        let tool = FileEditTool::new(state);
        tool.invoke(
            json!({
                "file_path": "script.sh",
                "old_string": "echo old",
                "new_string": "echo new"
            }),
            context(dir.clone()),
        )
        .await
        .unwrap();

        let new_mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(
            new_mode & 0o777,
            0o755,
            "file permissions were not preserved"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rejects_ambiguous_match_without_replace_all() {
        let dir = std::env::temp_dir().join("ate_edit_ambiguous_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "dup dup\n").unwrap();

        let state = FileReadState::default();
        state.lock().unwrap().insert(
            dir.join("f.txt"),
            FileReadRecord {
                content: "dup dup\n".into(),
                timestamp_ms: 0,
                is_partial_view: false,
                offset: None,
                limit: None,
            },
        );

        let tool = FileEditTool::new(state);
        let result = tool
            .invoke(
                json!({
                    "file_path": "f.txt",
                    "old_string": "dup",
                    "new_string": "one"
                }),
                context(dir.clone()),
            )
            .await;
        assert!(matches!(result, Err(ExecutionError::ToolExecution { .. })));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
