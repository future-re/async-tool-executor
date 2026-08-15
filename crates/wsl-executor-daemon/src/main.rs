#[cfg(unix)]
use executor_core::{ExecutorConfig, ToolExecutor, ToolRegistry};
#[cfg(unix)]
use executor_tools::register_core_tools;
use std::collections::HashMap;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use wsl_executor_daemon::config::load;
#[cfg(unix)]
use wsl_executor_daemon::serve;
#[cfg(unix)]
use wsl_runtime::{NativeShell, NativeShellConfig, ShellTool};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        let config = load()?;
        let cwd = config.cwd.clone().unwrap_or(std::env::current_dir()?);

        let base_env = config.base_env.clone().unwrap_or_else(|| {
            HashMap::from([
                (
                    "PATH".to_string(),
                    "/usr/local/bin:/usr/bin:/bin".to_string(),
                ),
                ("LANG".to_string(), "C.UTF-8".to_string()),
            ])
        });
        let shell = NativeShell::new(NativeShellConfig {
            inherit_env: config.inherit_env.unwrap_or(false),
            base_env,
            cwd: Some(cwd.clone()),
            limits: config.limits.clone().unwrap_or_default(),
        });

        let mut registry = ToolRegistry::new();
        register_core_tools(&mut registry);
        let mut tool = ShellTool::new(shell);
        if config.exclusive.unwrap_or(false) {
            tool = tool.exclusive();
        }
        registry.register(tool);
        let tools = registry.list();
        let executor = ToolExecutor::new(
            registry,
            ExecutorConfig {
                cwd,
                env: Arc::new(HashMap::new()),
                tool_timeout: config
                    .tool_timeout_ms
                    .map(std::time::Duration::from_millis),
                concurrency_limit: config.concurrency_limit.unwrap_or(4),
            },
        );

        serve(tokio::io::stdin(), tokio::io::stdout(), executor, tools).await?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err("wsl-executor-daemon only runs on Linux (WSL)".into())
    }
}
