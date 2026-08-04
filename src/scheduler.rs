use crate::diagnostics::FailureKind;
use crate::invocation::{error_result, fail_with, invoke};
use crate::observer::{ExecutionEvent, ExecutionObserver, ProgressReporter, notify};
use crate::{
    ExecutionOptions, ExecutionRequest, ExecutionResult, ExecutorConfig, ToolContext, ToolRegistry,
};
use futures_util::FutureExt;
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

struct PreparedExecution {
    index: usize,
    request: ExecutionRequest,
}

struct IndexedResult {
    index: usize,
    result: ExecutionResult,
}

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
}

pub(crate) async fn execute_one(
    request: ExecutionRequest,
    options: ExecutionOptions,
    runtime: ExecutionRuntime,
) -> ExecutionResult {
    let concurrent = is_concurrent(&request, &runtime.tools);
    run_isolated(
        PreparedExecution { index: 0, request },
        runtime,
        options,
        !concurrent,
    )
    .await
    .result
}

pub(crate) async fn execute_all(
    requests: Vec<ExecutionRequest>,
    options: ExecutionOptions,
    runtime: ExecutionRuntime,
) -> Vec<ExecutionResult> {
    let mut active = requests
        .into_iter()
        .enumerate()
        .map(|(index, request)| {
            let exclusive = !is_concurrent(&request, &runtime.tools);
            run_isolated(
                PreparedExecution { index, request },
                runtime.clone(),
                options.clone(),
                exclusive,
            )
        })
        .collect::<FuturesUnordered<_>>();
    let mut completed = Vec::with_capacity(active.len());
    while let Some(result) = active.next().await {
        completed.push(result);
    }
    completed.sort_by_key(|item| item.index);
    completed.into_iter().map(|item| item.result).collect()
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

async fn run_isolated(
    prepared: PreparedExecution,
    runtime: ExecutionRuntime,
    options: ExecutionOptions,
    exclusive: bool,
) -> IndexedResult {
    let index = prepared.index;
    let request = prepared.request;
    let mut context = tool_context(
        &request,
        &runtime.config,
        &options,
        runtime.observer.clone(),
    );
    let permit = match acquire_permit(runtime.pool.clone(), exclusive, &context).await {
        Ok(permit) => permit,
        Err(QueueExit::TimedOut) => {
            let message = "execution deadline exceeded while queued";
            context.cancellation.cancel();
            return IndexedResult {
                index,
                result: fail_with(
                    runtime.config.diagnostics.as_deref(),
                    &request,
                    &context,
                    FailureKind::TimedOut,
                    "timed_out",
                    message,
                ),
            };
        }
        Err(QueueExit::Cancelled) => {
            let message = "execution cancelled while queued";
            context.cancellation.cancel();
            return IndexedResult {
                index,
                result: fail_with(
                    runtime.config.diagnostics.as_deref(),
                    &request,
                    &context,
                    FailureKind::Cancelled,
                    "cancelled",
                    message,
                ),
            };
        }
        Err(QueueExit::Closed) => {
            return IndexedResult {
                index,
                result: error_result(&request, "executor_closed", "executor semaphore closed"),
            };
        }
    };
    context.deadline = effective_deadline(options.deadline, runtime.config.tool_timeout);
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
                runtime.config.diagnostics.as_deref(),
                &panic_request,
                &panic_context,
                FailureKind::Panic,
                "execution_panic",
                message,
            )
        }
    };
    drop(permit);
    IndexedResult { index, result }
}

enum QueueExit {
    TimedOut,
    Cancelled,
    Closed,
}

async fn acquire_permit(
    pool: PermitPool,
    exclusive: bool,
    context: &ToolContext,
) -> Result<OwnedSemaphorePermit, QueueExit> {
    let acquisition = async move {
        if exclusive {
            pool.permits.acquire_many_owned(pool.count).await
        } else {
            pool.permits.acquire_owned().await
        }
    };
    tokio::pin!(acquisition);

    let permit = if let Some(deadline) = context.deadline {
        tokio::select! {
            biased;
            _ = context.cancellation.cancelled() => return Err(QueueExit::Cancelled),
            _ = tokio::time::sleep_until(deadline) => return Err(QueueExit::TimedOut),
            permit = &mut acquisition => permit,
        }
    } else {
        tokio::select! {
            biased;
            _ = context.cancellation.cancelled() => return Err(QueueExit::Cancelled),
            permit = &mut acquisition => permit,
        }
    };
    permit.map_err(|_| QueueExit::Closed)
}

async fn run_prepared(
    request: ExecutionRequest,
    context: ToolContext,
    runtime: ExecutionRuntime,
) -> ExecutionResult {
    let tool = match runtime.tools.resolve(&request.tool) {
        Ok(tool) => tool,
        Err(error) => {
            return fail_with(
                runtime.config.diagnostics.as_deref(),
                &request,
                &context,
                FailureKind::NotFound,
                "tool_not_found",
                error.to_string(),
            );
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
    invoke(&request, &tool, context, &runtime.config).await
}

fn tool_context(
    request: &ExecutionRequest,
    config: &ExecutorConfig,
    options: &ExecutionOptions,
    observer: Option<Arc<dyn ExecutionObserver>>,
) -> ToolContext {
    ToolContext {
        execution_id: request.id.clone(),
        cwd: config.cwd.clone(),
        env: Arc::clone(&config.env),
        deadline: options.deadline,
        cancellation: options.cancellation.child_token(),
        progress: ProgressReporter::new(observer, request.id.clone(), request.tool.clone()),
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
