#[cfg(unix)]
use jsonschema::validator_for;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Component, Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub entrypoint: Vec<String>,
    #[serde(default)]
    pub required_commands: Vec<String>,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    pub tools: Vec<PluginToolManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginToolManifest {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(default)]
    pub concurrency_safe: bool,
}

const fn default_max_concurrency() -> usize {
    1
}

impl PluginManifest {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let manifest: Self = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported manifest schema {}",
                self.schema_version
            ));
        }
        if !valid_plugin_id(&self.id) {
            return Err("plugin id must be a dot-separated lowercase identifier".into());
        }
        Version::parse(&self.version).map_err(|error| format!("invalid version: {error}"))?;
        if self.entrypoint.is_empty() || self.entrypoint[0].is_empty() {
            return Err("entrypoint must not be empty".into());
        }
        if self.entrypoint.iter().any(|value| value.contains('\0')) {
            return Err("entrypoint contains NUL".into());
        }
        if self.required_commands.iter().any(String::is_empty) {
            return Err("required_commands must not contain empty names".into());
        }
        if self.max_concurrency == 0 {
            return Err("max_concurrency must be at least 1".into());
        }
        if self.tools.is_empty() {
            return Err("manifest must expose at least one tool".into());
        }
        let mut names = HashSet::new();
        for tool in &self.tools {
            if !valid_tool_name(&tool.name) {
                return Err(format!("invalid tool name `{}`", tool.name));
            }
            if !names.insert(&tool.name) {
                return Err(format!("duplicate tool name `{}`", tool.name));
            }
            #[cfg(unix)]
            validator_for(&tool.input_schema)
                .map_err(|error| format!("invalid schema for `{}`: {error}", tool.name))?;
            #[cfg(not(unix))]
            if !tool.input_schema.is_object() {
                return Err(format!(
                    "input schema for `{}` must be an object",
                    tool.name
                ));
            }
        }
        Ok(())
    }

    pub fn entrypoint_is_local(&self) -> bool {
        let path = Path::new(&self.entrypoint[0]);
        self.entrypoint[0].contains('/')
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
    }
}

fn valid_plugin_id(value: &str) -> bool {
    value.len() <= 128
        && value.split('.').count() >= 2
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

fn valid_tool_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_duplicate_tools() {
        let tool = PluginToolManifest {
            name: "same".into(),
            description: "a".into(),
            input_schema: json!({}),
            concurrency_safe: false,
        };
        let manifest = PluginManifest {
            schema_version: 1,
            id: "com.example.test".into(),
            version: "1.0.0".into(),
            entrypoint: vec!["bin/run".into()],
            required_commands: vec![],
            max_concurrency: 1,
            tools: vec![tool.clone(), tool],
        };
        assert!(manifest.validate().unwrap_err().contains("duplicate"));
    }
}
