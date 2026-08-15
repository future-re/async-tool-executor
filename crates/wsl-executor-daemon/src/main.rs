#[cfg(unix)]
use executor_core::{ExecutorConfig, ToolExecutor, ToolRegistry};
#[cfg(unix)]
use executor_plugin::{ExternalToolAdapter, PluginRuntimeConfig, PluginStore, PluginSupervisor};
#[cfg(unix)]
use executor_tools::register_core_tools;
#[cfg(unix)]
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use wsl_executor_daemon::config::{default_plugin_dir, load};
#[cfg(unix)]
use wsl_executor_daemon::serve;
#[cfg(unix)]
use wsl_runtime::{NativeShell, NativeShellConfig, ShellTool};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        let config = load()?;
        if std::env::args().nth(1).as_deref() == Some("tool") {
            return manage_tools(&config);
        }
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
            base_env: base_env.clone(),
            cwd: Some(cwd.clone()),
            limits: config.limits.clone().unwrap_or_default(),
        });

        let mut registry = ToolRegistry::new();
        register_core_tools(&mut registry)?;
        let mut tool = ShellTool::new(shell);
        if config.exclusive.unwrap_or(false) {
            tool = tool.exclusive();
        }
        registry.register(tool)?;
        if config.plugins_enabled.unwrap_or(true) {
            let plugin_dir = config
                .plugin_dir
                .clone()
                .or_else(default_plugin_dir)
                .ok_or("could not resolve plugin directory")?;
            let store = PluginStore::new(plugin_dir);
            let (plugins, diagnostics) = store.discover()?;
            for diagnostic in diagnostics {
                eprintln!("plugin skipped: {diagnostic}");
            }
            let eligible = plugins
                .into_iter()
                .filter(|plugin| plugin.enabled)
                .filter(|plugin| {
                    if plugin.missing_commands.is_empty() {
                        true
                    } else {
                        eprintln!(
                            "plugin {} skipped; missing commands: {}",
                            plugin.manifest.id,
                            plugin.missing_commands.join(", ")
                        );
                        false
                    }
                })
                .collect::<Vec<_>>();
            let builtins = registry
                .list()
                .into_iter()
                .map(|tool| tool.name)
                .collect::<HashSet<_>>();
            let mut name_counts = HashMap::<String, usize>::new();
            for plugin in &eligible {
                for tool in &plugin.manifest.tools {
                    *name_counts.entry(tool.name.clone()).or_default() += 1;
                }
            }
            for plugin in eligible {
                let conflicts = plugin
                    .manifest
                    .tools
                    .iter()
                    .filter(|tool| {
                        builtins.contains(&tool.name)
                            || name_counts.get(&tool.name).copied().unwrap_or_default() > 1
                    })
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>();
                if !conflicts.is_empty() {
                    eprintln!(
                        "plugin {} skipped; conflicting tool names: {}",
                        plugin.manifest.id,
                        conflicts.join(", ")
                    );
                    continue;
                }
                let supervisor = PluginSupervisor::new(
                    plugin.manifest.clone(),
                    plugin.path,
                    PluginRuntimeConfig {
                        start_timeout: std::time::Duration::from_millis(
                            config.plugin_start_timeout_ms.unwrap_or(5000),
                        ),
                        cancel_grace: std::time::Duration::from_millis(
                            config.plugin_cancel_grace_ms.unwrap_or(2000),
                        ),
                        base_env: base_env.clone(),
                        memory_bytes: config
                            .limits
                            .as_ref()
                            .and_then(|limits| limits.memory_bytes),
                        process_count: config
                            .limits
                            .as_ref()
                            .and_then(|limits| limits.process_count),
                    },
                );
                for descriptor in plugin.manifest.tools {
                    if let Err(error) =
                        registry.register(ExternalToolAdapter::new(descriptor, supervisor.clone()))
                    {
                        eprintln!("plugin {} skipped tool: {error}", plugin.manifest.id);
                    }
                }
            }
        }
        let tools = registry.list();
        let executor = ToolExecutor::new(
            registry,
            ExecutorConfig {
                cwd,
                env: Arc::new(HashMap::new()),
                tool_timeout: config.tool_timeout_ms.map(std::time::Duration::from_millis),
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

#[cfg(unix)]
fn manage_tools(
    config: &wsl_executor_daemon::config::DaemonConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = config
        .plugin_dir
        .clone()
        .or_else(default_plugin_dir)
        .ok_or("could not resolve plugin directory")?;
    let store = PluginStore::new(root);
    let args = std::env::args().skip(2).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("install") => {
            let replace = args.iter().any(|arg| arg == "--replace");
            let plugin = store.install(std::io::stdin().lock(), replace)?;
            println!("{}", serde_json::to_string_pretty(&plugin)?);
        }
        Some("list") => {
            let (plugins, diagnostics) = store.discover()?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({ "plugins": plugins, "diagnostics": diagnostics })
                )?
            );
        }
        Some("enable" | "disable" | "remove") => {
            let operation = args[0].as_str();
            let id = args.get(1).ok_or("tool operation requires a plugin id")?;
            match operation {
                "enable" => store.set_enabled(id, true)?,
                "disable" => store.set_enabled(id, false)?,
                "remove" => store.remove(id)?,
                _ => unreachable!(),
            }
            println!("{}", serde_json::json!({ "ok": true, "plugin_id": id }));
        }
        _ => return Err("usage: ate-daemon tool <install|list|enable|disable|remove>".into()),
    }
    Ok(())
}
