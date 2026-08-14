//! Async tool execution with bounded concurrency, deadlines, cancellation,
//! panic isolation, deterministic result ordering, and optional observations.
//!
//! # Entry point
//!
//! [`ToolExecutor`] is the primary API: register tools via [`ToolRegistry`],
//! configure the process-wide defaults with [`ExecutorConfig`], then submit
//! requests through [`execute`](ToolExecutor::execute) or
//! [`execute_all`](ToolExecutor::execute_all).

mod config;
mod error;
mod executor;
mod invocation;
mod observer;
mod registry;
mod scheduler;
mod tool;

pub use config::{ExecutorConfig, SubmissionControls};
pub use error::ExecutionError;
pub use executor::ToolExecutor;
pub use executor_protocol::{ExecutionRequest, ExecutionResult, ToolDescriptor};
pub use observer::{ExecutionEvent, ExecutionObserver, ProgressReporter};
pub use registry::ToolRegistry;
pub use tool::{Tool, ToolContext, ToolDefinition, ToolOutput};
