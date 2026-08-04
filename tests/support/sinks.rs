use async_tool_executor::{
    DiagnosticsSink, ExecutionEvent, ExecutionFailure, ExecutionObserver,
};
use std::sync::Mutex;

#[derive(Default)]
pub struct MemoryObserver(pub Mutex<Vec<ExecutionEvent>>);

impl ExecutionObserver for MemoryObserver {
    fn on_event(&self, event: ExecutionEvent) {
        self.0.lock().expect("observer lock poisoned").push(event);
    }
}

#[derive(Default)]
pub struct MemoryDiagnostics(pub Mutex<Vec<ExecutionFailure>>);

impl DiagnosticsSink for MemoryDiagnostics {
    fn record(&self, event: ExecutionFailure) {
        self.0
            .lock()
            .expect("diagnostics lock poisoned")
            .push(event);
    }
}
