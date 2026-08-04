use crate::{ExecutionError, Tool, ToolDefinition};
use jsonschema::Validator;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct RegisteredTool {
    pub(crate) implementation: Arc<dyn Tool>,
    pub(crate) definition: Arc<ToolDefinition>,
    pub(crate) validator: Option<Validator>,
}

impl RegisteredTool {
    /// Checks the arguments against the tool's declared input schema. Compiled
    /// once at registration time; a missing or invalid schema disables the check.
    pub(crate) fn validate_arguments(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<(), ExecutionError> {
        let Some(validator) = &self.validator else {
            return Ok(());
        };
        validator
            .validate(arguments)
            .map_err(|error| ExecutionError::ToolValidation {
                tool: self.definition.name.clone(),
                message: error.to_string(),
            })
    }
}

/// Immutable-at-execution registry of tool implementations and cached definitions.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: Tool>(&mut self, tool: T) {
        let tool: Arc<dyn Tool> = Arc::new(tool);
        let definition = Arc::new(tool.definition());
        let validator = jsonschema::validator_for(&definition.input_schema)
            .map_err(|error| {
                tracing::warn!(
                    tool = %definition.name,
                    "invalid input_schema, schema validation disabled: {error}"
                );
                error
            })
            .ok();
        self.tools.insert(
            definition.name.clone(),
            RegisteredTool {
                implementation: tool,
                definition,
                validator,
            },
        );
    }

    pub fn get(&self, name: &str) -> Result<Arc<dyn Tool>, ExecutionError> {
        Ok(self.resolve(name)?.implementation)
    }

    pub fn definition(&self, name: &str) -> Result<Arc<ToolDefinition>, ExecutionError> {
        Ok(self.resolve(name)?.definition)
    }

    pub(crate) fn resolve(&self, name: &str) -> Result<RegisteredTool, ExecutionError> {
        self.tools
            .get(name)
            .cloned()
            .ok_or_else(|| ExecutionError::ToolNotFound(name.to_string()))
    }
}
