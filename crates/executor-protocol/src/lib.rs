//! Platform-neutral messages exchanged by the Windows host and WSL guest.

mod framing;
mod message;
mod model;

pub use framing::{DEFAULT_MAX_FRAME_SIZE, FrameError, read_frame, write_frame};
pub use message::{ClientMessage, PROTOCOL_VERSION, ProtocolFailure, ServerMessage};
pub use model::{ExecutionRequest, ExecutionResult, ToolDescriptor};
