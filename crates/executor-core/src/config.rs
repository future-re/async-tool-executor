use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Process-wide defaults shared by all executions.
#[derive(Clone)]
pub struct ExecutorConfig {
    pub cwd: PathBuf,
    pub env: Arc<HashMap<String, String>>,
    pub tool_timeout: Option<Duration>,
    pub concurrency_limit: usize,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            env: Arc::new(HashMap::new()),
            tool_timeout: None,
            concurrency_limit: 1,
        }
    }
}
