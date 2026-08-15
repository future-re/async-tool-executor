//! Shared helpers used by the built-in tool implementations.

use executor_core::ExecutionError;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Default ceiling on bytes the file tools will read from a single file.
pub const DEFAULT_MAX_FILE_READ_BYTES: usize = 50 * 1024 * 1024;

/// Metadata captured when a file is read through the built-in `Read` tool.
///
/// Mutating file tools use this to reject stale writes: if the file changed
/// after the model read it, the model must read it again before editing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileReadRecord {
    pub content: String,
    pub timestamp_ms: u128,
    pub is_partial_view: bool,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

/// Shared per-executor file-read cache.
pub type FileReadState = Arc<Mutex<HashMap<PathBuf, FileReadRecord>>>;

/// Extract a required string field from JSON arguments or return a validation error.
pub fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, ExecutionError> {
    arguments
        .get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| ExecutionError::ToolValidation {
            tool: String::new(),
            message: format!("missing string `{key}`"),
        })
}

/// Extract the first available string field from JSON arguments.
pub fn required_string_any<'a>(
    arguments: &'a Value,
    keys: &[&str],
) -> Result<&'a str, ExecutionError> {
    for key in keys {
        if let Some(value) = arguments.get(*key).and_then(|value| value.as_str()) {
            return Ok(value);
        }
    }
    Err(ExecutionError::ToolValidation {
        tool: String::new(),
        message: format!("missing string `{}`", keys.join("` or `")),
    })
}

/// Extract an optional bool argument with a default.
pub fn optional_bool(arguments: &Value, key: &str, default: bool) -> bool {
    arguments
        .get(key)
        .and_then(|value| value.as_bool())
        .unwrap_or(default)
}

/// Extract an optional positive integer from any one of several keys.
pub fn optional_usize_any(arguments: &Value, keys: &[&str]) -> Option<usize> {
    keys.iter().find_map(|key| {
        arguments
            .get(*key)
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
    })
}

/// Resolve a user-supplied path against the workspace cwd, refusing to escape it.
///
/// Absolute paths are taken as-is; relative paths are joined onto `cwd`. We
/// normalise `.` / `..` and then assert the result still lies inside `cwd` —
/// this is the only line of defence against path-traversal attacks via the
/// filesystem tools.
pub fn resolve_workspace_path(cwd: &Path, path: &str) -> Result<PathBuf, ExecutionError> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    let normalized = normalize_path(&candidate);
    let normalized_cwd = normalize_path(cwd);
    if !normalized.starts_with(&normalized_cwd) {
        return Err(execution_error(format!(
            "path escapes cwd: {}",
            candidate.display()
        )));
    }
    Ok(normalized)
}

/// Lexically resolve `.` and `..` without touching the filesystem.
///
/// We deliberately don't follow symlinks — that would require I/O and could
/// race with the file being written. The trade-off is that a symlink pointing
/// outside `cwd` will slip through; tools that care should check separately.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Format `path` relative to `cwd` for display, falling back to the absolute path on failure.
pub fn display_relative(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

/// Resolve a path against `cwd` and follow symlinks, verifying the final
/// canonical location still lies inside `cwd`.
///
/// This is the second line of defence against path traversal via symlinks:
/// [`resolve_workspace_path`] only normalises `.`/`..` lexically, so a symlink
/// inside `cwd` that points outside will slip through unless we canonicalise.
///
/// For paths that do not exist yet, the function walks up the tree until it
/// finds an existing ancestor, canonicalises that ancestor, and then joins the
/// remaining suffix back on. The suffix is checked lexically against
/// `canonical_cwd` so that a symlinked ancestor pointing outside `cwd` is still
/// rejected.
pub async fn canonicalize_within_cwd(cwd: &Path, path: &Path) -> Result<PathBuf, ExecutionError> {
    let canonical_cwd = tokio::fs::canonicalize(cwd)
        .await
        .map_err(|err| execution_error(format!("failed to canonicalize cwd: {err}")))?;

    // Fast path: the target already exists.
    if let Ok(canonical_path) = tokio::fs::canonicalize(path).await {
        return check_cwd_prefix(canonical_path, &canonical_cwd, path);
    }

    // Slow path: walk up until we find an existing ancestor. This handles new
    // nested files (e.g. `src/new_dir/new_file.rs`) and reduces the symlink
    // race window to the existing-ancestor check.
    let mut existing_ancestor = path;
    let mut suffix = PathBuf::new();
    loop {
        if let Some(parent) = existing_ancestor.parent() {
            if let Some(name) = existing_ancestor.file_name() {
                suffix = if suffix.as_os_str().is_empty() {
                    PathBuf::from(name)
                } else {
                    PathBuf::from(name).join(&suffix)
                };
            }
            existing_ancestor = parent;
            if existing_ancestor.as_os_str().is_empty() {
                existing_ancestor = cwd;
                break;
            }
            if tokio::fs::metadata(existing_ancestor).await.is_ok() {
                break;
            }
        } else {
            existing_ancestor = cwd;
            break;
        }
    }

    let canonical_ancestor = tokio::fs::canonicalize(existing_ancestor)
        .await
        .map_err(|err| {
            execution_error(format!("failed to canonicalize parent directory: {err}"))
        })?;

    let canonical_path = if suffix.as_os_str().is_empty() {
        canonical_ancestor
    } else {
        canonical_ancestor.join(&suffix)
    };

    check_cwd_prefix(canonical_path, &canonical_cwd, path)
}

fn check_cwd_prefix(
    canonical_path: PathBuf,
    canonical_cwd: &Path,
    original: &Path,
) -> Result<PathBuf, ExecutionError> {
    if !canonical_path.starts_with(canonical_cwd) {
        return Err(execution_error(format!(
            "path escapes cwd after following symlinks: {}",
            original.display()
        )));
    }
    Ok(canonical_path)
}

/// Return a comparable millisecond timestamp for a file's last modification time.
pub async fn modified_timestamp_ms(path: &Path) -> Result<u128, ExecutionError> {
    let metadata = tokio::fs::metadata(path).await.map_err(|err| {
        execution_error(format!(
            "failed to read metadata for {}: {err}",
            path.display()
        ))
    })?;
    metadata
        .modified()
        .map_err(|err| execution_error(err.to_string()))?
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|err| execution_error(err.to_string()))
}

/// Reject writes to files that haven't been read or have changed since being read.
///
/// Shared by [`FileWriteTool`](crate::FileWriteTool) and
/// [`FileEditTool`](crate::FileEditTool) — both enforce the same
/// read-before-mutate invariant. `tool_name` is used in error messages so the
/// model knows which tool's guard was triggered.
pub async fn ensure_file_was_read_and_unchanged(
    tool_name: &str,
    state: &FileReadState,
    path: &Path,
    current_content: &str,
) -> Result<(), ExecutionError> {
    let last_read = state
        .lock()
        .expect("read-file-state lock poisoned")
        .get(path)
        .cloned();
    let Some(last_read) = last_read else {
        return Err(execution_error(format!(
            "{tool_name}: File has not been read yet. Read it first before writing to it."
        )));
    };
    if last_read.is_partial_view {
        return Err(execution_error(format!(
            "{tool_name}: File has only been partially read. Read the full file before writing to it."
        )));
    }
    if current_content != last_read.content {
        return Err(execution_error(format!(
            "{tool_name}: File has been modified since read, either by the user or by a linter. Read it again before attempting to write it."
        )));
    }
    Ok(())
}

/// Attach a tool name to an execution error for the built-in tools.
pub fn execution_error(message: impl Into<String>) -> ExecutionError {
    ExecutionError::ToolExecution {
        tool: "filesystem".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_path_resolves_dots() {
        let p = normalize_path(Path::new("/a/b/../c/./d"));
        assert_eq!(p, PathBuf::from("/a/c/d"));
    }

    #[test]
    fn resolve_workspace_rejects_escape() {
        let cwd = Path::new("/workspace");
        let err = resolve_workspace_path(cwd, "../etc/passwd").unwrap_err();
        assert!(matches!(err, ExecutionError::ToolExecution { .. }));
    }

    #[test]
    fn resolve_workspace_accepts_path_under_cwd() {
        let cwd = Path::new("/workspace");
        let resolved = resolve_workspace_path(cwd, "src/main.rs").unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/src/main.rs"));
    }

    #[test]
    fn required_string_extracts_successfully() {
        let args = json!({"file_path": "/tmp/test.txt"});
        assert_eq!(
            required_string(&args, "file_path").unwrap(),
            "/tmp/test.txt"
        );
    }

    #[test]
    fn required_string_errors_on_missing_key() {
        let args = json!({"other": "value"});
        assert!(required_string(&args, "file_path").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonicalize_rejects_symlink_ancestor_outside_cwd() {
        let dir = std::env::temp_dir().join("ate_canonicalize_symlink");
        let outside = std::env::temp_dir().join("ate_canonicalize_outside");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        std::os::unix::fs::symlink(&outside, dir.join("escape")).unwrap();

        let result = canonicalize_within_cwd(&dir, &dir.join("escape/new.txt")).await;
        assert!(
            matches!(result, Err(ExecutionError::ToolExecution { .. })),
            "expected ToolExecution for symlink escape, got {result:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
