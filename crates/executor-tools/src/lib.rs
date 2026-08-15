//! Platform-neutral base tools built on [`executor_core`].
//!
//! This crate ports the core filesystem, search and web tools from
//! `telos-agent`'s built-in set so a fresh [`ToolRegistry`] starts with a
//! useful toolbox instead of nothing:
//!
//! | Tool | Type | Purpose |
//! | --- | --- | --- |
//! | `Read`   | [`FileReadTool`]  | Read a UTF-8 text file (records state for the write guard) |
//! | `Write`  | [`FileWriteTool`] | Create/overwrite a UTF-8 text file |
//! | `Edit`   | [`FileEditTool`]  | Unambiguous exact-match replace |
//! | `Glob`   | [`GlobTool`]      | List files matching a pattern |
//! | `Grep`   | [`GrepTool`]      | Literal substring search over files |
//! | `WebFetch` | [`WebFetchTool`] | Fetch a URL and convert to text |
//!
//! # Registering tools
//!
//! Registering a tool anywhere in this workspace is a two-step process:
//!
//! 1. Implement [`executor_core::Tool`] (at minimum [`definition`](Tool::definition)
//!    and [`invoke`](Tool::invoke)).
//! 2. Call [`ToolRegistry::register`] before building the [`ToolExecutor`].
//!
//! The simplest way to get the whole built-in set is
//! [`register_core_tools`]:
//!
//! ```no_run
//! use executor_core::ToolRegistry;
//! use executor_tools::register_core_tools;
//!
//! let mut registry = ToolRegistry::new();
//! register_core_tools(&mut registry);
//! let tools = registry.list(); // six descriptors, ready for discovery
//! ```
//!
//! The three mutating file tools share one [`FileReadState`], which enforces
//! the read-before-mutate invariant across a session. Register the file tools
//! yourself if you need a different state layout or read-size ceiling.

mod file_edit;
mod file_read;
mod file_write;
mod glob;
mod grep;
mod shared;
mod web_fetch;

pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use shared::{
    DEFAULT_MAX_FILE_READ_BYTES, FileReadRecord, FileReadState, canonicalize_within_cwd,
    resolve_workspace_path,
};
pub use web_fetch::WebFetchTool;

use executor_core::ToolRegistry;

/// Register every built-in tool with the executor's [`ToolRegistry`].
///
/// The `Read`/`Write`/`Edit` tools share one file-read state so the write
/// guard works across the whole session.
pub fn register_core_tools(registry: &mut ToolRegistry) {
    let read_file_state = FileReadState::default();
    registry.register(FileReadTool::new(read_file_state.clone()));
    registry.register(FileWriteTool::new(read_file_state.clone()));
    registry.register(FileEditTool::new(read_file_state.clone()));
    registry.register(GlobTool);
    registry.register(GrepTool::default());
    registry.register(WebFetchTool::new());
}

/// Test helpers shared by the built-in tool unit tests.
#[cfg(test)]
pub(crate) mod test_support {
    use executor_core::{ProgressReporter, ToolContext};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    pub(crate) fn context(cwd: PathBuf) -> ToolContext {
        ToolContext {
            execution_id: "test".into(),
            cwd,
            env: Arc::new(HashMap::new()),
            deadline: None,
            cancellation: CancellationToken::new(),
            progress: ProgressReporter::default(),
        }
    }
}
