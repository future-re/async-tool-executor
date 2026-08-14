use crate::observer::ExecutionObserver;
use crate::scheduler::{self, ExecutionRuntime};
use crate::{ExecutionRequest, ExecutionResult, ExecutorConfig, SubmissionControls, ToolRegistry};
use std::sync::Arc;

/// Clone-cheap async execution service.
///
/// This is the primary entry point for running tools. Build it with
/// [`ToolExecutor::new`], attach an optional observer, then submit requests
/// through [`execute`](ToolExecutor::execute) (single, in order) or
/// [`execute_all`](ToolExecutor::execute_all) (batch, results kept in input
/// order).
///
/// The executor is cheap to clone: all submissions share one runtime
/// (tool registry, config, concurrency pool and observer).
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

    /// Runs a single request and returns its result.
    ///
    /// A single request has no peers, so this is trivially ordered: the
    /// request executes and its one result is returned directly.
    pub async fn execute(
        &self,
        request: ExecutionRequest,
        options: SubmissionControls,
    ) -> ExecutionResult {
        scheduler::execute(vec![request], options, self.runtime.clone())
            .await
            .into_iter()
            .next()
            .expect("one request yields one result")
    }

    /// Runs a batch of requests, preserving input order in the results.
    ///
    /// The requests run concurrently, bounded by
    /// `ExecutorConfig::concurrency_limit`; how many run in parallel is
    /// decided by how many are submitted together. Results are returned in
    /// the same order as the input.
    pub async fn execute_all(
        &self,
        requests: Vec<ExecutionRequest>,
        options: SubmissionControls,
    ) -> Vec<ExecutionResult> {
        scheduler::execute(requests, options, self.runtime.clone()).await
    }
}
