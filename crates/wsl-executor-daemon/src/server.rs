use executor_core::{
    ExecutionEvent, ExecutionObserver, SubmissionControls, ToolDescriptor, ToolExecutor,
};
use executor_protocol::{
    ClientMessage, DEFAULT_MAX_FRAME_SIZE, FrameError, PROTOCOL_VERSION, ProtocolFailure,
    ServerMessage, read_frame, write_frame,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("response writer task failed: {0}")]
    WriterTask(String),
}

pub struct Session {
    pub executor: ToolExecutor,
    pub tools: Vec<ToolDescriptor>,
    pub workspace: PathBuf,
}

struct WireObserver {
    responses: mpsc::UnboundedSender<ServerMessage>,
}

impl ExecutionObserver for WireObserver {
    fn on_event(&self, event: ExecutionEvent) {
        let message = match event {
            ExecutionEvent::Started {
                execution_id,
                tool,
                detail,
            } => ServerMessage::Started {
                execution_id,
                tool,
                detail,
            },
            ExecutionEvent::Progress {
                execution_id,
                tool,
                message,
                data,
            } => ServerMessage::Progress {
                execution_id,
                tool,
                message,
                data,
            },
            ExecutionEvent::Failed {
                execution_id,
                error,
                ..
            } => ServerMessage::Failed {
                execution_id: Some(execution_id),
                error: ProtocolFailure::new(error.code(), error.to_string()),
            },
        };
        let _ = self.responses.send(message);
    }
}

/// Serves one host connection. Dropping the connection cancels every task
/// submitted through it and waits for their cleanup before returning.
pub async fn serve<R, W>(
    mut reader: R,
    mut writer: W,
    create_session: impl FnOnce(&str, &str) -> Result<Session, ProtocolFailure>,
    global_permits: Arc<Semaphore>,
) -> Result<(), DaemonError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (responses, mut response_rx) = mpsc::unbounded_channel::<ServerMessage>();
    let writer_task = tokio::spawn(async move {
        while let Some(message) = response_rx.recv().await {
            write_frame(&mut writer, &message).await?;
        }
        Ok::<_, FrameError>(())
    });
    let active = Arc::new(Mutex::new(HashMap::<String, CancellationToken>::new()));
    let mut executions = JoinSet::new();
    let mut session = None;
    let mut create_session = Some(create_session);

    while let Some(message) =
        read_frame::<_, ClientMessage>(&mut reader, DEFAULT_MAX_FRAME_SIZE).await?
    {
        if session.is_none() && !matches!(message, ClientMessage::Hello { .. }) {
            send_failure(
                &responses,
                None,
                "handshake_required",
                "hello must be the first client message",
            );
            continue;
        }

        match message {
            ClientMessage::Hello {
                protocol_version,
                token,
                workspace,
            } => {
                if session.is_some() {
                    send_failure(
                        &responses,
                        None,
                        "duplicate_hello",
                        "session is already established",
                    );
                } else if protocol_version != PROTOCOL_VERSION {
                    send_failure(
                        &responses,
                        None,
                        "unsupported_protocol",
                        format!(
                            "host requested protocol {protocol_version}, guest supports {PROTOCOL_VERSION}"
                        ),
                    );
                    break;
                } else {
                    let factory = create_session.take().expect("session factory available");
                    match factory(&token, &workspace) {
                        Ok(created) => {
                            let canonical_workspace = created.workspace.display().to_string();
                            session = Some(Session {
                                executor: created.executor.with_observer(Arc::new(WireObserver {
                                    responses: responses.clone(),
                                })),
                                ..created
                            });
                            let _ = responses.send(ServerMessage::HelloAck {
                                protocol_version: PROTOCOL_VERSION,
                                workspace: canonical_workspace,
                            });
                        }
                        Err(error) => {
                            let _ = responses.send(ServerMessage::Failed {
                                execution_id: None,
                                error,
                            });
                            break;
                        }
                    }
                }
            }
            ClientMessage::Execute {
                request,
                timeout_ms,
            } => {
                let executor = session
                    .as_ref()
                    .expect("handshake checked")
                    .executor
                    .clone();
                let execution_id = request.id.clone();
                let cancellation = CancellationToken::new();
                let inserted = {
                    let mut active = active.lock().expect("active task map poisoned");
                    if active.contains_key(&execution_id) {
                        false
                    } else {
                        active.insert(execution_id.clone(), cancellation.clone());
                        true
                    }
                };
                if !inserted {
                    send_failure(
                        &responses,
                        Some(execution_id),
                        "duplicate_execution_id",
                        "an execution with this id is already active",
                    );
                    continue;
                }

                let _ = responses.send(ServerMessage::Accepted {
                    execution_id: execution_id.clone(),
                });
                let responses = responses.clone();
                let active = Arc::clone(&active);
                let global_permits = Arc::clone(&global_permits);
                executions.spawn(async move {
                    let wait_cancellation = cancellation.clone();
                    let mut options = SubmissionControls::new().with_cancellation(cancellation);
                    if let Some(timeout_ms) = timeout_ms {
                        options = options.with_timeout(Duration::from_millis(timeout_ms));
                    }
                    let permit = tokio::select! {
                        permit = global_permits.acquire_owned() => permit.ok(),
                        _ = wait_cancellation.cancelled() => None,
                    };
                    let result = executor.execute(request, options).await;
                    drop(permit);
                    active
                        .lock()
                        .expect("active task map poisoned")
                        .remove(&execution_id);
                    let _ = responses.send(ServerMessage::Completed { result });
                });
            }
            ClientMessage::Cancel { execution_id } => {
                let cancellation = active
                    .lock()
                    .expect("active task map poisoned")
                    .get(&execution_id)
                    .cloned();
                let found = cancellation.is_some();
                if let Some(cancellation) = cancellation {
                    cancellation.cancel();
                }
                let _ = responses.send(ServerMessage::CancelAcknowledged {
                    execution_id,
                    found,
                });
            }
            ClientMessage::ListTools => {
                let _ = responses.send(ServerMessage::Tools {
                    tools: session.as_ref().expect("handshake checked").tools.clone(),
                });
            }
            ClientMessage::Shutdown => {
                let _ = responses.send(ServerMessage::ShutdownAck);
                break;
            }
        }
    }

    for cancellation in active.lock().expect("active task map poisoned").values() {
        cancellation.cancel();
    }
    while executions.join_next().await.is_some() {}
    drop(responses);

    writer_task
        .await
        .map_err(|error| DaemonError::WriterTask(error.to_string()))??;
    Ok(())
}

fn send_failure(
    responses: &mpsc::UnboundedSender<ServerMessage>,
    execution_id: Option<String>,
    code: impl Into<String>,
    message: impl Into<String>,
) {
    let _ = responses.send(ServerMessage::Failed {
        execution_id,
        error: ProtocolFailure::new(code, message),
    });
}
