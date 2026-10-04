//! This is Command Center's wire protocol between a Frame and a host (docs/pairing.md, docs/agent.md),
//! both halves, in Rust. I checked it against the Python version, home/pair.py and home/agent.py, but
//! that's gone now, so cc-host's tests/conformance.rs and tests/pair.rs are the spec (docs/rust-host.md).

pub mod agent;
pub mod conf;
pub mod imu;
pub mod lan;
pub mod pair;
pub mod server;
pub use spake2;
