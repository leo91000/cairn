//! Node-wide disk limits. Dirty journals never participate in cache eviction.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub cache_mi_b: u64,
    pub reserve_mi_b: u64,
    pub reserve_percent: u64,
    pub backup_seconds: u64,
    pub max_dirty_seconds: u64,
    pub automatic_archiving: bool,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            cache_mi_b: 102400,
            reserve_mi_b: 10240,
            reserve_percent: 5,
            backup_seconds: 60,
            max_dirty_seconds: 300,
            automatic_archiving: false,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        if self.cache_mi_b > 16 * 1024 * 1024
            || self.reserve_mi_b < 64
            || self.reserve_mi_b > 16 * 1024 * 1024
            || !(1..=50).contains(&self.reserve_percent)
            || !(5..=3600).contains(&self.backup_seconds)
            || self.max_dirty_seconds < self.backup_seconds
            || self.max_dirty_seconds > 86400
        {
            return Err(Error::bad("Invalid storage limits."));
        }
        Ok(())
    }
    pub fn reserve(&self, total: u64) -> u64 {
        (self.reserve_mi_b * 1024 * 1024).max(total / 100 * self.reserve_percent)
    }
    pub fn pause_reason(
        &self,
        total: u64,
        free: u64,
        dirty_since: Option<i64>,
        now: i64,
    ) -> Option<&'static str> {
        if free <= self.reserve(total) {
            return Some("disk-space");
        }
        if dirty_since
            .is_some_and(|at| now.saturating_sub(at) >= (self.max_dirty_seconds * 1000) as i64)
        {
            return Some("backup-lag");
        }
        None
    }
}
pub fn space(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok((
        stat.f_blocks.saturating_mul(stat.f_frsize),
        stat.f_bavail.saturating_mul(stat.f_frsize),
    ))
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
