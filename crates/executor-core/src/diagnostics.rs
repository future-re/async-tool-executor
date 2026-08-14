use crate::{ExecutionRequest, ToolContext};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    Validation,
    Invocation,
    Panic,
    NotFound,
    TimedOut,
    Cancelled,
}

/// Sanitized execution failure information.
#[derive(Debug, Clone)]
pub struct ExecutionFailure {
    pub execution_id: String,
    pub tool: String,
    pub kind: FailureKind,
    pub argument_keys: Vec<String>,
    pub error: String,
    pub cwd: PathBuf,
    pub env_keys: Vec<String>,
}

/// Diagnostics should never affect execution flow.
pub trait DiagnosticsSink: Send + Sync + 'static {
    fn record(&self, failure: ExecutionFailure);
}

/// Prevent user diagnostics from crashing executor.
pub struct GuardedSink {
    inner: Arc<dyn DiagnosticsSink>,
}

impl GuardedSink {
    pub fn new(inner: Arc<dyn DiagnosticsSink>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

impl DiagnosticsSink for GuardedSink {
    fn record(&self, failure: ExecutionFailure) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.inner.record(failure);
        }));

        if result.is_err() {
            tracing::warn!("diagnostics sink panicked");
        }
    }
}

pub fn record_failure(
    sink: Option<&dyn DiagnosticsSink>,
    context: &ToolContext,
    request: &ExecutionRequest,
    kind: FailureKind,
    error: impl Into<String>,
) {
    let Some(sink) = sink else {
        return;
    };

    let argument_keys = request
        .arguments
        .as_object()
        .map(|args| args.keys().cloned().collect())
        .unwrap_or_default();

    let failure = ExecutionFailure {
        execution_id: request.id.clone(),
        tool: request.tool.clone(),
        kind,
        argument_keys,
        error: error.into(),
        cwd: context.cwd.clone(),
        env_keys: context.env.keys().cloned().collect(),
    };

    sink.record(failure);
}
