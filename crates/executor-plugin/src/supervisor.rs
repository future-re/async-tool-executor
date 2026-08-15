use crate::{
    PLUGIN_PROTOCOL_VERSION, PluginManifest, PluginRequest, PluginResponse, PluginToolManifest,
};
use async_trait::async_trait;
use executor_core::{
    ExecutionError, ProgressReporter, Tool, ToolContext, ToolDefinition, ToolOutput,
};
use executor_protocol::{DEFAULT_MAX_FRAME_SIZE, read_frame, write_frame};
use serde_json::Value;
use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct PluginRuntimeConfig {
    pub start_timeout: Duration,
    pub cancel_grace: Duration,
    pub base_env: HashMap<String, String>,
    pub memory_bytes: Option<u64>,
    pub process_count: Option<u64>,
}

impl Default for PluginRuntimeConfig {
    fn default() -> Self {
        Self {
            start_timeout: Duration::from_secs(5),
            cancel_grace: Duration::from_secs(2),
            base_env: HashMap::from([
                ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
                ("LANG".into(), "C.UTF-8".into()),
            ]),
            memory_bytes: None,
            process_count: None,
        }
    }
}

#[derive(Clone)]
pub struct PluginSupervisor {
    commands: mpsc::Sender<ManagerCommand>,
    permits: Arc<Semaphore>,
}

pub struct ExternalToolAdapter {
    tool: PluginToolManifest,
    supervisor: PluginSupervisor,
}

struct Invocation {
    tool: String,
    arguments: Value,
    cwd: PathBuf,
    timeout_ms: Option<u64>,
    progress: ProgressReporter,
    result: oneshot::Sender<Result<Value, ExecutionError>>,
    _permit: OwnedSemaphorePermit,
}

enum ManagerCommand {
    Invoke { id: String, invocation: Invocation },
    Cancel { id: String },
    KillIfPending { id: String },
}

enum ProcessEvent {
    Message {
        generation: u64,
        message: PluginResponse,
    },
    Closed {
        generation: u64,
        reason: String,
    },
}

struct RunningPlugin {
    generation: u64,
    stdin: ChildStdin,
    child: Child,
    reader: tokio::task::JoinHandle<()>,
    stderr: tokio::task::JoinHandle<()>,
}

impl Drop for RunningPlugin {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        self.reader.abort();
        self.stderr.abort();
    }
}

impl PluginSupervisor {
    pub fn new(manifest: PluginManifest, root: PathBuf, config: PluginRuntimeConfig) -> Self {
        let (commands, receiver) = mpsc::channel(256);
        let permits = Arc::new(Semaphore::new(manifest.max_concurrency));
        tokio::spawn(run_manager(
            manifest,
            root,
            config,
            receiver,
            commands.downgrade(),
        ));
        Self { commands, permits }
    }

    async fn invoke(
        &self,
        id: String,
        tool: String,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<Value, ExecutionError> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| plugin_error(&tool, "plugin supervisor stopped"))?;
        let timeout_ms = context.deadline.map(|deadline| {
            u64::try_from(
                deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .as_millis(),
            )
            .unwrap_or(u64::MAX)
        });
        let (result, result_rx) = oneshot::channel();
        let invocation = Invocation {
            tool: tool.clone(),
            arguments,
            cwd: context.cwd.clone(),
            timeout_ms,
            progress: context.progress.clone(),
            result,
            _permit: permit,
        };
        self.commands
            .send(ManagerCommand::Invoke {
                id: id.clone(),
                invocation,
            })
            .await
            .map_err(|_| plugin_error(&tool, "plugin supervisor stopped"))?;
        let commands = self.commands.clone();
        let cancellation = context.cancellation.clone();
        let finished = tokio_util::sync::CancellationToken::new();
        let watcher_finished = finished.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    let _ = commands.send(ManagerCommand::Cancel { id }).await;
                }
                _ = watcher_finished.cancelled() => {}
            }
        });
        let result = result_rx
            .await
            .map_err(|_| plugin_error(&tool, "plugin supervisor disconnected"))?;
        finished.cancel();
        result
    }
}

impl ExternalToolAdapter {
    pub fn new(tool: PluginToolManifest, supervisor: PluginSupervisor) -> Self {
        Self { tool, supervisor }
    }
}

#[async_trait]
impl Tool for ExternalToolAdapter {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.tool.name.clone(),
            description: self.tool.description.clone(),
            input_schema: self.tool.input_schema.clone(),
        }
    }
    fn is_concurrency_safe(&self, _: &Value) -> bool {
        self.tool.concurrency_safe
    }
    fn invocation_detail(&self, _: &Value) -> String {
        self.tool.name.clone()
    }
    async fn invoke(
        &self,
        arguments: Value,
        context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        self.supervisor
            .invoke(
                context.execution_id.clone(),
                self.tool.name.clone(),
                arguments,
                &context,
            )
            .await
            .map(ToolOutput::json)
    }
}

async fn run_manager(
    manifest: PluginManifest,
    root: PathBuf,
    config: PluginRuntimeConfig,
    mut commands: mpsc::Receiver<ManagerCommand>,
    command_tx: mpsc::WeakSender<ManagerCommand>,
) {
    let (events, mut event_rx) = mpsc::channel(256);
    let mut running: Option<RunningPlugin> = None;
    let mut pending = HashMap::<String, Invocation>::new();
    let mut generation = 0_u64;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(ManagerCommand::Invoke { id, invocation }) => {
                    if running.is_none() {
                        generation = generation.wrapping_add(1);
                        match start_plugin(&manifest, &root, &config, generation, events.clone()).await {
                            Ok(process) => running = Some(process),
                            Err(error) => { let _ = invocation.result.send(Err(plugin_error(&invocation.tool, error))); continue; }
                        }
                    }
                    let message = PluginRequest::Invoke { invocation_id: id.clone(), tool: invocation.tool.clone(), arguments: invocation.arguments.clone(), cwd: invocation.cwd.to_string_lossy().into_owned(), timeout_ms: invocation.timeout_ms };
                    if let Some(process) = &mut running {
                        if let Err(error) = write_frame(&mut process.stdin, &message).await {
                            let _ = invocation.result.send(Err(plugin_error(&invocation.tool, format!("writing request failed: {error}"))));
                            stop_process(&mut running).await;
                            fail_all(&mut pending, "plugin process disconnected");
                        } else { pending.insert(id, invocation); }
                    }
                }
                Some(ManagerCommand::Cancel { id }) => {
                    if pending.contains_key(&id) {
                        if let Some(process) = &mut running { let _ = write_frame(&mut process.stdin, &PluginRequest::Cancel { invocation_id: id.clone() }).await; }
                        let tx = command_tx.clone();
                        let grace = config.cancel_grace;
                        tokio::spawn(async move {
                            tokio::time::sleep(grace).await;
                            if let Some(tx) = tx.upgrade() {
                                let _ = tx.send(ManagerCommand::KillIfPending { id }).await;
                            }
                        });
                    }
                }
                Some(ManagerCommand::KillIfPending { id }) => {
                    if pending.contains_key(&id) { stop_process(&mut running).await; fail_all(&mut pending, "plugin ignored cancellation and was terminated"); }
                }
                None => { stop_process(&mut running).await; fail_all(&mut pending, "plugin supervisor stopped"); break; }
            },
            event = event_rx.recv() => match event {
                Some(ProcessEvent::Message { generation, message })
                    if running.as_ref().is_some_and(|process| process.generation == generation) =>
                {
                    handle_response(message, &mut pending, &mut running).await
                }
                Some(ProcessEvent::Closed { generation, reason })
                    if running.as_ref().is_some_and(|process| process.generation == generation) =>
                {
                    stop_process(&mut running).await;
                    fail_all(&mut pending, &reason);
                }
                Some(_) => {}
                None => {}
            }
        }
    }
}

async fn start_plugin(
    manifest: &PluginManifest,
    root: &PathBuf,
    config: &PluginRuntimeConfig,
    generation: u64,
    events: mpsc::Sender<ProcessEvent>,
) -> Result<RunningPlugin, String> {
    let program = if manifest.entrypoint_is_local() {
        root.join(&manifest.entrypoint[0])
    } else {
        PathBuf::from(&manifest.entrypoint[0])
    };
    let mut command = Command::new(program);
    command
        .args(&manifest.entrypoint[1..])
        .current_dir(root)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in &config.base_env {
        command.env(key, value);
    }
    command.env("ATE_PLUGIN_ID", &manifest.id).env(
        "ATE_PLUGIN_PROTOCOL_VERSION",
        PLUGIN_PROTOCOL_VERSION.to_string(),
    );
    let memory_bytes = config.memory_bytes;
    let process_count = config.process_count;
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            apply_rlimit(libc::RLIMIT_AS, memory_bytes)?;
            apply_rlimit(libc::RLIMIT_NPROC, process_count)?;
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawn failed: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("plugin stdin unavailable")?;
    let mut stdout = child.stdout.take().ok_or("plugin stdout unavailable")?;
    write_frame(
        &mut stdin,
        &PluginRequest::Hello {
            protocol_version: PLUGIN_PROTOCOL_VERSION,
            package_id: manifest.id.clone(),
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let hello = tokio::time::timeout(
        config.start_timeout,
        read_frame::<_, PluginResponse>(&mut stdout, DEFAULT_MAX_FRAME_SIZE),
    )
    .await
    .map_err(|_| "plugin handshake timed out".to_string())?
    .map_err(|error| error.to_string())?;
    if hello
        != Some(PluginResponse::HelloAck {
            protocol_version: PLUGIN_PROTOCOL_VERSION,
        })
    {
        return Err(format!("unexpected handshake response: {hello:?}"));
    }
    let reader_events = events.clone();
    let reader = tokio::spawn(async move {
        loop {
            match read_frame(&mut stdout, DEFAULT_MAX_FRAME_SIZE).await {
                Ok(Some(message)) => {
                    if reader_events
                        .send(ProcessEvent::Message {
                            generation,
                            message,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = reader_events
                        .send(ProcessEvent::Closed {
                            generation,
                            reason: "plugin exited".into(),
                        })
                        .await;
                    break;
                }
                Err(error) => {
                    let _ = reader_events
                        .send(ProcessEvent::Closed {
                            generation,
                            reason: format!("plugin protocol error: {error}"),
                        })
                        .await;
                    break;
                }
            }
        }
    });
    let mut stderr_pipe = child.stderr.take().ok_or("plugin stderr unavailable")?;
    let plugin_id = manifest.id.clone();
    let stderr = tokio::spawn(async move {
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            match stderr_pipe.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(size) => {
                    let chunk = String::from_utf8_lossy(&buffer[..size]);
                    tracing::warn!(plugin = %plugin_id, "{chunk}");
                }
            }
        }
    });
    Ok(RunningPlugin {
        generation,
        stdin,
        child,
        reader,
        stderr,
    })
}

#[cfg(target_env = "gnu")]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(target_env = "gnu"))]
type RlimitResource = libc::c_int;

fn apply_rlimit(resource: RlimitResource, value: Option<u64>) -> std::io::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

async fn handle_response(
    message: PluginResponse,
    pending: &mut HashMap<String, Invocation>,
    running: &mut Option<RunningPlugin>,
) {
    match message {
        PluginResponse::Progress {
            invocation_id,
            message,
            data,
        } => {
            if let Some(invocation) = pending.get(&invocation_id) {
                invocation.progress.emit(message, data);
            } else {
                protocol_violation(
                    running,
                    pending,
                    format!("progress for unknown invocation `{invocation_id}`"),
                )
                .await;
            }
        }
        PluginResponse::Completed {
            invocation_id,
            content,
        } => {
            if let Some(invocation) = pending.remove(&invocation_id) {
                let _ = invocation.result.send(Ok(content));
            } else {
                protocol_violation(
                    running,
                    pending,
                    format!("completion for unknown invocation `{invocation_id}`"),
                )
                .await;
            }
        }
        PluginResponse::Failed {
            invocation_id,
            code,
            message,
        } => {
            if let Some(invocation) = pending.remove(&invocation_id) {
                let tool = invocation.tool.clone();
                let _ = invocation
                    .result
                    .send(Err(plugin_error(&tool, format!("{code}: {message}"))));
            } else {
                protocol_violation(
                    running,
                    pending,
                    format!("failure for unknown invocation `{invocation_id}`"),
                )
                .await;
            }
        }
        PluginResponse::HelloAck { .. } | PluginResponse::ShutdownAck => {
            protocol_violation(running, pending, "unexpected lifecycle response".into()).await
        }
    }
}

async fn protocol_violation(
    running: &mut Option<RunningPlugin>,
    pending: &mut HashMap<String, Invocation>,
    message: String,
) {
    stop_process(running).await;
    fail_all(pending, &format!("plugin protocol violation: {message}"));
}

async fn stop_process(running: &mut Option<RunningPlugin>) {
    if let Some(mut process) = running.take() {
        let _ = write_frame(&mut process.stdin, &PluginRequest::Shutdown).await;
        let exited = matches!(
            tokio::time::timeout(Duration::from_millis(100), process.child.wait()).await,
            Ok(Ok(_))
        );
        if !exited {
            if let Some(pid) = process.child.id() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = process.child.wait().await;
        }
        process.reader.abort();
        process.stderr.abort();
    }
}

fn fail_all(pending: &mut HashMap<String, Invocation>, message: &str) {
    for (_, invocation) in pending.drain() {
        let tool = invocation.tool.clone();
        let _ = invocation.result.send(Err(plugin_error(&tool, message)));
    }
}

fn plugin_error(tool: &str, message: impl Into<String>) -> ExecutionError {
    ExecutionError::ToolExecution {
        tool: tool.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use executor_core::Tool;
    use serde_json::json;
    use std::collections::HashMap;
    use std::fs;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn persistent_plugin_handles_multiple_invocations() {
        if Command::new("python3")
            .arg("--version")
            .output()
            .await
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("ate-supervisor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("plugin.py"), r#"
import json, struct, sys, time
count = 0
def read():
    head = sys.stdin.buffer.read(4)
    if not head: return None
    size = struct.unpack('>I', head)[0]
    return json.loads(sys.stdin.buffer.read(size))
def write(value):
    data = json.dumps(value, separators=(',', ':')).encode()
    sys.stdout.buffer.write(struct.pack('>I', len(data)) + data)
    sys.stdout.buffer.flush()
while True:
    msg = read()
    if msg is None: break
    if msg['type'] == 'hello': write({'type':'hello_ack','protocol_version':1})
    elif msg['type'] == 'invoke':
        if msg['arguments'].get('crash'): sys.exit(7)
        if msg['arguments'].get('hang'): time.sleep(60)
        count += 1
        write({'type':'completed','invocation_id':msg['invocation_id'],'content':{'count':count,'arguments':msg['arguments']}})
    elif msg['type'] == 'shutdown':
        write({'type':'shutdown_ack'})
        break
"#).unwrap();
        let tool = PluginToolManifest {
            name: "test_echo".into(),
            description: "echo".into(),
            input_schema: json!({}),
            concurrency_safe: true,
        };
        let manifest = PluginManifest {
            schema_version: 1,
            id: "com.example.test".into(),
            version: "1.0.0".into(),
            entrypoint: vec!["python3".into(), "plugin.py".into()],
            required_commands: vec!["python3".into()],
            max_concurrency: 2,
            tools: vec![tool.clone()],
        };
        let supervisor = PluginSupervisor::new(
            manifest,
            root.clone(),
            PluginRuntimeConfig {
                cancel_grace: Duration::from_millis(50),
                ..PluginRuntimeConfig::default()
            },
        );
        let adapter = ExternalToolAdapter::new(tool, supervisor);
        for (index, expected) in [("one", 1), ("two", 2)] {
            let context = ToolContext {
                execution_id: index.into(),
                cwd: root.clone(),
                env: Arc::new(HashMap::new()),
                deadline: None,
                cancellation: CancellationToken::new(),
                progress: ProgressReporter::default(),
            };
            let output = adapter
                .invoke(json!({"value": index}), context)
                .await
                .unwrap();
            assert_eq!(output.content["count"], expected);
        }

        let crash_context = context("crash", &root, CancellationToken::new());
        assert!(
            adapter
                .invoke(json!({"crash": true}), crash_context)
                .await
                .is_err()
        );
        let recovered = adapter
            .invoke(
                json!({"value": "recovered"}),
                context("recovered", &root, CancellationToken::new()),
            )
            .await
            .unwrap();
        assert_eq!(recovered.content["count"], 1);

        let cancellation = CancellationToken::new();
        let cancel_signal = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel_signal.cancel();
        });
        let hung = tokio::time::timeout(
            Duration::from_secs(1),
            adapter.invoke(json!({"hang": true}), context("hang", &root, cancellation)),
        )
        .await
        .expect("hung plugin was not terminated")
        .unwrap_err();
        assert!(hung.to_string().contains("ignored cancellation"));
        let _ = fs::remove_dir_all(root);
    }

    fn context(id: &str, cwd: &std::path::Path, cancellation: CancellationToken) -> ToolContext {
        ToolContext {
            execution_id: id.into(),
            cwd: cwd.to_path_buf(),
            env: Arc::new(HashMap::new()),
            deadline: None,
            cancellation,
            progress: ProgressReporter::default(),
        }
    }
}
