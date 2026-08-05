//! The Linux/WSL execution backend.
//!
//! [`NativeShell`] runs commands directly on the local operating system under
//! sandbox constraints: a controlled working directory, an environment that is
//! assembled from configured bases rather than inherited blindly, resource
//! limits set through `setrlimit`, and whole-process-group termination so a
//! cancelled command cannot leave orphaned children behind.

use crate::{CommandOutput, CommandSpec, ResourceLimits, Shell};
use async_trait::async_trait;
use executor_core::ExecutionError;
use std::collections::HashMap;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

/// Configuration for a [`NativeShell`].
#[derive(Debug, Clone, Default)]
pub struct NativeShellConfig {
    /// Whether commands start from the parent process environment. Sandboxing
    /// defaults to `false`; enable for local development convenience.
    pub inherit_env: bool,
    /// Base environment applied to every command. Per-command `env` entries
    /// take precedence over these values.
    pub base_env: HashMap<String, String>,
    /// Fallback working directory when a command does not set one.
    pub cwd: Option<PathBuf>,
    /// Default resource ceilings applied to every command.
    pub limits: ResourceLimits,
}

/// Runs commands on the local operating system.
pub struct NativeShell {
    config: NativeShellConfig,
}

impl NativeShell {
    pub fn new(config: NativeShellConfig) -> Self {
        Self { config }
    }

    /// Convenience backend that inherits the parent environment, for local use.
    pub fn local() -> Self {
        Self {
            config: NativeShellConfig {
                inherit_env: true,
                ..NativeShellConfig::default()
            },
        }
    }

    fn assemble_env(&self, spec: &CommandSpec) -> HashMap<String, String> {
        let mut env = if self.config.inherit_env {
            std::env::vars().collect()
        } else {
            HashMap::new()
        };
        for (key, value) in &self.config.base_env {
            env.insert(key.clone(), value.clone());
        }
        for (key, value) in &spec.env {
            env.insert(key.clone(), value.clone());
        }
        env
    }

    fn spawn(&self, spec: &CommandSpec) -> Result<Child, ExecutionError> {
        let program = spec
            .argv
            .first()
            .ok_or_else(|| ExecutionError::Other("command argv is empty".into()))?;

        let mut command = Command::new(program);
        command
            .args(&spec.argv[1..])
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(cwd) = spec.cwd.clone().or_else(|| self.config.cwd.clone()) {
            command.current_dir(&cwd);
        }

        let env = self.assemble_env(spec);
        command.env_clear();
        for (key, value) in &env {
            command.env(key, value);
        }

        let memory = spec.limits.memory_bytes.or(self.config.limits.memory_bytes);
        let cpu = spec.limits.cpu_seconds.or(self.config.limits.cpu_seconds);
        let processes = spec
            .limits
            .process_count
            .or(self.config.limits.process_count);

        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                apply_rlimit(libc::RLIMIT_AS, memory)?;
                apply_rlimit(libc::RLIMIT_CPU, cpu)?;
                apply_rlimit(libc::RLIMIT_NPROC, processes)?;
                Ok(())
            });
        }

        command
            .spawn()
            .map_err(|error| ExecutionError::Other(format!("spawn `{program}` failed: {error}")))
    }
}

fn apply_rlimit(resource: libc::__rlimit_resource_t, value: Option<u64>) -> io::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    // SAFETY: `setrlimit` touches only the current process; the target value is
    // bounded by the caller's configured ceiling.
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sends `SIGKILL` to the whole process group. `pgid` is the child's pid, which
/// is the process-group id because the child places itself in a new group via
/// `setpgid(0, 0)`.
fn kill_group(pgid: i32) {
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

/// Kills the process group when dropped while still armed. This guarantees that
/// a [`NativeShell::run`] future dropped by timeout or cancellation still
/// terminates every process it spawned, even if the async cleanup never runs.
struct GroupKiller {
    pgid: i32,
    armed: bool,
}

impl GroupKiller {
    fn new(pgid: i32) -> Self {
        Self { pgid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for GroupKiller {
    fn drop(&mut self) {
        if self.armed {
            kill_group(self.pgid);
        }
    }
}

/// Reads up to `cap` bytes, then drains the remainder so the writer is never
/// blocked on a full pipe. Returns the captured text and whether it truncated.
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(mut reader: R, cap: usize) -> (String, bool) {
    let mut bytes = Vec::with_capacity(cap.min(8192));
    let mut scratch = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = match reader.read(&mut scratch).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if bytes.len() >= cap {
            truncated = true;
            continue;
        }
        let remaining = cap - bytes.len();
        if n > remaining {
            truncated = true;
            bytes.extend_from_slice(&scratch[..remaining]);
        } else {
            bytes.extend_from_slice(&scratch[..n]);
        }
    }
    (String::from_utf8_lossy(&bytes).into_owned(), truncated)
}

#[async_trait]
impl Shell for NativeShell {
    async fn run(
        &self,
        spec: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput, ExecutionError> {
        let mut child = self.spawn(&spec)?;
        let pgid = child
            .id()
            .ok_or_else(|| ExecutionError::Other("spawned child has no pid".into()))?
            as i32;
        let mut killer = GroupKiller::new(pgid);

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let stdout_task = tokio::spawn(read_capped(stdout, spec.limits.max_stdout));
        let stderr_task = tokio::spawn(read_capped(stderr, spec.limits.max_stderr));
        let started = Instant::now();

        let (exit_code, signal, terminated) = tokio::select! {
            _ = cancellation.cancelled() => {
                kill_group(pgid);
                let _ = child.wait().await;
                (None, None, true)
            }
            status = child.wait() => {
                let status = status.map_err(|error| {
                    ExecutionError::Other(format!("waiting for command failed: {error}"))
                })?;
                (status.code(), status.signal(), false)
            }
        };
        killer.disarm();

        let (stdout, stdout_truncated) = stdout_task.await.unwrap_or_default();
        let (stderr, stderr_truncated) = stderr_task.await.unwrap_or_default();

        Ok(CommandOutput {
            exit_code,
            signal,
            stdout,
            stderr,
            truncated: stdout_truncated || stderr_truncated,
            terminated,
            duration: started.elapsed(),
        })
    }
}
