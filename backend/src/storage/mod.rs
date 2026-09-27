//! Conversation disk storage. Callers hold the conversation lock while preparing or exporting.
mod local;
pub use local::{copy_blocks, prepare};
mod device;
pub use device::{Disk, LocalDisk, export};
