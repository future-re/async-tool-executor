use crate::support::tools::{CountingTool, HoldingTool, ProbeTool};
use crate::support::{config, request};
use executor_core::{ExecutionOptions, ExecutorConfig, ToolExecutor, ToolRegistry};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn queued_requests_honor_cancellation_and_deadline() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(HoldingTool {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    });
    tools.register(CountingTool {
        calls: Arc::clone(&calls),
    });
    let executor = ToolExecutor::new(tools, config(1));
    let holder = tokio::spawn({
        let executor = executor.clone();
        async move {
            executor
                .execute(request("holder", "hold"), ExecutionOptions::new())
                .await
        }
    });
    started.notified().await;

    let cancellation = CancellationToken::new();
    let cancel_signal = cancellation.clone();
    let cancel = async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        cancel_signal.cancel();
    };
    let cancelled = executor.execute(
        request("cancelled-in-queue", "count"),
        ExecutionOptions::new().with_cancellation(cancellation),
    );
    let timed_out = executor.execute(
        request("timed-out-in-queue", "count"),
        ExecutionOptions::new()
            .with_deadline(tokio::time::Instant::now() + Duration::from_millis(10)),
    );
    let (cancelled, timed_out, ()) = tokio::join!(cancelled, timed_out, cancel);

    assert!(cancelled.content.to_string().contains("cancelled"));
    assert!(timed_out.content.to_string().contains("timed_out"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    release.notify_one();
    assert!(!holder.await.expect("holder task failed").is_error);
}

#[tokio::test]
async fn tool_timeout_starts_after_the_global_permit_is_acquired() {
    let mut tools = ToolRegistry::new();
    tools.register(ProbeTool {
        current: Arc::new(AtomicUsize::new(0)),
        max: Arc::new(AtomicUsize::new(0)),
        delay: Duration::from_millis(40),
    });
    let executor = ToolExecutor::new(
        tools,
        ExecutorConfig {
            tool_timeout: Some(Duration::from_millis(65)),
            ..config(1)
        },
    );

    let (first, second) = tokio::join!(
        executor.execute(request("first", "probe"), ExecutionOptions::new()),
        executor.execute(request("second", "probe"), ExecutionOptions::new()),
    );

    assert!(!first.is_error);
    assert!(!second.is_error);
}

#[tokio::test]
async fn timeout_and_cancellation_are_terminal_results() {
    let mut tools = ToolRegistry::new();
    tools.register(ProbeTool {
        current: Arc::new(AtomicUsize::new(0)),
        max: Arc::new(AtomicUsize::new(0)),
        delay: Duration::from_millis(100),
    });
    let executor = ToolExecutor::new(
        tools,
        ExecutorConfig {
            tool_timeout: Some(Duration::from_millis(5)),
            ..config(1)
        },
    );
    let timeout = executor
        .execute(request("timeout", "probe"), ExecutionOptions::new())
        .await;
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let cancelled = executor
        .execute(
            request("cancelled", "probe"),
            ExecutionOptions::new().with_cancellation(cancellation),
        )
        .await;

    assert!(timeout.is_error);
    assert!(timeout.content.to_string().contains("timed_out"));
    assert!(cancelled.is_error);
    assert!(cancelled.content.to_string().contains("cancelled"));
}
