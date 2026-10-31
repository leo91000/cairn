use std::time::Duration;

/// Bookends stay on the SQLite actor thread and exclude queue admission.
pub(crate) struct CpuTime(Option<Duration>);

impl CpuTime {
    pub(crate) fn start() -> Self {
        if !tracing::event_enabled!(target: "cairn_performance", tracing::Level::INFO) {
            return Self(None);
        }
        Self(read())
    }

    pub(crate) fn elapsed(self) -> Option<Duration> {
        let started = self.0?;
        read()?.checked_sub(started)
    }
}

#[cfg(target_os = "linux")]
fn read() -> Option<Duration> {
    let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: the successful clock call initializes the supplied timespec.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, value.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: the successful call above initialized this value.
    let value = unsafe { value.assume_init() };
    let seconds = u64::try_from(value.tv_sec).ok()?;
    let nanos = u32::try_from(value.tv_nsec).ok()?;
    if nanos >= 1_000_000_000 {
        return None;
    }
    Some(Duration::new(seconds, nanos))
}

#[cfg(not(target_os = "linux"))]
fn read() -> Option<Duration> {
    None
}
