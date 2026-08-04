use crate::support::sinks::{MemoryDiagnostics, MemoryObserver};
use crate::support::tools::{FailingTool, ProgressTool};
use crate::support::{config, request};
use async_tool_executor::{
    ExecutionEvent, ExecutionOptions, ExecutorConfig, FailureKind, ToolExecutor, ToolRegistry,
};
use std::sync::Arc;

#[tokio::test]
async fn progress_uses_the_optional_observer() {
    let mut tools = ToolRegistry::new();
    tools.register(ProgressTool);
    let observer = Arc::new(MemoryObserver::default());
    let executor = ToolExecutor::new(tools, config(1)).with_observer(observer.clone());

    let result = executor
        .execute(request("c1", "progress"), ExecutionOptions::new())
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
async fn diagnostics_are_redacted() {
    let diagnostics = Arc::new(MemoryDiagnostics::default());
    let mut tools = ToolRegistry::new();
    tools.register(FailingTool);
    let executor = ToolExecutor::new(
        tools,
        ExecutorConfig {
            diagnostics: Some(diagnostics.clone()),
            ..config(1)
        },
    );
    let mut failing = request("failure", "fail");
    failing.arguments = serde_json::json!({"secret": "do-not-record"});

    let result = executor.execute(failing, ExecutionOptions::new()).await;

    assert!(result.is_error);
    let events = diagnostics.0.lock().expect("diagnostics lock poisoned");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, FailureKind::Invocation);
    assert_eq!(events[0].argument_keys, vec!["secret"]);
    assert!(!format!("{:?}", events[0]).contains("do-not-record"));
}
