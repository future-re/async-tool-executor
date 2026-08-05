pub mod sinks;
pub mod tools;

use executor_core::{ExecutionRequest, ExecutorConfig};

pub fn request(id: impl Into<String>, tool: &str) -> ExecutionRequest {
    ExecutionRequest {
        id: id.into(),
        tool: tool.into(),
        arguments: serde_json::json!({}),
    }
}

pub fn config(limit: usize) -> ExecutorConfig {
    ExecutorConfig {
        concurrency_limit: limit,
        ..Default::default()
    }
}
