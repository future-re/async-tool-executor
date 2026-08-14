use crate::support::sinks::MemoryObserver;
use crate::support::tools::{FailingTool, ProgressTool};
use crate::support::{config, request};
use executor_core::{
    ExecutionError, ExecutionEvent, SubmissionControls, ToolExecutor, ToolRegistry,
};
use std::sync::Arc;

#[tokio::test]
async fn progress_uses_the_optional_observer() {
    let mut tools = ToolRegistry::new();
    tools.register(ProgressTool);
    let observer = Arc::new(MemoryObserver::default());
    let executor = ToolExecutor::new(tools, config(1)).with_observer(observer.clone());

    let result = executor
        .execute(request("c1", "progress"), SubmissionControls::new())
        .await;

    assert!(!result.is_error);
    let events = observer.0.lock().expect("observer lock poisoned");
    assert!(events.iter().any(|event| matches!(
        event,
        ExecutionEvent::Progress { tool, message, .. }
            if tool == "progress" && message == "halfway"
    )));
}

#[tokio::test]
async fn failures_are_recorded_without_argument_values() {
    let observer = Arc::new(MemoryObserver::default());
    let mut tools = ToolRegistry::new();
    tools.register(FailingTool);
    let executor = ToolExecutor::new(tools, config(1)).with_observer(observer.clone());
    let mut failing = request("failure", "fail");
    failing.arguments = serde_json::json!({"secret": "do-not-record"});

    let result = executor.execute(failing, SubmissionControls::new()).await;

    assert!(result.is_error);
    let events = observer.0.lock().expect("observer lock poisoned");
    let failed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ExecutionEvent::Failed {
                error,
                argument_keys,
                ..
            } => Some((error, argument_keys)),
            _ => None,
        })
        .collect();
    assert_eq!(failed.len(), 1);
    assert!(matches!(failed[0].0, ExecutionError::ToolExecution { .. }));
    assert_eq!(failed[0].1, &vec!["secret".to_string()]);
    assert!(!format!("{:?}", events).contains("do-not-record"));
}
