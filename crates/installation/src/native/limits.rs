//! Provision before opening journals, including under an older installed unit.
use std::io;

const MIN_OPEN_FILES: libc::rlim_t = 65_536;

pub(crate) fn configure() -> io::Result<()> {
    let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limits.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut limits = unsafe { limits.assume_init() };
    if limits.rlim_max < MIN_OPEN_FILES {
        return Err(io::Error::other(
            "Native storage requires an open-file hard limit of at least 65536; configure LimitNOFILE on the native node service.",
        ));
    }
    if limits.rlim_cur < MIN_OPEN_FILES {
        limits.rlim_cur = MIN_OPEN_FILES;
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    tracing::info!(
        soft = limits.rlim_cur,
        hard = limits.rlim_max,
        "Native open-file budget configured"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_open_file_budget() {
        const CHILD: &str = "CAIRN_TEST_NATIVE_NOFILE";
        if let Ok(mode) = std::env::var(CHILD) {
            let mut limits = libc::rlimit {
                rlim_cur: 1024,
                rlim_max: MIN_OPEN_FILES,
            };
            if mode == "high" {
                limits.rlim_cur = MIN_OPEN_FILES;
            }
            if mode == "insufficient" {
                limits.rlim_max = 1024;
            }
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) }, 0);
            if mode == "insufficient" {
                assert!(configure().unwrap_err().to_string().contains("LimitNOFILE"));
                return;
            }
            configure().unwrap();
            let mut observed = std::mem::MaybeUninit::<libc::rlimit>::uninit();
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, observed.as_mut_ptr()) },
                0
            );
            let observed = unsafe { observed.assume_init() };
            assert_eq!(observed.rlim_cur, limits.rlim_cur.max(MIN_OPEN_FILES));
            assert_eq!(observed.rlim_max, limits.rlim_max);
            if mode == "low" {
                retained_journal_generations();
            }
            return;
        }
        for mode in ["low", "high", "insufficient"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .env(CHILD, mode)
                .args([
                    "--exact",
                    "native::limits::tests::inherited_open_file_budget",
                    "--nocapture",
                ])
                .status()
                .unwrap();
            assert!(status.success(), "Open-file budget child failed: {mode}");
        }
    }

    fn retained_journal_generations() {
        use crate::storage::{BlockSource, Disk, LazyDisk};
        use std::sync::Arc;

        struct NoRemote;
        impl BlockSource for NoRemote {
            fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
                Err(io::Error::other("Unexpected remote read"))
            }
        }

        let root = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!({
            "version": 1,
            "size": 4194304,
            "blockSize": 4194304,
            "blocks": [{ "offset": 0, "size": 4194304, "hash": null }]
        });
        let disk = LazyDisk::create(root.path(), &manifest, Arc::new(NoRemote)).unwrap();
        for generation in 0..1500u64 {
            disk.write_at(0, &generation.to_le_bytes()).unwrap();
            disk.seal_completed().unwrap();
        }
        drop(disk);
        let disk = LazyDisk::open(root.path(), Arc::new(NoRemote)).unwrap();
        let mut bytes = [0; 8];
        disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(u64::from_le_bytes(bytes), 1499);
    }
}
