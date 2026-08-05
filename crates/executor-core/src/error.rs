/// Errors returned by tools and executor extension points.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("tool `{tool}` execution failed: {message}")]
    ToolExecution { tool: String, message: String },
    #[error("tool `{tool}` validation failed: {message}")]
    ToolValidation { tool: String, message: String },
    #[error("tool `{0}` not found")]
    ToolNotFound(String),
    #[error("blocking worker failed: {0}")]
    BlockingWorker(String),
    #[error("{0}")]
    Other(String),
}
