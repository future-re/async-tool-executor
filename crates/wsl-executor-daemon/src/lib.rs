//! WSL-side protocol server and execution task lifecycle management.

pub mod config;
#[cfg(unix)]
pub mod daemon;
mod server;

pub use server::{DaemonError, Session, serve};
