use async_trait::async_trait;
use executor_protocol::{ExecutionRequest, ExecutionResult};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct WslClientConfig {
    pub distribution: String,
    pub guest_program: PathBuf,
    pub workspace: String,
}

impl WslClientConfig {
    pub fn new(
        distribution: impl Into<String>,
        guest_program: impl Into<PathBuf>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            distribution: distribution.into(),
            guest_program: guest_program.into(),
            workspace: workspace.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("this client is only available on Windows")]
    UnsupportedPlatform,
    #[error("failed to start WSL executor: {0}")]
    Spawn(String),
    #[error("invalid WSL workspace: {0}")]
    InvalidWorkspace(String),
    #[error("protocol handshake failed: {0}")]
    Handshake(String),
    #[error("transport disconnected")]
    Disconnected,
    #[error("execution id `{0}` is already pending")]
    DuplicateExecutionId(String),
    #[error("guest rejected the request ({code}): {message}")]
    Remote { code: String, message: String },
}

#[async_trait]
pub trait ExecutionClient: Send + Sync {
    async fn execute(
        &self,
        request: ExecutionRequest,
        timeout: Option<Duration>,
    ) -> Result<ExecutionResult, ClientError>;

    /// Requests cancellation and waits for the guest to acknowledge it.
    /// Returns `true` when the target execution was active and cancelled,
    /// `false` when no such execution was running.
    async fn cancel(&self, execution_id: &str) -> Result<bool, ClientError>;
}
