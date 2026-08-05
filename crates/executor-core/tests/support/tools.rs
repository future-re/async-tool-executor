use async_trait::async_trait;
use executor_core::{ExecutionError, Tool, ToolContext, ToolDefinition, ToolOutput};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

pub struct ProbeTool {
    pub current: Arc<AtomicUsize>,
    pub max: Arc<AtomicUsize>,
    pub delay: Duration,
}

#[async_trait]
impl Tool for ProbeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "probe".into(),
            description: "concurrency probe".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn is_concurrency_safe(&self, _arguments: &Value) -> bool {
        true
    }

    async fn invoke(
        &self,
        _arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        let previous = self.current.fetch_add(1, Ordering::SeqCst);
        self.max.fetch_max(previous + 1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::text("ok"))
    }
}

pub struct ExclusiveProbeTool {
    pub current: Arc<AtomicUsize>,
    pub max: Arc<AtomicUsize>,
    pub delay: Duration,
}

#[async_trait]
impl Tool for ExclusiveProbeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exclusive".into(),
            description: "exclusive probe".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    async fn invoke(
        &self,
        _arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        let previous = self.current.fetch_add(1, Ordering::SeqCst);
        self.max.fetch_max(previous + 1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::text("ok"))
    }
}

pub struct HoldingTool {
    pub started: Arc<Notify>,
    pub release: Arc<Notify>,
}

#[async_trait]
impl Tool for HoldingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "hold".into(),
            description: "holds one permit".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn is_concurrency_safe(&self, _arguments: &Value) -> bool {
        true
    }

    async fn invoke(
        &self,
        _arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(ToolOutput::text("released"))
    }
}

pub struct CountingTool {
    pub calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "count".into(),
            description: "counts invocations".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn is_concurrency_safe(&self, _arguments: &Value) -> bool {
        true
    }

    async fn invoke(
        &self,
        _arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("counted"))
    }
}

pub struct BlockingProbeTool {
    pub timer_fired: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for BlockingProbeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "blocking".into(),
            description: "blocking helper probe".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    async fn invoke(
        &self,
        arguments: Value,
        context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        let timer_fired = Arc::clone(&self.timer_fired);
        let should_panic = arguments
            .get("panic")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let should_error = arguments
            .get("error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let timer_was_ready = context
            .run_blocking(move |_| {
                std::thread::sleep(Duration::from_millis(30));
                assert!(!should_panic, "blocking worker panic");
                if should_error {
                    return Err(ExecutionError::Other("blocking operation failed".into()));
                }
                Ok(timer_fired.load(Ordering::SeqCst))
            })
            .await?;
        Ok(ToolOutput::json(Value::Bool(timer_was_ready)))
    }
}

pub struct CooperativeBlockingTool {
    pub stopped: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for CooperativeBlockingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "cooperative-blocking".into(),
            description: "stops when its token is cancelled".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    async fn invoke(
        &self,
        _arguments: Value,
        context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        let stopped = Arc::clone(&self.stopped);
        context
            .run_blocking(move |cancellation| {
                while !cancellation.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                stopped.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await?;
        Ok(ToolOutput::text("stopped"))
    }
}

pub struct ProgressTool;

#[async_trait]
impl Tool for ProgressTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "progress".into(),
            description: "emits progress".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    async fn invoke(
        &self,
        _arguments: Value,
        context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        context.emit_progress("halfway", Some(serde_json::json!({"pct": 50})));
        Ok(ToolOutput::text("done"))
    }
}

pub struct PanickingTool;

#[async_trait]
impl Tool for PanickingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "panic".into(),
            description: "panics".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn is_concurrency_safe(&self, _arguments: &Value) -> bool {
        true
    }

    async fn invoke(
        &self,
        _arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        panic!("intentional panic");
    }
}

pub struct FailingTool;

#[async_trait]
impl Tool for FailingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fail".into(),
            description: "fails".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    async fn invoke(
        &self,
        _arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        Err(ExecutionError::ToolExecution {
            tool: "fail".into(),
            message: "simulated failure".into(),
        })
    }
}
