//! Node-wide disk limits. Dirty journals never participate in cache eviction.
use crate::error::{Error, Result};
use std::path::Path;

pub use cairn_contracts::NodeStoragePolicy as Policy;

pub fn for_node(value: &serde_json::Value) -> Result<Policy> {
    let value = if value.is_object() {
        value.clone()
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };
    let policy: Policy =
        serde_json::from_value(value).map_err(|_| Error::bad("Invalid node storage limits."))?;
    policy.validate().map_err(Error::bad)?;
    Ok(policy)
}

pub fn space(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    // Sample pending writes before physical/quota usage. A write that finishes
    // during the query stays conservatively charged in this result. Both cache
    // fills and new writes use this same headroom calculation under admission.
    let pending = super::cache::pending_write_bytes();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    let (total, free) = crate::microvm::budget::disk_space(
        path,
        stat.f_blocks.saturating_mul(stat.f_frsize),
        stat.f_bavail.saturating_mul(stat.f_frsize),
    )?;
    Ok((total, free.saturating_sub(pending)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_saved_disks_do_not_pause_and_dirty_disks_resume_only_when_safe() {
        let policy = Policy::default();
        let total = 1024_u64.pow(4);
        assert_eq!(policy.reserve(total), total / 100 * 5);
        assert_eq!(policy.pause_reason(total, total, None, i64::MAX), None);
        assert_eq!(policy.pause_reason(total, total, Some(1000), 300999), None);
        assert_eq!(
            policy.pause_reason(total, total, Some(1000), 301000),
            Some("backup-lag")
        );
        assert_eq!(
            policy.pause_reason(total, 0, Some(1000), 301000),
            Some("disk-space")
        );
        assert_eq!(policy.pause_reason(total, total, None, 301000), None);
    }
}
