use executor_core::{ExecutorConfig, ToolExecutor, ToolRegistry};
use std::collections::HashMap;
use std::sync::Arc;
use wsl_executor_daemon::serve;
use wsl_runtime::{NativeShell, NativeShellConfig, ResourceLimits, ShellTool};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let base_env = HashMap::from([
        (
            "PATH".to_string(),
            "/usr/local/bin:/usr/bin:/bin".to_string(),
        ),
        ("LANG".to_string(), "C.UTF-8".to_string()),
    ]);
    let shell = NativeShell::new(NativeShellConfig {
        inherit_env: false,
        base_env,
        cwd: Some(cwd.clone()),
        limits: ResourceLimits::default(),
    });

    let mut registry = ToolRegistry::new();
    registry.register(ShellTool::new(shell));
    let tools = registry.list();
    let executor = ToolExecutor::new(
        registry,
        ExecutorConfig {
            cwd,
            env: Arc::new(HashMap::new()),
            tool_timeout: None,
            concurrency_limit: 4,
            diagnostics: None,
        },
    );

    serve(tokio::io::stdin(), tokio::io::stdout(), executor, tools).await?;
    Ok(())
}
