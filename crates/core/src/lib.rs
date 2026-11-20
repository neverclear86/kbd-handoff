//! Shared code for kbd-handoff: wire protocol, encrypted transport,
//! configuration helpers, input abstractions and the evdev-to-Windows keymap.

pub mod config;
pub mod crypto;
pub mod input;
pub mod keymap;
pub mod proto;

/// Default TCP port the Linux sender listens on.
pub const DEFAULT_PORT: u16 = 47820;
/// Interval at which each side sends `Msg::Ping` when it has nothing else to send.
pub const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);
/// A connection is considered dead when no frame arrives within this time.
pub const HEARTBEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(3000);
/// Upper bound for the Noise handshake.
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
