//! Command execution backends.
//!
//! A [`Shell`] is the execution side of a tool request: it runs a resolved
//! command and returns a structured result. The forwarded side of the system
//! (the WSL guest) is a [`NativeShell`]; the Windows host side that forwards
//! requests into WSL will be another implementation of the same trait.

use async_trait::async_trait;
use executor_core::{ExecutionError, Tool, ToolContext, ToolDefinition, ToolOutput};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Sandbox resource ceilings applied to a spawned process.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLimits {
    /// Virtual memory ceiling per process (`RLIMIT_AS`), in bytes.
    pub memory_bytes: Option<u64>,
    /// CPU seconds per process (`RLIMIT_CPU`).
    pub cpu_seconds: Option<u64>,
    /// Maximum processes the same user may have (`RLIMIT_NPROC`).
    pub process_count: Option<u64>,
    /// Maximum stdout captured; excess output is drained and discarded.
    pub max_stdout: usize,
    /// Maximum stderr captured; excess output is drained and discarded.
    pub max_stderr: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            memory_bytes: None,
            cpu_seconds: None,
            process_count: None,
            max_stdout: 1 << 20,
            max_stderr: 1 << 20,
        }
    }
}

/// A fully resolved command ready for execution by a [`Shell`].
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// Program and arguments; the first element names the executable.
    pub argv: Vec<String>,
    /// Working directory for the command. The backend falls back to its own
    /// configured directory when absent.
    pub cwd: Option<PathBuf>,
    /// Environment applied on top of the backend's base environment.
    pub env: HashMap<String, String>,
    /// Resource ceilings; unset entries fall back to the backend defaults.
    pub limits: ResourceLimits,
}

/// Structured outcome of a command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit code, or `None` when the process ended on a signal or was killed.
    pub exit_code: Option<i32>,
    /// Terminating signal, when the process ended on a signal.
    pub signal: Option<i32>,
    /// Captured standard output (lossy UTF-8).
    pub stdout: String,
    /// Captured standard error (lossy UTF-8).
    pub stderr: String,
    /// Whether stdout or stderr exceeded its capture ceiling.
    pub truncated: bool,
    /// Whether the command was killed by cancellation rather than exiting.
    pub terminated: bool,
    /// Wall-clock time from spawn until the process was reaped.
    pub duration: Duration,
}

/// Backend that runs a [`CommandSpec`] under its sandbox rules.
#[async_trait]
pub trait Shell: Send + Sync + 'static {
    /// Runs the command and reports its structured result. The backend applies
    /// sandbox constraints (working directory, environment, resource limits,
    /// and process-tree lifecycle). A returned `Ok` merely means the command
    /// was executed; check [`CommandOutput`] for its exit status.
    async fn run(
        &self,
        spec: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput, ExecutionError>;
}

/// A [`Tool`] that executes commands through a [`Shell`] backend.
///
/// Invocations take a JSON object:
/// ```json
/// {
///   "argv": ["ls", "-la"],
///   "cwd": "/tmp",
///   "env": { "FOO": "bar" },
///   "memory_bytes": 1048576,
///   "cpu_seconds": 5,
///   "process_count": 32
/// }
/// ```
/// Only `argv` is required. The structured result carries `exit_code`, `signal`,
/// `stdout`, `stderr`, `truncated`, `terminated`, and `duration_ms`. A non-zero
/// exit is a successful tool execution whose payload reports the exit code, so
/// the caller can distinguish a failed command from an execution failure.
pub struct ShellTool {
    name: String,
    description: String,
    shell: Arc<dyn Shell>,
    exclusive: bool,
}

impl ShellTool {
    pub fn new<S: Shell>(shell: S) -> Self {
        Self {
            name: "shell".into(),
            description: "runs a command on the execution backend".into(),
            shell: Arc::new(shell),
            exclusive: false,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Marks the tool as requiring the exclusive execution slot. Shell backends
    /// isolate work in processes and are concurrency-safe by default.
    pub fn exclusive(mut self) -> Self {
        self.exclusive = true;
        self
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: json!({
                "type": "object",
                "required": ["argv"],
                "properties": {
                    "argv": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1
                    },
                    "cwd": { "type": "string" },
                    "env": {
                        "type": "object",
                        "additionalProperties": { "type": "string" }
                    },
                    "memory_bytes": { "type": "integer", "minimum": 0 },
                    "cpu_seconds": { "type": "integer", "minimum": 0 },
                    "process_count": { "type": "integer", "minimum": 0 }
                }
            }),
        }
    }

    fn is_concurrency_safe(&self, _arguments: &Value) -> bool {
        !self.exclusive
    }

    fn invocation_detail(&self, arguments: &Value) -> String {
        argv_from(arguments)
            .map(|argv| argv.join(" "))
            .unwrap_or_else(|_| self.name.clone())
    }

    async fn invoke(
        &self,
        arguments: Value,
        context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        let argv = argv_from(&arguments)?;

        let mut env = HashMap::new();
        if let Some(entries) = arguments.get("env").and_then(Value::as_object) {
            for (key, value) in entries {
                if let Some(value) = value.as_str() {
                    env.insert(key.clone(), value.to_string());
                }
            }
        }

        let limits = ResourceLimits {
            memory_bytes: arguments.get("memory_bytes").and_then(Value::as_u64),
            cpu_seconds: arguments.get("cpu_seconds").and_then(Value::as_u64),
            process_count: arguments.get("process_count").and_then(Value::as_u64),
            ..ResourceLimits::default()
        };

        let spec = CommandSpec {
            argv,
            cwd: arguments
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from),
            env,
            limits,
        };

        let output = self.shell.run(spec, context.cancellation.clone()).await?;

        Ok(ToolOutput::json(json!({
            "exit_code": output.exit_code,
            "signal": output.signal,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "truncated": output.truncated,
            "terminated": output.terminated,
            "duration_ms": output.duration.as_millis(),
        })))
    }
}

/// Extracts the required non-empty `argv` array. Shared by the tool's schema
/// validation entry points so the argument shape is checked once.
fn argv_from(arguments: &Value) -> Result<Vec<String>, ExecutionError> {
    let argv = arguments
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| ExecutionError::Other("shell: `argv` must be a non-empty array".into()))?;
    if argv.is_empty() {
        return Err(ExecutionError::Other(
            "shell: `argv` must not be empty".into(),
        ));
    }
    argv.iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                ExecutionError::Other("shell: `argv` entries must be strings".into())
            })
        })
        .collect()
}
