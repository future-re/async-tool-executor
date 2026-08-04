use crate::support::{config, request};
use async_tool_executor::{
    ExecutionError, ExecutionOptions, ExecutionRequest, Tool, ToolContext, ToolDefinition,
    ToolExecutor, ToolOutput, ToolRegistry,
};
use async_trait::async_trait;
use serde_json::{Value, json};

struct StrictTool;

#[async_trait]
impl Tool for StrictTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "strict".into(),
            description: "requires a numeric `amount` and rejects extras".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "amount": { "type": "number", "minimum": 0 }
                },
                "required": ["amount"],
                "additionalProperties": false,
            }),
        }
    }

    async fn invoke(
        &self,
        arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ExecutionError> {
        Ok(ToolOutput::json(arguments))
    }
}

#[tokio::test]
async fn arguments_matching_the_schema_pass() {
    let mut tools = ToolRegistry::new();
    tools.register(StrictTool);
    let executor = ToolExecutor::new(tools, config(1));

    let result = executor
        .execute(
            ExecutionRequest {
                arguments: json!({"amount": 10}),
                ..request("v1", "strict")
            },
            ExecutionOptions::new(),
        )
        .await;
    assert!(!result.is_error);
    assert_eq!(result.content, json!({"amount": 10}));
}

#[tokio::test]
async fn arguments_violating_the_schema_fail_without_invoking_the_tool() {
    let mut tools = ToolRegistry::new();
    tools.register(StrictTool);
    let executor = ToolExecutor::new(tools, config(1));

    for arguments in [json!({"amount": -1}), json!({}), json!({"amount": "ten"})] {
        let result = executor
            .execute(
                ExecutionRequest {
                    arguments,
                    ..request("bad", "strict")
                },
                ExecutionOptions::new(),
            )
            .await;
        assert!(result.is_error);
        assert_eq!(result.content["error"]["kind"], "validation_error");
    }
}
