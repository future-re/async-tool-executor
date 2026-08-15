use crate::{ExecutionRequest, ExecutionResult, ToolDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 2;

/// Messages sent from the Windows agent to the WSL executor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        protocol_version: u32,
        token: String,
        workspace: String,
    },
    Execute {
        request: ExecutionRequest,
        timeout_ms: Option<u64>,
    },
    Cancel {
        execution_id: String,
    },
    ListTools {
        request_id: String,
    },
    Shutdown,
}

/// Stable failure representation used across the process boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolFailure {
    pub code: String,
    pub message: String,
}

impl ProtocolFailure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Messages sent from the WSL executor to the Windows agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    HelloAck {
        protocol_version: u32,
        workspace: String,
    },
    Accepted {
        execution_id: String,
    },
    Started {
        execution_id: String,
        tool: String,
        detail: String,
    },
    Progress {
        execution_id: String,
        tool: String,
        message: String,
        data: Option<Value>,
    },
    Completed {
        result: ExecutionResult,
    },
    CancelAcknowledged {
        execution_id: String,
        found: bool,
    },
    Tools {
        request_id: String,
        tools: Vec<ToolDescriptor>,
    },
    Failed {
        execution_id: Option<String>,
        error: ProtocolFailure,
    },
    ShutdownAck,
}
