use async_trait::async_trait;
use executor_protocol::{ExecutionRequest, ExecutionResult};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct WslClientConfig {
    pub distribution: String,
    pub guest_program: PathBuf,
}

impl WslClientConfig {
    pub fn new(distribution: impl Into<String>, guest_program: impl Into<PathBuf>) -> Self {
        Self {
            distribution: distribution.into(),
            guest_program: guest_program.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("this client is only available on Windows")]
    UnsupportedPlatform,
    #[error("failed to start WSL executor: {0}")]
    Spawn(String),
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

    async fn cancel(&self, execution_id: &str) -> Result<(), ClientError>;
}
