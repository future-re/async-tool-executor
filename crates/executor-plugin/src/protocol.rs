use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PLUGIN_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginRequest {
    Hello {
        protocol_version: u32,
        package_id: String,
    },
    Invoke {
        invocation_id: String,
        tool: String,
        arguments: Value,
        cwd: String,
        timeout_ms: Option<u64>,
    },
    Cancel {
        invocation_id: String,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginResponse {
    HelloAck {
        protocol_version: u32,
    },
    Progress {
        invocation_id: String,
        message: String,
        data: Option<Value>,
    },
    Completed {
        invocation_id: String,
        content: Value,
    },
    Failed {
        invocation_id: String,
        code: String,
        message: String,
    },
    ShutdownAck,
}
