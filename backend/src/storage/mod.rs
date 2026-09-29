//! Conversation disk storage. Callers hold the conversation lock while preparing or exporting.
mod local;
pub use local::prepare;
mod device;
pub use device::{Disk, LocalDisk, export};

mod lazy;
pub(crate) mod metrics;
pub use lazy::{BlockSource, LazyDisk};
pub mod fuse;

pub mod remote;

pub mod policy;

pub mod runtime;

pub mod checkpoint;

pub mod cache;

pub mod bootstrap;
