//! Platform-independent core of MouseTail.

pub mod audio;
pub mod config;
pub mod controller;
pub mod discovery;
pub mod identity;
pub mod keys;
pub mod layout;
pub mod net;
pub mod pairing;
pub mod proto;
pub mod update;

/// Re-exported so front ends use the same QUIC version.
pub use quinn;
