//! Firecracker execution: persistent guest disks, ephemeral jailed attempts.
pub mod budget;
mod codex_state;
pub mod guest;
pub mod host;
pub(crate) mod images;
mod listener;
pub mod mcp;
mod network;
pub mod plan;
pub mod pool;
pub mod projects;
pub mod protocol;
pub mod vm;
pub mod wire;
