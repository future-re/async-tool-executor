//! Async tool execution with bounded concurrency, deadlines, cancellation,
//! panic isolation, deterministic result ordering, and optional observations.

mod config;
mod diagnostics;
mod error;
mod executor;
mod invocation;
mod observer;
mod registry;
mod scheduler;
mod tool;

pub use config::ExecutorConfig;
pub use diagnostics::{DiagnosticsSink, ExecutionFailure, FailureKind};
pub use error::ExecutionError;
pub use executor::{ExecutionOptions, ToolExecutor};
pub use executor_protocol::{ExecutionRequest, ExecutionResult, ToolDescriptor};
pub use observer::{ExecutionEvent, ExecutionObserver, ProgressReporter};
pub use registry::ToolRegistry;
pub use tool::{Tool, ToolContext, ToolDefinition, ToolOutput};
