pub mod codec;
pub mod msg;

pub use codec::{FrameCodec, ProtoError};
pub use msg::*;

/// Bumped on any breaking change to `msg`. Peers refuse to talk across versions.
pub const PROTOCOL_VERSION: u16 = 1;

pub const DEFAULT_PORT: u16 = 7070;
