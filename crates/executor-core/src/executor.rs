use crate::observer::ExecutionObserver;
use crate::scheduler::{self, ExecutionRuntime};
use crate::{ExecutionRequest, ExecutionResult, ExecutorConfig, ToolRegistry};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

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

/// Clone-cheap async execution service.
#[derive(Clone)]
pub struct ToolExecutor {
    runtime: ExecutionRuntime,
}

impl ToolExecutor {
    pub fn new(tools: ToolRegistry, config: ExecutorConfig) -> Self {
        Self {
            runtime: ExecutionRuntime::new(Arc::new(tools), Arc::new(config), None),
        }
    }

    pub fn with_observer(mut self, observer: Arc<dyn ExecutionObserver>) -> Self {
        self.runtime = self.runtime.with_observer(observer);
        self
    }

    pub async fn execute(
        &self,
        request: ExecutionRequest,
        options: SubmissionControls,
    ) -> ExecutionResult {
        scheduler::execute_one(request, options, self.runtime.clone()).await
    }

    pub async fn execute_all(
        &self,
        requests: Vec<ExecutionRequest>,
        options: SubmissionControls,
    ) -> Vec<ExecutionResult> {
        scheduler::execute_all(requests, options, self.runtime.clone()).await
    }
}
