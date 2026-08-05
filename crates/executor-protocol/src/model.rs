use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A fully authorized request ready for execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionRequest {
    pub id: String,
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
}

/// The terminal outcome of one execution request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionResult {
    pub execution_id: String,
    pub tool: String,
    pub content: Value,
    pub is_error: bool,
}

/// Serializable tool metadata returned during capability discovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}
