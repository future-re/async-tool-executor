use executor_core::{ExecutionEvent, ExecutionObserver};
use std::sync::Mutex;

#[derive(Default)]
pub struct MemoryObserver(pub Mutex<Vec<ExecutionEvent>>);

impl ExecutionObserver for MemoryObserver {
    fn on_event(&self, event: ExecutionEvent) {
        self.0.lock().expect("observer lock poisoned").push(event);
    }
}
