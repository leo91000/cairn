//! Reproducible local block-interface comparison, without VMM or filesystem IO.
//! This complements the disposable native/VM end-to-end qualifications.
use cairn_installation::storage::{Disk, LazyDisk, LocalDisk};
use serde_json::{Value, json};
use std::{fs::File, io, os::fd::AsRawFd, path::Path, time::Instant};

const MIB: usize = 1024 * 1024;
const SIZE: usize = 16 * MIB;

fn cpu_ms() -> f64 {
    let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, value.as_mut_ptr()) },
        0
    );
    let value = unsafe { value.assume_init() };
    value.tv_sec as f64 * 1000.0 + value.tv_nsec as f64 / 1_000_000.0
}

fn measured(work: impl FnOnce() -> io::Result<()>) -> io::Result<Value> {
    let at = Instant::now();
    let cpu = cpu_ms();
    work()?;
    Ok(json!({ "wallMs": at.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu }))
}

fn cold_hint(root: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            cold_hint(&entry.path())?;
        } else if entry.file_type()?.is_file() {
            let file = File::open(entry.path())?;
            let result =
                unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
            if result != 0 {
                return Err(io::Error::from_raw_os_error(result));
            }
        }
    }
    Ok(())
}

fn reads(disk: &dyn Disk, asset: &[u8], text: &[u8]) -> io::Result<Value> {
    measured(|| {
        let mut read = vec![0; asset.len()];
        disk.read_at(0, &mut read)?;
        assert_eq!(read, asset);
        let mut read = vec![0; text.len()];
        disk.read_at(asset.len() as u64, &mut read)?;
        assert_eq!(read, text);
        Ok(())
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempfile::Builder::new()
        .prefix("local-storage-measurement-")
        .tempdir_in(std::env::current_dir()?.parent().unwrap())?;
    let mut asset = vec![0; 8 * MIB];
    for (index, bytes) in asset.chunks_mut(32).enumerate() {
        bytes.copy_from_slice(blake3::hash(&(index as u64).to_le_bytes()).as_bytes());
    }
    let text: Vec<_> = b"cairn local storage fixture\n"
        .iter()
        .copied()
        .cycle()
        .take(128 * 4096)
        .collect();
    for sample in 0..3 {
        for mode in ["legacy-journal", "direct-file"] {
            let root = fixture.path().join(format!("{mode}-{sample}"));
            std::fs::create_dir(&root)?;
            let open = || -> io::Result<Box<dyn Disk>> {
                Ok(if mode == "legacy-journal" {
                    Box::new(LazyDisk::open(&root.join("lazy"))?)
                } else {
                    Box::new(LocalDisk::open(&root.join("data.ext4"), true)?)
                })
            };
            if mode == "legacy-journal" {
                let disk = LazyDisk::create(
                    &root.join("lazy"),
                    &json!({
                        "version": 1, "size": SIZE, "blockSize": 4 * MIB,
                        "blocks": (0..4).map(|index| json!({"offset": index * 4 * MIB, "size": 4 * MIB, "hash": null})).collect::<Vec<_>>()
                    }),
                )?;
                disk.set_context(&json!({ "owner": cairn_installation::config::id(),
                    "policy": { "memoryCacheMiB": 4, "reserveMiB": 64, "reservePercent": 1 } }))?;
            } else {
                File::create(root.join("data.ext4"))?.set_len(SIZE as u64)?;
            }
            let disk = open()?;
            let asset_write = measured(|| disk.write_at(0, &asset))?;
            let writes = measured(|| {
                for (index, bytes) in text.chunks(4096).enumerate() {
                    disk.write_at((asset.len() + index * 4096) as u64, bytes)?;
                }
                disk.sync()
            })?;
            drop(disk);
            cold_hint(&root)?;
            let disk = open()?;
            let cold = reads(disk.as_ref(), &asset, &text)?;
            let warm = reads(disk.as_ref(), &asset, &text)?;
            drop(disk);
            let allocated = cairn_installation::microvm::budget::allocated(&root)?;
            let encoded: u64 = walk_lengths(&root)?;
            println!(
                "{}",
                json!({ "mode": mode, "sample": sample, "logicalCapacity": SIZE,
                "payloadBytes": asset.len() + text.len(), "encodedBytes": encoded, "allocatedBytes": allocated,
                "assetWrite": asset_write, "writes128Fsynced4KiB": writes, "coldHintRead": cold, "warmRead": warm,
                "conditions": "One process, local block-interface IO, identical deterministic BLAKE3 asset/text; development build; no VMM, ext4 guest, cgroup or WAN. Cold uses reopen + POSIX_FADV_DONTNEED, not a guaranteed hardware-cold cache. Journal remains unconsolidated." })
            );
        }
    }
    Ok(())
}

fn walk_lengths(root: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            total += walk_lengths(&entry.path())?;
        } else if entry.file_type()?.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}
