//! Byte-addressed block devices, digests, metrics, and kernel ublk host disk plumbing.
pub mod device;
pub mod digest;
pub mod layout;
pub mod metrics;
pub mod ublk;

pub use device::{Disk, DiskWrite, LocalDisk, export};
