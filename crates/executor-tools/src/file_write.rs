//! `Write` tool — overwrite a UTF-8 text file inside the workspace.

use async_trait::async_trait;
use executor_core::{ExecutionError, Tool, ToolContext, ToolDefinition, ToolOutput};
use serde_json::{Value, json};

use crate::shared::{
    FileReadRecord, FileReadState, canonicalize_within_cwd, ensure_file_was_read_and_unchanged,
    modified_timestamp_ms, required_string, required_string_any, resolve_workspace_path,
};

/// Built-in file-write tool. Writes (and creates) text files inside the workspace.
pub struct FileWriteTool {
    read_file_state: FileReadState,
}

impl FileWriteTool {
    pub fn new(read_file_state: FileReadState) -> Self {
        Self { read_file_state }
    }
}

const WRITE_CONTENT_PREVIEW_CHARS: usize = 20_000;

#[async_trait]
impl Tool for FileWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "Write".into(),
            description: "Create or overwrite a UTF-8 text file. Existing files must be read first with Read."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["file_path", "content"]
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
        required_string(arguments, "content")?;
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
        let content = required_string(&arguments, "content")?;
        if let Ok(existing_content) = tokio::fs::read_to_string(&path).await {
            ensure_file_was_read_and_unchanged(
                "Write",
                &self.read_file_state,
                &path,
                &existing_content,
            )
            .await?;
        }
        // Create any missing parent directories so the model can write nested paths in one call.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                ExecutionError::ToolExecution {
                    tool: "Write".into(),
                    message: err.to_string(),
                }
            })?;
        }
        tokio::fs::write(&path, content)
            .await
            .map_err(|err| ExecutionError::ToolExecution {
                tool: "Write".into(),
                message: err.to_string(),
            })?;
        let timestamp_ms = modified_timestamp_ms(&path).await?;
        self.read_file_state
            .lock()
            .expect("read-file-state lock poisoned")
            .insert(
                path.clone(),
                FileReadRecord {
                    content: content.to_string(),
                    timestamp_ms,
                    is_partial_view: false,
                    offset: None,
                    limit: None,
                },
            );
        let (content_preview, content_truncated) = content_preview(content);
        Ok(ToolOutput::json(json!({
            "file_path": input_path,
            "path": path,
            "written": true,
            "bytes": content.len(),
            "content_preview": content_preview,
            "content_truncated": content_truncated,
        })))
    }
}

fn content_preview(content: &str) -> (String, bool) {
    if content.chars().count() <= WRITE_CONTENT_PREVIEW_CHARS {
        return (content.to_string(), false);
    }

    (
        content.chars().take(WRITE_CONTENT_PREVIEW_CHARS).collect(),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::context;
    use serde_json::json;

    #[tokio::test]
    async fn rejects_write_without_prior_read() {
        let dir = std::env::temp_dir().join("ate_write_no_read_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "original").unwrap();

        let tool = FileWriteTool::new(FileReadState::default());
        let result = tool
            .invoke(
                json!({ "file_path": "f.txt", "content": "new" }),
                context(dir.clone()),
            )
            .await;
        assert!(
            matches!(result, Err(ExecutionError::ToolExecution { .. })),
            "write without a prior Read must be rejected"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn writes_new_file_creating_parent_directories() {
        let dir = std::env::temp_dir().join("ate_write_nested_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let tool = FileWriteTool::new(FileReadState::default());
        let output = tool
            .invoke(
                json!({ "file_path": "a/b/c.txt", "content": "hello" }),
                context(dir.clone()),
            )
            .await
            .unwrap();
        assert_eq!(output.content["written"], true);
        assert_eq!(
            std::fs::read_to_string(dir.join("a/b/c.txt")).unwrap(),
            "hello"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
