//! Conversation disk storage. Callers hold the conversation lock while preparing or exporting.
pub mod layout;
mod local;

pub use local::prepare;

mod device;

pub use device::{Disk, DiskWrite, LocalDisk, export};

mod view;

pub use view::DiskView;

pub mod digest;
mod lazy;
pub(crate) mod metrics;

pub(crate) use lazy::NodeBlockCache;
pub use lazy::{BlockSource, LazyDisk};

pub mod fuse;

pub mod vhost;

pub mod transport;

#[cfg(feature = "ublk")]
pub mod ublk;

pub mod remote;

pub mod source;

pub mod policy;

pub mod runtime;

pub mod checkpoint;

pub mod cache;

pub mod bootstrap;

pub mod environment;
