//! Jobs belong to an execution environment, independently of its provider turns.
//!
//! The environment owns each helper and its private control socket. The helper
//! adopts orphaned descendants and only reports completion after reaping them.
//! Workload stdout/stderr can never supply a completion receipt.
pub mod channel;
pub mod controller;
pub mod environment;
mod process;
pub mod protocol;
mod registry;

pub use process::{Receipt, Request, helper};
pub use registry::{Outcome, Registry};

/// Monotonic deadline clock shared with the environment entrypoint.
pub fn boot_ms() -> std::io::Result<u64> {
    process::boot_ms()
}
