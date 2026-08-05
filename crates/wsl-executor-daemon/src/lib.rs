//! WSL-side protocol server and execution task lifecycle management.

mod server;

pub use server::{DaemonError, serve};
