//! Firecracker execution: persistent guest disks, ephemeral jailed attempts.
pub mod budget;
mod codex_state;
mod entrypoint;
pub mod guest;
pub mod hibernation;
pub mod host;
pub(crate) mod images;
pub(crate) mod listener;
pub mod mcp;
mod network;
pub mod plan;
pub mod pool;
pub mod projects;
pub mod protocol;
pub mod suspension;
pub mod vm;
pub mod wire;
