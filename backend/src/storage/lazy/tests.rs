use super::Disk;
use super::*;
use sha2::{Digest, Sha256};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
struct Source {
    reads: AtomicUsize,
}
impl BlockSource for Source {
    fn fetch(&self, _hash: &str) -> io::Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(vec![7; 4 * 1024 * 1024])
    }
}
#[test]
fn acknowledged_write_survives_killing_the_storage_process() {
    use std::io::BufRead;
    let root = tempfile::tempdir().unwrap();
    let manifest = serde_json::json!({"version":1,"size":4096,"blockSize":4194304,"blocks":[{"offset":0,"size":4096,"hash":null}]});
    drop(
        LazyDisk::create(
            root.path(),
            &manifest,
            Arc::new(Source {
                reads: AtomicUsize::new(0),
            }),
        )
        .unwrap(),
    );
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["crash_writer", "--nocapture"])
        .env("LEO_STORAGE_CRASH_TEST_DIR", root.path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut acknowledged = false;
    for line in output.lines() {
        if line.unwrap().contains("WRITE_ACKNOWLEDGED") {
            acknowledged = true;
            break;
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(acknowledged);
    let disk = LazyDisk::open(
        root.path(),
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    let mut bytes = [0; 8];
    disk.read_at(3, &mut bytes).unwrap();
    assert_eq!(&bytes, b"survives");
}
#[test]
fn crash_writer() {
    let Some(root) = std::env::var_os("LEO_STORAGE_CRASH_TEST_DIR") else {
        return;
    };
    let disk = LazyDisk::open(
        std::path::Path::new(&root),
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    disk.write_at(3, b"survives").unwrap();
    println!("WRITE_ACKNOWLEDGED");
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    loop {
        std::thread::park();
    }
}
#[test]
fn journal_integrity_covers_the_write_location() {
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let manifest = serde_json::json!({"version":1,"size":4096,"blockSize":4194304,"blocks":[{"offset":0,"size":4096,"hash":null}]});
    let disk = LazyDisk::create(root.path(), &manifest, source.clone()).unwrap();
    disk.write_at(3, b"retained").unwrap();
    drop(disk);
    let corruptor = rusqlite::Connection::open(root.path().join("journal.sqlite")).unwrap();
    corruptor
        .execute("UPDATE writes SET start=4, end=12", [])
        .unwrap();
    drop(corruptor);
    assert!(
        LazyDisk::open(root.path(), source).is_err(),
        "corrupt journal metadata must be rejected before any read"
    );
}
#[test]
fn unavailable_or_corrupt_base_never_becomes_a_zero_block() {
    struct Missing;
    impl BlockSource for Missing {
        fn fetch(&self, _: &str) -> io::Result<Vec<u8>> {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Storage temporarily unavailable",
            ))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let hash = hex::encode(Sha256::digest(vec![8; 4 * 1024 * 1024]));
    let manifest = serde_json::json!({"version":1,"size":4194304,"blockSize":4194304,"blocks":[{"offset":0,"size":4194304,"hash":hash}]});
    let disk = LazyDisk::create(root.path(), &manifest, Arc::new(Missing)).unwrap();
    assert_eq!(
        disk.read_at(0, &mut [0; 8]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(disk);
    let disk = LazyDisk::open(
        root.path(),
        Arc::new(Source {
            reads: AtomicUsize::new(0),
        }),
    )
    .unwrap();
    assert!(disk.read_at(0, &mut [0; 8]).is_err());
}
#[test]
fn sealed_generation_excludes_later_writes_and_publication_preserves_them() {
    struct Remote(std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>);
    impl BlockSource for Remote {
        fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .get(hash)
                .cloned()
                .ok_or_else(|| io::Error::other("Not published"))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Remote(Default::default()));
    let initial = serde_json::json!({"version":1,"size":4096,"blockSize":4194304,"blocks":[{"offset":0,"size":4096,"hash":null}]});
    let disk = LazyDisk::create(root.path(), &initial, source.clone()).unwrap();
    disk.write_at(7, b"before").unwrap();
    let generation = disk.seal().unwrap();
    disk.write_at(7, b"after!").unwrap();
    let manifest = disk.capture(generation).unwrap();
    let hash = manifest["blocks"][0]["hash"].as_str().unwrap();
    let bytes = disk.captured_block(generation, hash).unwrap();
    assert_eq!(&bytes[7..13], b"before");
    source.0.lock().unwrap().insert(hash.to_owned(), bytes);
    disk.commit_published(generation).unwrap();
    assert!(
        disk.commit_published(generation).is_err(),
        "a stale publication cannot advance the disk twice"
    );
    drop(disk);
    let disk = LazyDisk::open(root.path(), source).unwrap();
    let mut bytes = [0; 6];
    disk.read_at(7, &mut bytes).unwrap();
    assert_eq!(&bytes, b"after!");
}
#[test]
fn partial_write_is_durable_without_fetching_its_remote_base() {
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        reads: AtomicUsize::new(0),
    });
    let hash = hex::encode(Sha256::digest(vec![7; 4 * 1024 * 1024]));
    let manifest = serde_json::json!({"version":1,"size":4194304,"blockSize":4194304,"blocks":[{"offset":0,"size":4194304,"hash":hash}]});
    let disk = LazyDisk::create(root.path(), &manifest, source.clone()).unwrap();
    disk.write_at(123, b"retained").unwrap();
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
    drop(disk);
    let disk = LazyDisk::open(root.path(), source.clone()).unwrap();
    let mut bytes = [0; 8];
    disk.read_at(123, &mut bytes).unwrap();
    assert_eq!(&bytes, b"retained");
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
    let mut bytes = [0; 10];
    disk.read_at(122, &mut bytes).unwrap();
    assert_eq!(&bytes, b"\x07retained\x07");
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    disk.read_at(122, &mut bytes).unwrap();
    assert_eq!(
        source.reads.load(Ordering::SeqCst),
        1,
        "a verified clean block should be reused"
    );
}
