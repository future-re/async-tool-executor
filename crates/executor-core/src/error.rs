/// Errors returned by tools and executor extension points. The variant also
/// drives the failure classification exposed through `ExecutionEvent::Failed`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionError {
    #[error("tool `{tool}` execution failed: {message}")]
    ToolExecution { tool: String, message: String },
    #[error("tool `{tool}` validation failed: {message}")]
    ToolValidation { tool: String, message: String },
    #[error("tool `{0}` not found")]
    ToolNotFound(String),
    #[error("blocking worker failed: {0}")]
    BlockingWorker(String),
    #[error("{message}")]
    TimedOut { message: String },
    #[error("{message}")]
    Cancelled { message: String },
    #[error("tool panicked: {0}")]
    Panicked(String),
    #[error("{0}")]
    Other(String),
}

impl ExecutionError {
    /// Stable machine-readable code used across the process boundary.
    pub fn code(&self) -> &'static str {
        match self {
            ExecutionError::ToolExecution { .. } | ExecutionError::BlockingWorker(_) => {
                "execution_error"
            }
            ExecutionError::ToolValidation { .. } => "validation_error",
            ExecutionError::ToolNotFound(_) => "tool_not_found",
            ExecutionError::TimedOut { .. } => "timed_out",
            ExecutionError::Cancelled { .. } => "cancelled",
            ExecutionError::Panicked(_) => "execution_panic",
            ExecutionError::Other(_) => "execution_error",
        }
    }
}
