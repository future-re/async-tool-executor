use crate::observer::{ExecutionEvent, notify};
use crate::registry::RegisteredTool;
use crate::scheduler::ExecutionRuntime;
use crate::{ExecutionError, ExecutionObserver, ExecutionRequest, ExecutionResult, ToolContext};
use serde_json::json;
use tokio::time::sleep_until;

pub(crate) async fn invoke(
    request: &ExecutionRequest,
    tool: &RegisteredTool,
    context: ToolContext,
    runtime: &ExecutionRuntime,
) -> ExecutionResult {
    let operation = invoke_inner(request, tool, &context, runtime);
    tokio::pin!(operation);

    enum Outcome<T> {
        Completed(T),
        TimedOut,
        Cancelled,
    }

    let outcome = if let Some(deadline) = context.deadline {
        tokio::select! {
            biased;
            _ = context.cancellation.cancelled() => Outcome::Cancelled,
            _ = sleep_until(deadline) => Outcome::TimedOut,
            result = &mut operation => Outcome::Completed(result),
        }
    } else {
        tokio::select! {
            biased;
            _ = context.cancellation.cancelled() => Outcome::Cancelled,
            result = &mut operation => Outcome::Completed(result),
        }
    };

    match outcome {
        Outcome::Completed(result) => result,
        Outcome::TimedOut => {
            context.cancellation.cancel();
            fail_with(
                runtime.observer(),
                request,
                &context,
                ExecutionError::TimedOut {
                    message: "execution deadline exceeded".into(),
                },
            )
        }
        Outcome::Cancelled => {
            context.cancellation.cancel();
            fail_with(
                runtime.observer(),
                request,
                &context,
                ExecutionError::Cancelled {
                    message: "execution cancelled".into(),
                },
            )
        }
    }
}

async fn invoke_inner(
    request: &ExecutionRequest,
    tool: &RegisteredTool,
    context: &ToolContext,
    runtime: &ExecutionRuntime,
) -> ExecutionResult {
    if let Err(error) = tool.validate_arguments(&request.arguments) {
        return fail_with(runtime.observer(), request, context, error);
    }

    if let Err(error) = tool
        .implementation
        .validate(&request.arguments, context)
        .await
    {
        return fail_with(runtime.observer(), request, context, error);
    }

    let span = tracing::info_span!(
        "tool_execution",
        tool = %request.tool,
        execution_id = %request.id
    );
    match tracing::Instrument::instrument(
        tool.implementation
            .invoke(request.arguments.clone(), context.clone()),
        span,
    )
    .await
    {
        Ok(output) => ExecutionResult {
            execution_id: request.id.clone(),
            tool: request.tool.clone(),
            content: output.content,
            is_error: false,
        },
        Err(error) => fail_with(runtime.observer(), request, context, error),
    }
}

pub(crate) fn error_result(
    request: &ExecutionRequest,
    kind: &str,
    message: impl Into<String>,
) -> ExecutionResult {
    ExecutionResult {
        execution_id: request.id.clone(),
        tool: request.tool.clone(),
        content: json!({
            "error": {
                "kind": kind,
                "message": message.into(),
            }
        }),
        is_error: true,
    }
}

/// Records the failure to the observer (if any) and returns the
/// corresponding error result.
pub(crate) fn fail_with(
    observer: Option<&dyn ExecutionObserver>,
    request: &ExecutionRequest,
    context: &ToolContext,
    error: ExecutionError,
) -> ExecutionResult {
    notify(
        observer,
        ExecutionEvent::Failed {
            execution_id: request.id.clone(),
            tool: request.tool.clone(),
            error: error.clone(),
            argument_keys: request
                .arguments
                .as_object()
                .map(|args| args.keys().cloned().collect())
                .unwrap_or_default(),
            cwd: context.cwd.clone(),
            env_keys: context.env.keys().cloned().collect(),
        },
    );
    error_result(request, error.code(), error.to_string())
}
