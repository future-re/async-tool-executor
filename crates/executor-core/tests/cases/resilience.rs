use crate::support::tools::{PanickingTool, ProbeTool};
use crate::support::{config, request};
use executor_core::{ExecutionOptions, ToolExecutor, ToolRegistry};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

#[tokio::test]
async fn panic_is_isolated_for_single_and_batch_execution() {
    let mut tools = ToolRegistry::new();
    tools.register(PanickingTool);
    tools.register(ProbeTool {
        current: Arc::new(AtomicUsize::new(0)),
        max: Arc::new(AtomicUsize::new(0)),
        delay: Duration::from_millis(1),
    });
    let executor = ToolExecutor::new(tools, config(2));

    let single = executor
        .execute(request("single", "panic"), ExecutionOptions::new())
        .await;
    let batch = executor
        .execute_all(
            vec![request("p", "panic"), request("ok", "probe")],
            ExecutionOptions::new(),
        )
        .await;

    assert!(single.is_error);
    assert!(single.content.to_string().contains("panic"));
    assert!(batch[0].is_error);
    assert!(!batch[1].is_error);
}
