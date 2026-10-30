//! Conversation disk storage. Callers hold the conversation lock while preparing or exporting.
pub(crate) mod backend_conversion;
pub mod layout;
mod local;

pub use local::prepare;

mod device;

pub use device::{Disk, DiskWrite, LocalDisk, export};

pub mod block_codec;
pub mod digest;
pub mod direct;
mod lazy;
pub(crate) mod metrics;

pub(crate) use lazy::NodeBlockCache;
pub use lazy::{BlockSource, LazyDisk};

pub mod transport;

pub mod ublk;

pub(crate) mod read_scope;
pub mod remote;
mod reports;

pub mod source;

pub mod policy;

pub mod runtime;

pub mod checkpoint;

pub mod cache;

pub mod bootstrap;

pub mod environment;
