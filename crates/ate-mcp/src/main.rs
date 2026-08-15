//! MCP stdio server exposing the WSL executor daemon's tools to any MCP client
//! (e.g. opencode). It speaks JSON-RPC 2.0 over newline-delimited messages on
//! stdin/stdout and forwards `tools/call` to the daemon over the existing
//! `windows-agent-client` TCP transport.
//!
//! Configuration mirrors `WslClientConfig` and is read from environment
//! variables so the server can be launched from an `opencode.json` MCP entry:
//!   ATE_MCP_DISTRIBUTION (default: Ubuntu)
//!   ATE_MCP_GUEST_PROGRAM (default: /home/<user>/...)
//!   ATE_MCP_WORKSPACE (required)

use executor_protocol::{ExecutionRequest, ToolDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::BufRead;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use windows_agent_client::{ClientError, ExecutionClient, WslClient, WslClientConfig};

const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Deserialize)]
struct RpcMessage {
    #[serde(rename = "id")]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

fn ok(id: Value, result: Value) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn err(id: Value, code: i64, message: impl Into<String>) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(json!({ "code": code, "message": message.into() })),
    }
}

struct Session {
    client: WslClient,
    next_id: AtomicU64,
}

impl Session {
    async fn tools_list(&self) -> Result<Value, ClientError> {
        let tools = self.client.list_tools().await?;
        Ok(json!({
            "tools": tools.iter().map(to_mcp_tool).collect::<Vec<_>>()
        }))
    }

    async fn tools_call(&self, name: &str, arguments: Value) -> Result<Value, ClientError> {
        let execution_id = format!("mcp-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let result = self
            .client
            .execute(
                ExecutionRequest {
                    id: execution_id,
                    tool: name.to_string(),
                    arguments,
                },
                None,
            )
            .await?;
        Ok(json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&result.content).unwrap_or_else(|_| {
                    "failed to serialize tool output".to_string()
                }),
            }],
            "isError": result.is_error,
        }))
    }
}

fn to_mcp_tool(tool: &ToolDescriptor) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": tool.input_schema,
    })
}

async fn handle(session: &Session, message: RpcMessage) -> Option<RpcResponse> {
    let Some(id) = message.id else {
        // Notifications are not answered.
        return None;
    };
    let result = match message.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "ate-mcp", "version": env!("CARGO_PKG_VERSION") },
        })),
        "notifications/initialized" | "notifications/cancelled" => Ok(Value::Null),
        "ping" => Ok(json!({})),
        "tools/list" => session.tools_list().await.map_err(failure_to_string),
        "tools/call" => {
            let name = message
                .params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "tools/call requires a `name`".to_string());
            let arguments = message
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match name {
                Ok(name) => session
                    .tools_call(name, arguments)
                    .await
                    .map_err(failure_to_string),
                Err(error) => Err(error),
            }
        }
        other => Err(format!("method not found: {other}")),
    };
    match result {
        Ok(value) if value.is_null() => None,
        Ok(value) => Some(ok(id, value)),
        Err(error) => Some(err(id, -32603, error)),
    }
}

fn failure_to_string(error: ClientError) -> String {
    error.to_string()
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("ate-mcp: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let distribution = std::env::var("ATE_MCP_DISTRIBUTION").unwrap_or_else(|_| "Ubuntu".into());
    let guest_program = std::env::var("ATE_MCP_GUEST_PROGRAM")?;
    let workspace = std::env::var("ATE_MCP_WORKSPACE")?;
    let config = WslClientConfig::new(distribution, guest_program, workspace);

    // Start reading stdin before connecting. On Windows, tokio's stdin handle
    // is not initialized correctly when standard input is a pipe; if the
    // daemon bootstrap (`wsl.exe`) is spawned first it grabs the pipe and all
    // later reads surface as an immediate EOF. Establishing the handle here,
    // before any child process exists, sidesteps the race. The standard
    // library stream is used for the same reason.
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(64);
    tokio::task::spawn_blocking(move || {
        for line in std::io::stdin().lock().lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => break,
            };
            if line_tx.blocking_send(line).is_err() {
                break;
            }
        }
    });

    let client = WslClient::connect(config).await?;
    let session = Arc::new(Session {
        client,
        next_id: AtomicU64::new(1),
    });

    let mut stdout = tokio::io::stdout();
    while let Some(line) = line_rx.recv().await {
        if line.trim().is_empty() {
            continue;
        }
        let message: RpcMessage = serde_json::from_str(&line)?;
        let session = Arc::clone(&session);
        let response = handle(&session, message).await;
        if let Some(response) = response {
            let mut payload = serde_json::to_vec(&response)?;
            payload.push(b'\n');
            stdout.write_all(&payload).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}
