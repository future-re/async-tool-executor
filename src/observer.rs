use serde_json::Value;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// A best-effort execution lifecycle observation.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    Started {
        execution_id: String,
        tool: String,
        detail: String,
    },
    Progress {
        execution_id: String,
        tool: String,
        message: String,
        data: Option<Value>,
    },
}

/// Non-blocking observation hook.
///
/// Implementations should enqueue or record the event quickly. Slow work must
/// be moved to infrastructure owned by the observer.
pub trait ExecutionObserver: Send + Sync + 'static {
    fn on_event(&self, event: ExecutionEvent);
}

/// Progress capability supplied to a running tool.
#[derive(Clone, Default)]
pub struct ProgressReporter {
    observer: Option<Arc<dyn ExecutionObserver>>,
    execution_id: String,
    tool: String,
}

impl ProgressReporter {
    pub(crate) fn new(
        observer: Option<Arc<dyn ExecutionObserver>>,
        execution_id: String,
        tool: String,
    ) -> Self {
        Self {
            observer,
            execution_id,
            tool,
        }
    }

    pub fn emit(&self, message: impl Into<String>, data: Option<Value>) {
        notify(
            self.observer.as_deref(),
            ExecutionEvent::Progress {
                execution_id: self.execution_id.clone(),
                tool: self.tool.clone(),
                message: message.into(),
                data,
            },
        );
    }
}

pub(crate) fn notify(observer: Option<&dyn ExecutionObserver>, event: ExecutionEvent) {
    if let Some(observer) = observer
        && catch_unwind(AssertUnwindSafe(|| observer.on_event(event))).is_err()
    {
        tracing::warn!("execution observer panicked");
    }
}
