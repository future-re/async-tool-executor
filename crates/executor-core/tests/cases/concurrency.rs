use crate::support::tools::{CountingTool, ExclusiveProbeTool, HoldingTool, ProbeTool};
use crate::support::{config, request};
use executor_core::{SubmissionControls, ToolExecutor, ToolRegistry};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

#[tokio::test]
async fn concurrent_tools_respect_the_limit_and_result_order() {
    let current = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(ProbeTool {
        current,
        max: Arc::clone(&max),
        delay: Duration::from_millis(20),
    });
    let executor = ToolExecutor::new(tools, config(3));
    let requests = (0..7)
        .map(|index| request(format!("call-{index}"), "probe"))
        .collect();

    let results = executor
        .execute_all(requests, SubmissionControls::new())
        .await;

    assert_eq!(results.len(), 7);
    assert_eq!(max.load(Ordering::SeqCst), 3);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result.execution_id, format!("call-{index}"));
        assert!(!result.is_error);
    }
}

#[tokio::test]
async fn concurrent_submissions_share_the_executor_limit() {
    let current = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(ProbeTool {
        current,
        max: Arc::clone(&max),
        delay: Duration::from_millis(15),
    });
    let executor = ToolExecutor::new(tools, config(2));
    let left = (0..4)
        .map(|index| request(format!("left-{index}"), "probe"))
        .collect();
    let right = (0..4)
        .map(|index| request(format!("right-{index}"), "probe"))
        .collect();

    let (left, right) = tokio::join!(
        executor.execute_all(left, SubmissionControls::new()),
        executor.execute_all(right, SubmissionControls::new()),
    );

    assert_eq!(left.len() + right.len(), 8);
    assert_eq!(max.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unsafe_tool_is_exclusive_across_submissions() {
    let current = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(ProbeTool {
        current: Arc::clone(&current),
        max: Arc::clone(&max),
        delay: Duration::from_millis(15),
    });
    tools.register(ExclusiveProbeTool {
        current,
        max: Arc::clone(&max),
        delay: Duration::from_millis(15),
    });
    let executor = ToolExecutor::new(tools, config(3));

    let (parallel, exclusive) = tokio::join!(
        executor.execute(request("parallel", "probe"), SubmissionControls::new()),
        executor.execute(request("exclusive", "exclusive"), SubmissionControls::new(),),
    );

    assert!(!parallel.is_error);
    assert!(!exclusive.is_error);
    assert_eq!(max.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn queued_exclusive_tool_prevents_later_parallel_work_from_bypassing_it() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let late_calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(HoldingTool {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    });
    tools.register(ExclusiveProbeTool {
        current: Arc::new(AtomicUsize::new(0)),
        max: Arc::new(AtomicUsize::new(0)),
        delay: Duration::from_millis(5),
    });
    tools.register(CountingTool {
        calls: Arc::clone(&late_calls),
    });
    let executor = ToolExecutor::new(tools, config(2));

    let holder = tokio::spawn({
        let executor = executor.clone();
        async move {
            executor
                .execute(request("holder", "hold"), SubmissionControls::new())
                .await
        }
    });
    started.notified().await;
    let exclusive = tokio::spawn({
        let executor = executor.clone();
        async move {
            executor
                .execute(request("exclusive", "exclusive"), SubmissionControls::new())
                .await
        }
    });
    tokio::task::yield_now().await;
    let later = tokio::spawn({
        let executor = executor.clone();
        async move {
            executor
                .execute(request("later", "count"), SubmissionControls::new())
                .await
        }
    });

    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(late_calls.load(Ordering::SeqCst), 0);
    release.notify_one();

    assert!(!holder.await.expect("holder task failed").is_error);
    assert!(!exclusive.await.expect("exclusive task failed").is_error);
    assert!(!later.await.expect("later task failed").is_error);
    assert_eq!(late_calls.load(Ordering::SeqCst), 1);
}
