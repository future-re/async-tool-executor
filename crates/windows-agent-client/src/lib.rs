//! Windows-side client API for forwarding tool requests into WSL.

mod api;
#[cfg(windows)]
mod wsl;

pub use api::{ClientError, ExecutionClient, WslClientConfig};
#[cfg(windows)]
pub use wsl::WslClient;
