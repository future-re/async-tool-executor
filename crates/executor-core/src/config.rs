use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Process-wide defaults shared by all executions.
#[derive(Clone)]
pub struct ExecutorConfig {
    pub cwd: PathBuf,
    pub env: Arc<HashMap<String, String>>,
    pub tool_timeout: Option<Duration>,
    pub concurrency_limit: usize,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            env: Arc::new(HashMap::new()),
            tool_timeout: None,
            concurrency_limit: 1,
        }
    }
}

/// Per-submission controls. Requests passed to the executor are already authorized.
#[derive(Clone)]
pub struct SubmissionControls {
    deadline: Option<Instant>,
    cancellation: CancellationToken,
}

impl SubmissionControls {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.deadline = Some(Instant::now() + timeout);
        self
    }

    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(crate) fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

impl Default for SubmissionControls {
    fn default() -> Self {
        Self {
            deadline: None,
            cancellation: CancellationToken::new(),
        }
    }
}
