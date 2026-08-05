//! Linux/WSL-specific tool implementations and process isolation primitives.

#[cfg(unix)]
mod native;
mod shell;

#[cfg(unix)]
pub use native::{NativeShell, NativeShellConfig};
pub use shell::{CommandOutput, CommandSpec, ResourceLimits, Shell, ShellTool};
