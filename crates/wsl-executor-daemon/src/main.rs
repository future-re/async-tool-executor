#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use tokio::sync::Semaphore;
#[cfg(unix)]
use wsl_executor_daemon::config::load;
#[cfg(unix)]
use wsl_executor_daemon::daemon::{DaemonRuntime, ensure_running, run_tcp, status, stop};
#[cfg(unix)]
use wsl_executor_daemon::serve;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        let command = args
            .iter()
            .find(|arg| !arg.starts_with('-') && !is_config_value(&args, arg))
            .map(String::as_str)
            .unwrap_or("stdio");
        let config_args = config_args(&args);
        match command {
            "serve" => run_tcp(load()?).await?,
            "ensure-running" | "start" => {
                println!(
                    "{}",
                    serde_json::to_string(&ensure_running(&config_args).await?)?
                );
            }
            "status" => match status().await {
                Some(state) => println!("{}", serde_json::to_string(&state)?),
                None => return Err("daemon is not running".into()),
            },
            "stop" => println!("{}", if stop()? { "stopped" } else { "not running" }),
            "stdio" => {
                let token = std::env::var("ATE_TOKEN").unwrap_or_else(|_| "stdio".into());
                let runtime = DaemonRuntime::new(load()?, token)?;
                let session_runtime = runtime.clone();
                serve(
                    tokio::io::stdin(),
                    tokio::io::stdout(),
                    move |token, workspace| session_runtime.session(token, workspace),
                    Arc::new(Semaphore::new(usize::MAX >> 3)),
                )
                .await?;
            }
            other => return Err(format!("unknown command `{other}`").into()),
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err("wsl-executor-daemon only runs on Linux (WSL)".into())
    }
}

#[cfg(unix)]
fn config_args(args: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--config" {
            if let Some(value) = args.get(index + 1) {
                result.push("--config".into());
                result.push(value.clone());
                index += 2;
                continue;
            }
        } else if args[index].starts_with("--config=") {
            result.push(args[index].clone());
        }
        index += 1;
    }
    result
}

#[cfg(unix)]
fn is_config_value(args: &[String], candidate: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == "--config" && pair[1] == candidate)
}
