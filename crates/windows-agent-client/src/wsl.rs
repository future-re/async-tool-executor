use crate::{ClientError, ExecutionClient, WslClientConfig};
use async_trait::async_trait;
use executor_protocol::{
    ClientMessage, DEFAULT_MAX_FRAME_SIZE, ExecutionRequest, ExecutionResult, PROTOCOL_VERSION,
    ServerMessage, read_frame, write_frame,
};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

type PendingResult = oneshot::Sender<Result<ExecutionResult, ClientError>>;

pub struct WslClient {
    requests: mpsc::UnboundedSender<ClientMessage>,
    pending: Arc<Mutex<HashMap<String, PendingResult>>>,
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
        });

        Ok(Self {
            requests,
            pending,
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

    async fn cancel(&self, execution_id: &str) -> Result<(), ClientError> {
        self.requests
            .send(ClientMessage::Cancel {
                execution_id: execution_id.to_string(),
            })
            .map_err(|_| ClientError::Disconnected)
    }
}

impl Drop for WslClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.writer_task.abort();
        self.reader_task.abort();
    }
}
