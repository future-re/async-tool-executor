use crate::diagnostics::{DiagnosticsSink, FailureKind, record_failure};
use crate::registry::RegisteredTool;
use crate::{ExecutionRequest, ExecutionResult, ExecutorConfig, ToolContext};
use serde_json::json;
use tokio::time::sleep_until;

pub(crate) async fn invoke(
    request: &ExecutionRequest,
    tool: &RegisteredTool,
    context: ToolContext,
    config: &ExecutorConfig,
) -> ExecutionResult {
    let operation = invoke_inner(request, tool, &context, config);
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
            let message = "execution deadline exceeded";
            context.cancellation.cancel();
            fail_with(
                config.diagnostics.as_deref(),
                request,
                &context,
                FailureKind::TimedOut,
                "timed_out",
                message,
            )
        }
        Outcome::Cancelled => {
            let message = "execution cancelled";
            context.cancellation.cancel();
            fail_with(
                config.diagnostics.as_deref(),
                request,
                &context,
                FailureKind::Cancelled,
                "cancelled",
                message,
            )
        }
    }
}

async fn invoke_inner(
    request: &ExecutionRequest,
    tool: &RegisteredTool,
    context: &ToolContext,
    config: &ExecutorConfig,
) -> ExecutionResult {
    if let Err(error) = tool.validate_arguments(&request.arguments) {
        return fail_with(
            config.diagnostics.as_deref(),
            request,
            context,
            FailureKind::Validation,
            "validation_error",
            error.to_string(),
        );
    }

    if let Err(error) = tool.implementation.validate(&request.arguments, context).await {
        return fail_with(
            config.diagnostics.as_deref(),
            request,
            context,
            FailureKind::Validation,
            "validation_error",
            error.to_string(),
        );
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
        Err(error) => fail_with(
            config.diagnostics.as_deref(),
            request,
            context,
            FailureKind::Invocation,
            "execution_error",
            error.to_string(),
        ),
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

/// Records the failure to the diagnostics sink (if any) and returns the
/// corresponding error result.
pub(crate) fn fail_with(
    sink: Option<&dyn DiagnosticsSink>,
    request: &ExecutionRequest,
    context: &ToolContext,
    kind: FailureKind,
    code: &str,
    message: impl Into<String>,
) -> ExecutionResult {
    let message = message.into();
    record_failure(sink, context, request, kind, &message);
    error_result(request, code, message)
}
