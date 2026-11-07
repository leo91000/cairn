//! Per-publication cancellation for synchronous journal reads. VM reads retain
//! their normal source lifetime; host-executor requests inherit this scope.
use std::{
    cell::RefCell,
    io,
    sync::{Mutex, MutexGuard, TryLockError},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

thread_local! {
    static STOP: RefCell<Option<CancellationToken>> = const { RefCell::new(None) };
}

pub(super) fn current() -> Option<CancellationToken> {
    STOP.with(|slot| slot.borrow().clone())
}

pub(crate) fn scope<T>(stop: Option<CancellationToken>, read: impl FnOnce() -> T) -> T {
    struct Restore(Option<CancellationToken>);
    impl Drop for Restore {
        fn drop(&mut self) {
            STOP.with(|slot| slot.replace(self.0.take()));
        }
    }

    let Some(stop) = stop else {
        return read();
    };

    let previous = STOP.with(|slot| slot.replace(Some(stop)));
    let _restore = Restore(previous);
    read()
}

/// A VM may already be fetching this same hash. Cancelling its publication
/// waiter must leave that foreground read running, without retaining the waiter.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> io::Result<MutexGuard<'_, T>> {
    let poisoned = || io::Error::other("Disk read cache lock failed");
    let Some(stop) = current() else {
        return mutex.lock().map_err(|_| poisoned());
    };
    loop {
        if stop.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Disk read cancelled",
            ));
        }
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(_)) => return Err(poisoned()),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}
