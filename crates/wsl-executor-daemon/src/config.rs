use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use wsl_runtime::ResourceLimits;

/// Parsed configuration for the guest daemon. Every field is optional so a
/// config file only needs to override what differs from the built-in defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// Working directory for executed commands and the executor.
    pub cwd: Option<PathBuf>,
    /// Maximum concurrent executions.
    pub concurrency_limit: Option<usize>,
    /// Hard timeout applied to every execution, in milliseconds.
    pub tool_timeout_ms: Option<u64>,
    /// Whether commands inherit the parent process environment.
    pub inherit_env: Option<bool>,
    /// Base environment applied to every command.
    pub base_env: Option<HashMap<String, String>>,
    /// Sandbox resource ceilings for executed commands.
    pub limits: Option<ResourceLimits>,
    /// Whether the shell tool requires the exclusive execution slot.
    pub exclusive: Option<bool>,
}

/// Resolves the configuration: `--config` argument, then `ATE_CONFIG`, then
/// `~/.config/ate/config.json`. A missing file falls back to defaults; a file
/// that exists but fails to parse is a hard error so misconfiguration is loud.
pub fn load() -> Result<DaemonConfig, ConfigError> {
    let path = match find_path()? {
        Some(path) => path,
        None => return Ok(DaemonConfig::default()),
    };
    let raw =
        std::fs::read_to_string(&path).map_err(|error| ConfigError::Read(path.clone(), error))?;
    let config = serde_json::from_str(&raw).map_err(ConfigError::Parse)?;
    Ok(config)
}

fn find_path() -> Result<Option<PathBuf>, ConfigError> {
    if let Some(value) = std::env::args().next() {
        let mut args = std::env::args();
        let program = args.next().unwrap_or(value);
        let mut explicit = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" => {
                    explicit = Some(args.next().ok_or_else(|| {
                        ConfigError::Usage(format!("{program}: --config requires a path"))
                    })?);
                }
                value if value.starts_with("--config=") => {
                    explicit = Some(value.trim_start_matches("--config=").to_string());
                }
                _ => {}
            }
        }
        if let Some(path) = explicit {
            return Ok(Some(PathBuf::from(path)));
        }
    }
    if let Some(value) = std::env::var_os("ATE_CONFIG") {
        return Ok(Some(PathBuf::from(value)));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = home {
        let default_path = home.join(".config/ate/config.json");
        if default_path.exists() {
            return Ok(Some(default_path));
        }
    }
    Ok(None)
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid invocation: {0}")]
    Usage(String),
    #[error("cannot read config file {0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("invalid config: {0}")]
    Parse(serde_json::Error),
}
