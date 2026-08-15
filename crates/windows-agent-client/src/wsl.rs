use crate::{ClientError, ExecutionClient, WslClientConfig};
use async_trait::async_trait;
use executor_protocol::{
    ClientMessage, DEFAULT_MAX_FRAME_SIZE, ExecutionRequest, ExecutionResult, PROTOCOL_VERSION,
    ServerMessage, ToolDescriptor, read_frame, write_frame,
};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

type PendingResult = oneshot::Sender<Result<ExecutionResult, ClientError>>;
type CancelAck = oneshot::Sender<bool>;
type ToolsAck = oneshot::Sender<Result<Vec<ToolDescriptor>, ClientError>>;

pub struct WslClient {
    requests: mpsc::UnboundedSender<ClientMessage>,
    pending: Arc<Mutex<HashMap<String, PendingResult>>>,
    cancel_acks: Arc<Mutex<HashMap<String, CancelAck>>>,
    tool_acks: Arc<Mutex<HashMap<String, ToolsAck>>>,
    next_request_id: AtomicU64,
    child: Child,
    writer_task: JoinHandle<()>,
    reader_task: JoinHandle<()>,
}

impl WslClient {
    pub async fn connect(config: WslClientConfig) -> Result<Self, ClientError> {
        let mut child = Command::new("wsl.exe")
            .arg("--distribution")
            .arg(config.distribution)
            .arg("--exec")
            .arg(config.guest_program)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| ClientError::Spawn(error.to_string()))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::Spawn("WSL stdin was not piped".into()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::Spawn("WSL stdout was not piped".into()))?;

        write_frame(
            &mut stdin,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .map_err(|error| ClientError::Handshake(error.to_string()))?;
        let hello = read_frame(&mut stdout, DEFAULT_MAX_FRAME_SIZE)
            .await
            .map_err(|error| ClientError::Handshake(error.to_string()))?;
        if !matches!(
            hello,
            Some(ServerMessage::HelloAck {
                protocol_version: PROTOCOL_VERSION
            })
        ) {
            return Err(ClientError::Handshake(format!(
                "unexpected guest response: {hello:?}"
            )));
        }

        let (requests, mut request_rx) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(async move {
            while let Some(message) = request_rx.recv().await {
                if write_frame(&mut stdin, &message).await.is_err() {
                    break;
                }
            }
        });
        let pending = Arc::new(Mutex::new(HashMap::<String, PendingResult>::new()));
        let reader_pending = Arc::clone(&pending);
        let cancel_acks = Arc::new(Mutex::new(HashMap::<String, CancelAck>::new()));
        let reader_cancel_acks = Arc::clone(&cancel_acks);
        let tool_acks = Arc::new(Mutex::new(HashMap::<String, ToolsAck>::new()));
        let reader_tool_acks = Arc::clone(&tool_acks);
        let reader_task = tokio::spawn(async move {
            loop {
                let message = match read_frame(&mut stdout, DEFAULT_MAX_FRAME_SIZE).await {
                    Ok(Some(message)) => message,
                    Ok(None) | Err(_) => break,
                };
                match message {
                    ServerMessage::Completed { result } => {
                        if let Some(sender) = reader_pending
                            .lock()
                            .expect("pending request map poisoned")
                            .remove(&result.execution_id)
                        {
                            let _ = sender.send(Ok(result));
                        }
                    }
                    ServerMessage::Failed {
                        execution_id: Some(execution_id),
                        error,
                    } => {
                        if let Some(sender) = reader_pending
                            .lock()
                            .expect("pending request map poisoned")
                            .remove(&execution_id)
                        {
                            let _ = sender.send(Err(ClientError::Remote {
                                code: error.code,
                                message: error.message,
                            }));
                        }
                    }
                    ServerMessage::CancelAcknowledged {
                        execution_id,
                        found,
                    } => {
                        if let Some(sender) = reader_cancel_acks
                            .lock()
                            .expect("cancel ack map poisoned")
                            .remove(&execution_id)
                        {
                            let _ = sender.send(found);
                        }
                    }
                    ServerMessage::Tools { request_id, tools } => {
                        if let Some(sender) = reader_tool_acks
                            .lock()
                            .expect("tool ack map poisoned")
                            .remove(&request_id)
                        {
                            let _ = sender.send(Ok(tools));
                        }
                    }
                    _ => {}
                }
            }

            for (_, sender) in reader_pending
                .lock()
                .expect("pending request map poisoned")
                .drain()
            {
                let _ = sender.send(Err(ClientError::Disconnected));
            }
            for (_, sender) in reader_cancel_acks
                .lock()
                .expect("cancel ack map poisoned")
                .drain()
            {
                // Dropping the sender makes the awaiting `cancel` see a closed
                // channel and report `Disconnected` instead of a spurious ack.
                drop(sender);
            }
            for (_, sender) in reader_tool_acks
                .lock()
                .expect("tool ack map poisoned")
                .drain()
            {
                let _ = sender.send(Err(ClientError::Disconnected));
            }
        });

        Ok(Self {
            requests,
            pending,
            cancel_acks,
            tool_acks,
            next_request_id: AtomicU64::new(1),
            child,
            writer_task,
            reader_task,
        })
    }
}

#[async_trait]
impl ExecutionClient for WslClient {
    async fn execute(
        &self,
        request: ExecutionRequest,
        timeout: Option<Duration>,
    ) -> Result<ExecutionResult, ClientError> {
        let execution_id = request.id.clone();
        let (result_tx, result_rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("pending request map poisoned");
            if pending.contains_key(&execution_id) {
                return Err(ClientError::DuplicateExecutionId(execution_id));
            }
            pending.insert(execution_id.clone(), result_tx);
        }

        if self
            .requests
            .send(ClientMessage::Execute {
                request,
                timeout_ms: timeout
                    .map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX)),
            })
            .is_err()
        {
            self.pending
                .lock()
                .expect("pending request map poisoned")
                .remove(&execution_id);
            return Err(ClientError::Disconnected);
        }

        result_rx.await.unwrap_or(Err(ClientError::Disconnected))
    }

    async fn cancel(&self, execution_id: &str) -> Result<bool, ClientError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        {
            let mut acks = self.cancel_acks.lock().expect("cancel ack map poisoned");
            acks.insert(execution_id.to_string(), ack_tx);

            if self
                .requests
                .send(ClientMessage::Cancel {
                    execution_id: execution_id.to_string(),
                })
                .is_err()
            {
                acks.remove(execution_id);
                return Err(ClientError::Disconnected);
            }
        }

        ack_rx.await.map_err(|_| ClientError::Disconnected)
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ClientError> {
        let request_id = format!(
            "tools-{}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        );
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tool_acks
            .lock()
            .expect("tool ack map poisoned")
            .insert(request_id.clone(), ack_tx);
        if self
            .requests
            .send(ClientMessage::ListTools {
                request_id: request_id.clone(),
            })
            .is_err()
        {
            self.tool_acks
                .lock()
                .expect("tool ack map poisoned")
                .remove(&request_id);
            return Err(ClientError::Disconnected);
        }
        ack_rx.await.unwrap_or(Err(ClientError::Disconnected))
    }
}

impl Drop for WslClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.writer_task.abort();
        self.reader_task.abort();
    }
}
