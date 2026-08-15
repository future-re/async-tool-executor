//! WSL-side protocol server and execution task lifecycle management.

pub mod config;
mod server;

pub use server::{DaemonError, serve};
