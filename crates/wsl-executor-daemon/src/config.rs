use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use wsl_runtime::ResourceLimits;

/// Parsed configuration for the guest daemon. Every field is optional so a
/// config file only needs to override what differs from the built-in defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    /// Parent directory containing every workspace accepted by this daemon.
    #[serde(alias = "cwd")]
    pub workspace_root: Option<PathBuf>,
    /// Maximum concurrent executions in one client session.
    pub concurrency_limit: Option<usize>,
    /// Maximum concurrent executions across all client sessions.
    pub global_concurrency_limit: Option<usize>,
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
    /// Whether installed external plugins are discovered for new sessions.
    pub plugins_enabled: Option<bool>,
    /// Plugin installation root. Defaults to ~/.local/share/ate/plugins.
    pub plugin_dir: Option<PathBuf>,
    /// Maximum time allowed for a plugin handshake.
    pub plugin_start_timeout_ms: Option<u64>,
    /// Grace period after cancellation before the plugin process is killed.
    pub plugin_cancel_grace_ms: Option<u64>,
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
    if let Some(home) = home_dir() {
        let default_path = home.join(".config/ate/config.json");
        if default_path.exists() {
            return Ok(Some(default_path));
        }
    }
    Ok(None)
}

/// Resolves the user's home directory. `HOME` is preferred when it is an
/// absolute path. `wsl.exe --exec` inherits a mangled Windows profile value
/// (e.g. `C:Usersfutur`) for `HOME`, so a non-absolute `HOME` is ignored in
/// favour of the passwd entry, which always yields a real Linux home.
pub fn home_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home {
        Some(path) if path.is_absolute() => Some(path),
        _ => unix_passwd_home().or(home),
    }
}

pub fn default_plugin_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".local/share/ate/plugins"))
}

/// Queries the passwd database for the current user's home directory via
/// libc `getpwuid_r(3)`.
#[cfg(unix)]
fn unix_passwd_home() -> Option<PathBuf> {
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut buf = vec![0u8; 4096];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            libc::geteuid(),
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        );
        if rc != 0 || result.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr(pwd.pw_dir);
        Some(PathBuf::from(dir.to_string_lossy().into_owned()))
    }
}

#[cfg(not(unix))]
fn unix_passwd_home() -> Option<PathBuf> {
    None
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
