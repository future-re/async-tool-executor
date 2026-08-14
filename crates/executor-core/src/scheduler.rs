use crate::invocation::{fail_with, invoke};
use crate::observer::{ExecutionEvent, ExecutionObserver, ProgressReporter, notify};
use crate::{
    ExecutionError, ExecutionRequest, ExecutionResult, ExecutorConfig, SubmissionControls,
    ToolContext, ToolRegistry,
};
use futures_util::FutureExt;
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Capacity pair for the concurrency semaphore. The semaphore does not expose
/// its total capacity, so the count is kept alongside to guarantee that
/// exclusive tools can acquire the whole pool.
#[derive(Clone)]
struct PermitPool {
    permits: Arc<Semaphore>,
    count: u32,
}

impl PermitPool {
    fn new(concurrency_limit: usize) -> Self {
        let configured = concurrency_limit.clamp(1, Semaphore::MAX_PERMITS);
        let count = u32::try_from(configured).unwrap_or(u32::MAX);
        Self {
            permits: Arc::new(Semaphore::new(count as usize)),
            count,
        }
    }
}

/// Everything an execution needs besides the per-submission options. Assembled
/// once by the executor and shared across submissions.
#[derive(Clone)]
pub(crate) struct ExecutionRuntime {
    tools: Arc<ToolRegistry>,
    config: Arc<ExecutorConfig>,
    observer: Option<Arc<dyn ExecutionObserver>>,
    pool: PermitPool,
}

impl ExecutionRuntime {
    pub(crate) fn new(
        tools: Arc<ToolRegistry>,
        config: Arc<ExecutorConfig>,
        observer: Option<Arc<dyn ExecutionObserver>>,
    ) -> Self {
        let pool = PermitPool::new(config.concurrency_limit);
        Self {
            tools,
            config,
            observer,
            pool,
        }
    }

    pub(crate) fn with_observer(mut self, observer: Arc<dyn ExecutionObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    pub(crate) fn observer(&self) -> Option<&dyn ExecutionObserver> {
        self.observer.as_deref()
    }
}

/// Runs a batch of requests concurrently, bounded by the runtime's concurrency
/// limit. Results are returned in input order.
pub(crate) async fn execute(
    requests: Vec<ExecutionRequest>,
    options: SubmissionControls,
    runtime: ExecutionRuntime,
) -> Vec<ExecutionResult> {
    let mut active = requests
        .into_iter()
        .enumerate()
        .map(|(index, request)| run_one(request, index, &options, &runtime))
        .collect::<FuturesUnordered<_>>();
    let mut completed = Vec::with_capacity(active.len());
    while let Some((index, result)) = active.next().await {
        completed.push((index, result));
    }
    completed.sort_by_key(|(index, _)| *index);
    completed.into_iter().map(|(_, result)| result).collect()
}

/// Runs one request through its full lifecycle: queue for a permit, execute
/// with panic isolation, and return the result alongside its input index.
async fn run_one(
    request: ExecutionRequest,
    index: usize,
    options: &SubmissionControls,
    runtime: &ExecutionRuntime,
) -> (usize, ExecutionResult) {
    let mut context = tool_context(&request, &runtime.config, options, runtime.observer.clone());
    let exclusive = !is_concurrent(&request, &runtime.tools);

    let permit = match acquire_permit(
        runtime.pool.clone(),
        exclusive,
        options.deadline(),
        &context.cancellation,
    )
    .await
    {
        Ok(permit) => permit,
        Err(error) => {
            context.cancellation.cancel();
            return (
                index,
                fail_with(runtime.observer(), &request, &context, error),
            );
        }
    };

    context.deadline = effective_deadline(options.deadline(), runtime.config.tool_timeout);
    let panic_request = request.clone();
    let panic_context = context.clone();
    let outcome = AssertUnwindSafe(run_prepared(request, context, runtime.clone()))
        .catch_unwind()
        .await;

    let result = match outcome {
        Ok(result) => result,
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|value| value.to_string()))
                .unwrap_or_else(|| "tool execution panicked".to_string());
            tracing::warn!(
                tool = %panic_request.tool,
                execution_id = %panic_request.id,
                "tool execution panicked: {message}"
            );
            fail_with(
                runtime.observer(),
                &panic_request,
                &panic_context,
                ExecutionError::Panicked(message),
            )
        }
    };
    drop(permit);
    (index, result)
}

/// Waits for a permit, racing cancellation and the submission deadline.
async fn acquire_permit(
    pool: PermitPool,
    exclusive: bool,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<OwnedSemaphorePermit, ExecutionError> {
    let wait = async {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ExecutionError::Cancelled {
                message: "execution cancelled while queued".into(),
            }),
            permit = async {
                if exclusive {
                    pool.permits.acquire_many_owned(pool.count).await
                } else {
                    pool.permits.acquire_owned().await
                }
            } => permit.map_err(|_| ExecutionError::ExecutorClosed),
        }
    };

    match deadline {
        Some(deadline) => {
            tokio::time::timeout_at(deadline, wait)
                .await
                .map_err(|_| ExecutionError::TimedOut {
                    message: "execution deadline exceeded while queued".into(),
                })?
        }
        None => wait.await,
    }
}

/// Resolves the tool and invokes it, reporting the lifecycle start event.
async fn run_prepared(
    request: ExecutionRequest,
    context: ToolContext,
    runtime: ExecutionRuntime,
) -> ExecutionResult {
    let tool = match runtime.tools.resolve(&request.tool) {
        Ok(tool) => tool,
        Err(error) => {
            return fail_with(runtime.observer(), &request, &context, error);
        }
    };

    notify(
        runtime.observer.as_deref(),
        ExecutionEvent::Started {
            execution_id: request.id.clone(),
            tool: request.tool.clone(),
            detail: tool.implementation.invocation_detail(&request.arguments),
        },
    );
    invoke(&request, &tool, context, &runtime).await
}

fn tool_context(
    request: &ExecutionRequest,
    config: &ExecutorConfig,
    options: &SubmissionControls,
    observer: Option<Arc<dyn ExecutionObserver>>,
) -> ToolContext {
    ToolContext {
        execution_id: request.id.clone(),
        cwd: config.cwd.clone(),
        env: Arc::clone(&config.env),
        deadline: None,
        cancellation: options.cancellation().child_token(),
        progress: ProgressReporter::new(observer, request.id.clone(), request.tool.clone()),
    }
}

fn is_concurrent(request: &ExecutionRequest, tools: &ToolRegistry) -> bool {
    match tools.resolve(&request.tool) {
        Ok(tool) => catch_unwind(AssertUnwindSafe(|| {
            tool.implementation.is_concurrency_safe(&request.arguments)
        }))
        .unwrap_or(false),
        Err(_) => true,
    }
}

fn effective_deadline(
    request_deadline: Option<Instant>,
    tool_timeout: Option<std::time::Duration>,
) -> Option<Instant> {
    let tool_deadline = tool_timeout.map(|timeout| Instant::now() + timeout);
    match (request_deadline, tool_deadline) {
        (Some(request), Some(tool)) => Some(request.min(tool)),
        (Some(request), None) => Some(request),
        (None, tool) => tool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn effective_deadline_preserves_an_explicit_deadline_without_a_tool_timeout() {
        let request_deadline = Instant::now() + Duration::from_secs(1);

        assert_eq!(
            effective_deadline(Some(request_deadline), None),
            Some(request_deadline)
        );
    }

    #[test]
    fn effective_deadline_uses_the_earlier_request_deadline() {
        let request_deadline = Instant::now() + Duration::from_millis(10);

        assert_eq!(
            effective_deadline(Some(request_deadline), Some(Duration::from_secs(1))),
            Some(request_deadline)
        );
    }

    #[test]
    fn effective_deadline_is_absent_without_any_limit() {
        assert_eq!(effective_deadline(None, None), None);
    }
}
