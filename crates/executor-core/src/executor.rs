use crate::diagnostics::GuardedSink;
use crate::observer::ExecutionObserver;
use crate::scheduler::{self, ExecutionRuntime};
use crate::{ExecutionRequest, ExecutionResult, ExecutorConfig, ToolRegistry};
use std::sync::Arc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Per-submission controls. Requests passed to the executor are already authorized.
#[derive(Clone)]
pub struct ExecutionOptions {
    pub deadline: Option<Instant>,
    pub cancellation: CancellationToken,
}

impl ExecutionOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

impl Default for ExecutionOptions {
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
    pub fn new(tools: ToolRegistry, mut config: ExecutorConfig) -> Self {
        config.diagnostics = config
            .diagnostics
            .map(|sink| Arc::new(GuardedSink::new(sink)) as Arc<dyn crate::DiagnosticsSink>);
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
        options: ExecutionOptions,
    ) -> ExecutionResult {
        scheduler::execute_one(request, options, self.runtime.clone()).await
    }

    pub async fn execute_all(
        &self,
        requests: Vec<ExecutionRequest>,
        options: ExecutionOptions,
    ) -> Vec<ExecutionResult> {
        scheduler::execute_all(requests, options, self.runtime.clone()).await
    }
}
