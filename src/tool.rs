use crate::{ExecutionError, ProgressReporter};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Immutable execution capabilities and environment passed to a tool.
#[derive(Clone)]
pub struct ToolContext {
    pub execution_id: String,
    pub cwd: PathBuf,
    pub env: Arc<HashMap<String, String>>,
    pub deadline: Option<Instant>,
    pub cancellation: CancellationToken,
    pub progress: ProgressReporter,
}

impl ToolContext {
    pub fn emit_progress(&self, message: impl Into<String>, data: Option<Value>) {
        self.progress.emit(message, data);
    }

    /// Run synchronous work without blocking the async runtime.
    ///
    /// The operation receives the execution cancellation token and should check
    /// it periodically when performing long-running work. Timing out or dropping
    /// this future cannot forcibly stop the underlying operating-system thread.
    pub async fn run_blocking<F, T>(&self, operation: F) -> Result<T, ExecutionError>
    where
        F: FnOnce(CancellationToken) -> Result<T, ExecutionError> + Send + 'static,
        T: Send + 'static,
    {
        let cancellation = self.cancellation.clone();
        tokio::task::spawn_blocking(move || operation(cancellation))
            .await
            .map_err(|error| ExecutionError::BlockingWorker(error.to_string()))?
    }
}

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: Value,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: Value::String(text.into()),
        }
    }

    pub fn json(content: Value) -> Self {
        Self { content }
    }
}

#[async_trait]
pub trait Tool: Send + Sync + 'static {
    fn definition(&self) -> ToolDefinition;

    fn is_concurrency_safe(&self, _arguments: &Value) -> bool {
        false
    }

    fn invocation_detail(&self, _arguments: &Value) -> String {
        String::new()
    }

    async fn validate(
        &self,
        _arguments: &Value,
        _context: &ToolContext,
    ) -> Result<(), ExecutionError> {
        Ok(())
    }

    async fn invoke(
        &self,
        arguments: Value,
        context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError>;
}
