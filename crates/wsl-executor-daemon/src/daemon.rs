use crate::config::{DaemonConfig, default_plugin_dir, home_dir};
use crate::server::{Session, serve};
use executor_core::{ExecutorConfig, ToolExecutor, ToolRegistry};
use executor_plugin::{ExternalToolAdapter, PluginRuntimeConfig, PluginStore, PluginSupervisor};
use executor_protocol::{PROTOCOL_VERSION, ProtocolFailure};
use executor_tools::register_core_tools;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use wsl_runtime::{NativeShell, NativeShellConfig, ShellTool};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    pub port: u16,
    pub pid: u32,
    pub protocol_version: u32,
    pub token: String,
}

#[derive(Clone)]
pub struct DaemonRuntime {
    config: DaemonConfig,
    workspace_root: PathBuf,
    token: String,
    global_permits: Arc<Semaphore>,
}

impl DaemonRuntime {
    pub fn new(config: DaemonConfig, token: String) -> Result<Self, String> {
        let root = config
            .workspace_root
            .clone()
            .or_else(home_dir)
            .ok_or_else(|| {
                "workspace_root is not configured and HOME is unavailable".to_string()
            })?;
        let workspace_root = std::fs::canonicalize(&root)
            .map_err(|error| format!("invalid workspace_root {}: {error}", root.display()))?;
        reject_windows_filesystem(&workspace_root)?;
        let global_limit = config.global_concurrency_limit.unwrap_or(8).max(1);
        Ok(Self {
            config,
            workspace_root,
            token,
            global_permits: Arc::new(Semaphore::new(global_limit)),
        })
    }

    pub fn session(&self, token: &str, workspace: &str) -> Result<Session, ProtocolFailure> {
        if token != self.token {
            return Err(ProtocolFailure::new(
                "authentication_failed",
                "invalid daemon token",
            ));
        }
        let workspace = validate_workspace(&self.workspace_root, Path::new(workspace))
            .map_err(|message| ProtocolFailure::new("invalid_workspace", message))?;
        let base_env = self.config.base_env.clone().unwrap_or_else(|| {
            HashMap::from([
                (
                    "PATH".to_string(),
                    "/usr/local/bin:/usr/bin:/bin".to_string(),
                ),
                ("LANG".to_string(), "C.UTF-8".to_string()),
            ])
        });
        let shell = NativeShell::new(NativeShellConfig {
            inherit_env: self.config.inherit_env.unwrap_or(false),
            base_env: base_env.clone(),
            cwd: Some(workspace.clone()),
            workspace_root: Some(workspace.clone()),
            limits: self.config.limits.clone().unwrap_or_default(),
        });
        let mut registry = ToolRegistry::new();
        register_core_tools(&mut registry)
            .map_err(|error| ProtocolFailure::new("setup_failed", error.to_string()))?;
        let mut shell_tool = ShellTool::new(shell);
        if self.config.exclusive.unwrap_or(false) {
            shell_tool = shell_tool.exclusive();
        }
        registry
            .register(shell_tool)
            .map_err(|error| ProtocolFailure::new("setup_failed", error.to_string()))?;
        self.register_plugins(&mut registry, &base_env)?;
        let tools = registry.list();
        let executor = ToolExecutor::new(
            registry,
            ExecutorConfig {
                cwd: workspace.clone(),
                env: Arc::new(HashMap::new()),
                tool_timeout: self
                    .config
                    .tool_timeout_ms
                    .map(std::time::Duration::from_millis),
                concurrency_limit: self.config.concurrency_limit.unwrap_or(4).max(1),
            },
        );
        Ok(Session {
            executor,
            tools,
            workspace,
        })
    }

    fn register_plugins(
        &self,
        registry: &mut ToolRegistry,
        base_env: &HashMap<String, String>,
    ) -> Result<(), ProtocolFailure> {
        if !self.config.plugins_enabled.unwrap_or(true) {
            return Ok(());
        }
        let plugin_dir = self
            .config
            .plugin_dir
            .clone()
            .or_else(default_plugin_dir)
            .ok_or_else(|| ProtocolFailure::new("plugin_setup_failed", "could not resolve plugin directory"))?;
        let store = PluginStore::new(plugin_dir);
        let (plugins, diagnostics) = store.discover().map_err(|error| {
            ProtocolFailure::new(
                "plugin_setup_failed",
                format!("plugin discovery failed: {error}"),
            )
        })?;
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
                        self.config.plugin_start_timeout_ms.unwrap_or(5000),
                    ),
                    cancel_grace: std::time::Duration::from_millis(
                        self.config.plugin_cancel_grace_ms.unwrap_or(2000),
                    ),
                    base_env: base_env.clone(),
                    memory_bytes: self
                        .config
                        .limits
                        .as_ref()
                        .and_then(|limits| limits.memory_bytes),
                    process_count: self
                        .config
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
        Ok(())
    }

    pub async fn serve_stream(&self, stream: TcpStream) -> Result<(), crate::DaemonError> {
        let (reader, writer) = stream.into_split();
        let runtime = self.clone();
        serve(
            reader,
            writer,
            move |token, workspace| runtime.session(token, workspace),
            Arc::clone(&self.global_permits),
        )
        .await
    }
}

pub fn validate_workspace(root: &Path, workspace: &Path) -> Result<PathBuf, String> {
    if !workspace.is_absolute() {
        return Err("workspace must be an absolute Linux path".into());
    }
    let canonical = std::fs::canonicalize(workspace)
        .map_err(|error| format!("workspace {} is unavailable: {error}", workspace.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "workspace {} is outside workspace_root {}",
            canonical.display(),
            root.display()
        ));
    }
    reject_windows_filesystem(&canonical)?;
    Ok(canonical)
}

fn reject_windows_filesystem(path: &Path) -> Result<(), String> {
    if path.starts_with("/mnt") {
        return Err(format!(
            "workspace {} is under /mnt and is not on WSL ext4",
            path.display()
        ));
    }
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| "workspace path contains a NUL byte".to_string())?;
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stats) } != 0 {
        return Err(format!(
            "cannot inspect workspace filesystem: {}",
            std::io::Error::last_os_error()
        ));
    }
    const V9FS_MAGIC: libc::c_long = 0x0102_1997;
    if stats.f_type as libc::c_long == V9FS_MAGIC {
        return Err(format!(
            "workspace {} is on a DrvFS/9P filesystem",
            path.display()
        ));
    }
    Ok(())
}

pub async fn run_tcp(config: DaemonConfig) -> Result<(), Box<dyn std::error::Error>> {
    let _service_lock = ServiceLock::acquire()?;
    let token = random_token()?;
    let runtime = DaemonRuntime::new(config, token.clone())?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let state = DaemonState {
        port: listener.local_addr()?.port(),
        pid: std::process::id(),
        protocol_version: PROTOCOL_VERSION,
        token,
    };
    write_state(&state)?;
    loop {
        let (stream, _) = listener.accept().await?;
        let runtime = runtime.clone();
        tokio::spawn(async move {
            if let Err(error) = runtime.serve_stream(stream).await {
                tracing::warn!("client session failed: {error}");
            }
        });
    }
}

pub async fn ensure_running(
    extra_args: &[String],
) -> Result<DaemonState, Box<dyn std::error::Error>> {
    let _lock = StateLock::acquire()?;
    if let Some(state) = read_healthy_state().await {
        return Ok(state);
    }
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.arg("serve").args(extra_args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn()?;
    for _ in 0..100 {
        if let Some(state) = read_healthy_state().await {
            return Ok(state);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    Err("daemon did not become ready".into())
}

pub async fn status() -> Option<DaemonState> {
    read_healthy_state().await
}

pub fn stop() -> Result<bool, Box<dyn std::error::Error>> {
    let _lock = StateLock::acquire()?;
    let Some(state) = read_state()? else {
        return Ok(false);
    };
    if process_is_daemon(state.pid) {
        unsafe { libc::kill(state.pid as i32, libc::SIGTERM) };
        let _ = std::fs::remove_file(state_path()?);
        Ok(true)
    } else {
        let _ = std::fs::remove_file(state_path()?);
        Ok(false)
    }
}

async fn read_healthy_state() -> Option<DaemonState> {
    let state = read_state().ok().flatten()?;
    if state.protocol_version != PROTOCOL_VERSION || !process_is_daemon(state.pid) {
        return None;
    }
    TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, state.port))
        .await
        .ok()
        .map(|_| state)
}

fn process_is_daemon(pid: u32) -> bool {
    let Ok(process_exe) = std::fs::canonicalize(format!("/proc/{pid}/exe")) else {
        return false;
    };
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .is_ok_and(|current| current == process_exe)
}

fn random_token() -> Result<String, std::io::Error> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_state(state: &DaemonState) -> Result<(), Box<dyn std::error::Error>> {
    let path = state_path()?;
    std::fs::create_dir_all(path.parent().expect("state path has parent"))?;
    let temporary = path.with_extension(format!("json.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, state)?;
    file.flush()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn read_state() -> Result<Option<DaemonState>, Box<dyn std::error::Error>> {
    let path = state_path()?;
    match File::open(path) {
        Ok(file) => Ok(Some(serde_json::from_reader(file)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn state_path() -> Result<PathBuf, String> {
    Ok(home_dir()
        .ok_or_else(|| "HOME is unavailable".to_string())?
        .join(".local/state/ate/daemon.json"))
}

struct StateLock(File);

impl StateLock {
    fn acquire() -> Result<Self, Box<dyn std::error::Error>> {
        let path = state_path()?.with_extension("lock");
        std::fs::create_dir_all(path.parent().expect("lock path has parent"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(file))
    }
}

struct ServiceLock(File);

impl ServiceLock {
    fn acquire() -> Result<Self, Box<dyn std::error::Error>> {
        let path = state_path()?.with_extension("service.lock");
        std::fs::create_dir_all(path.parent().expect("lock path has parent"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        if unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        } != 0
        {
            return Err("another daemon instance is already running".into());
        }
        Ok(Self(file))
    }
}

impl Drop for ServiceLock {
    fn drop(&mut self) {
        unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.0), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_and_outside_workspaces() {
        let root = std::env::temp_dir().join(format!("ate-root-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("ate-outside-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        assert_eq!(validate_workspace(&root, &root).unwrap(), root);
        assert!(validate_workspace(&root, Path::new("relative")).is_err());
        assert!(validate_workspace(&root, &outside).is_err());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn rejects_symlink_escape() {
        let base = std::env::temp_dir().join(format!("ate-symlink-{}", std::process::id()));
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        assert!(validate_workspace(&root, &root.join("escape")).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_mnt_even_before_filesystem_detection() {
        assert!(reject_windows_filesystem(Path::new("/mnt/c/project")).is_err());
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.0), libc::LOCK_UN) };
    }
}
