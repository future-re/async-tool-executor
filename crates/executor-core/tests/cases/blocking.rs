use crate::support::tools::{BlockingProbeTool, CooperativeBlockingTool};
use crate::support::{config, request};
use executor_core::{ExecutionOptions, ExecutorConfig, ToolExecutor, ToolRegistry};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn run_blocking_does_not_block_the_async_runtime_and_maps_failures() {
    let timer_fired = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools.register(BlockingProbeTool {
        timer_fired: Arc::clone(&timer_fired),
    });
    let executor = ToolExecutor::new(tools, config(1));
    let timer = {
        let timer_fired = Arc::clone(&timer_fired);
        async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            timer_fired.store(true, Ordering::SeqCst);
        }
    };

    let (success, ()) = tokio::join!(
        executor.execute(request("blocking", "blocking"), ExecutionOptions::new()),
        timer,
    );
    assert_eq!(success.content, Value::Bool(true));

    let mut error_request = request("blocking-error", "blocking");
    error_request.arguments = serde_json::json!({"error": true});
    let error = executor
        .execute(error_request, ExecutionOptions::new())
        .await;
    assert!(error.is_error);
    assert!(
        error
            .content
            .to_string()
            .contains("blocking operation failed")
    );

    let mut panic_request = request("blocking-panic", "blocking");
    panic_request.arguments = serde_json::json!({"panic": true});
    let panic = executor
        .execute(panic_request, ExecutionOptions::new())
        .await;
    assert!(panic.is_error);
    assert!(panic.content.to_string().contains("blocking worker failed"));
}

#[tokio::test]
async fn blocking_work_receives_cooperative_cancellation_after_timeout() {
    let stopped = Arc::new(AtomicBool::new(false));
    let mut tools = ToolRegistry::new();
    tools.register(CooperativeBlockingTool {
        stopped: Arc::clone(&stopped),
    });
    let executor = ToolExecutor::new(
        tools,
        ExecutorConfig {
            tool_timeout: Some(Duration::from_millis(5)),
            ..config(1)
        },
    );

    let result = executor
        .execute(
            request("cooperative", "cooperative-blocking"),
            ExecutionOptions::new(),
        )
        .await;
    tokio::time::timeout(Duration::from_millis(100), async {
        while !stopped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("blocking work did not observe cancellation");

    assert!(result.is_error);
    assert!(result.content.to_string().contains("timed_out"));
}
