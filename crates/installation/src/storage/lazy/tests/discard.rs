use super::*;
use std::{
    collections::HashMap,
    os::unix::fs::{FileExt, MetadataExt},
    sync::Mutex,
};

#[derive(Default)]
struct Published(Mutex<HashMap<String, Vec<u8>>>);

impl BlockSource for Published {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .get(hash)
            .cloned()
            .ok_or_else(|| failure("Missing published test block"))
    }
}

fn fixture(root: &Path, source: Arc<Published>) -> LazyDisk {
    LazyDisk::create(
        root,
        &serde_json::json!({
            "version": 1,
            "size": 3 * BLOCK,
            "blockSize": BLOCK,
            "blocks": (0..3).map(|index| serde_json::json!({
                "offset": index * BLOCK,
                "size": BLOCK,
                "hash": null
            })).collect::<Vec<_>>()
        }),
        source,
    )
    .unwrap()
}

#[test]
fn discard_preserves_neighbours_new_writes_and_sealed_generations_across_reopen() {
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Published::default());
    let disk = fixture(root.path(), source.clone());
    disk.write_at(0, &vec![0x3a; 2 * BLOCK as usize]).unwrap();
    let old = disk.seal_completed().unwrap();
    disk.discard(512, 2 * BLOCK as usize - 1024).unwrap();
    disk.write_at(BLOCK + 1024, b"newer").unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(old));
    let old_hash = disk.base.lock().unwrap()["blocks"][0]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        std::fs::read(root.path().join("cache").join(old_hash))
            .unwrap()
            .iter()
            .all(|byte| *byte == 0x3a)
    );
    drop(disk);
    let disk = LazyDisk::open(root.path(), source.clone()).unwrap();
    let mut actual = vec![0; 2 * BLOCK as usize];
    disk.read_at(0, &mut actual).unwrap();
    let mut expected = vec![0; 2 * BLOCK as usize];
    expected[..512].fill(0x3a);
    expected[2 * BLOCK as usize - 512..].fill(0x3a);
    expected[BLOCK as usize + 1024..BLOCK as usize + 1029].copy_from_slice(b"newer");
    assert_eq!(actual, expected);
    disk.discard(0, 2 * BLOCK as usize).unwrap();
    let next = disk.seal_completed().unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(next));
    let manifest = disk.base.lock().unwrap().as_ref().clone();
    assert!(
        manifest["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|extent| extent["hash"].is_null())
    );
    assert!(
        std::fs::read_dir(root.path().join("cache"))
            .unwrap()
            .next()
            .is_none(),
        "a fully discarded disk keeps no base block files"
    );
    disk.read_at(0, &mut actual).unwrap();
    assert!(actual.iter().all(|byte| *byte == 0));
    assert!(source.0.lock().unwrap().is_empty());
}

#[test]
fn repeated_discard_and_sparse_tail_do_not_append_more_frames() {
    let root = tempfile::tempdir().unwrap();
    let disk = fixture(root.path(), Arc::new(Published::default()));
    disk.discard(0, disk.size() as usize).unwrap();
    assert_eq!(disk.accounting().unwrap()["journalBytes"], 0);
    disk.write_at(1024, b"deleted").unwrap();
    disk.discard(0, BLOCK as usize).unwrap();
    let before = disk.accounting().unwrap();
    disk.discard(0, disk.size() as usize).unwrap();
    assert_eq!(disk.accounting().unwrap(), before);
    let path = root.path().join("payload-1-1.segment");
    let metadata = std::fs::metadata(path).unwrap();
    assert!(metadata.len() >= BLOCK);
    assert!(
        metadata.blocks() * 512 < 64 * 1024,
        "zero payload was not sparse"
    );
}

#[test]
fn invalid_discard_does_not_change_any_prefix_and_sparse_payload_is_authenticated() {
    let root = tempfile::tempdir().unwrap();
    let source = Arc::new(Published::default());
    let disk = fixture(root.path(), source.clone());
    disk.write_at(0, b"kept").unwrap();
    assert!(disk.discard(disk.size() - 1, 2).is_err());
    assert!(disk.discard(u64::MAX, 1).is_err());
    let mut kept = [0; 4];
    disk.read_at(0, &mut kept).unwrap();
    assert_eq!(&kept, b"kept");
    disk.discard(0, BLOCK as usize).unwrap();
    drop(disk);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(root.path().join("payload-1-1.segment"))
        .unwrap();
    file.write_all_at(b"corruption", 112 + 4 + 112 + 512)
        .unwrap();
    file.sync_all().unwrap();
    let disk = LazyDisk::open(root.path(), source).unwrap();
    assert!(disk.read_at(0, &mut kept).is_err());
    let generation = disk.seal().unwrap();
    assert!(disk.capture(generation).is_err());
}

#[test]
fn discard_sync_failure_is_not_acknowledged() {
    let root = tempfile::tempdir().unwrap();
    let disk = fixture(root.path(), Arc::new(Published::default()));
    disk.write_at(0, b"deleted").unwrap();
    disk.commit_failure.store(true, Ordering::Relaxed);
    assert!(disk.discard(0, 4096).is_err());
    assert!(disk.sync().is_err());
    assert!(disk.write_at(0, b"later").is_err());
}

#[test]
fn acknowledged_discard_survives_process_kill() {
    use std::io::BufRead;
    let root = tempfile::tempdir().unwrap();
    let disk = fixture(root.path(), Arc::new(Published::default()));
    disk.write_at(0, &vec![0x6b; BLOCK as usize]).unwrap();
    drop(disk);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["discard_crash_child", "--nocapture"])
        .env("CAIRN_DISCARD_CRASH_FIXTURE", root.path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let acknowledged = std::io::BufReader::new(child.stdout.take().unwrap())
        .lines()
        .any(|line| line.unwrap().contains("DISCARD_ACKNOWLEDGED"));
    let _ = child.kill();
    child.wait().unwrap();
    assert!(acknowledged);
    let disk = LazyDisk::open(root.path(), Arc::new(Published::default())).unwrap();
    let mut actual = vec![1; BLOCK as usize];
    disk.read_at(0, &mut actual).unwrap();
    assert!(actual.iter().all(|byte| *byte == 0));
}

#[test]
fn discard_crash_child() {
    let Some(root) = std::env::var_os("CAIRN_DISCARD_CRASH_FIXTURE") else {
        return;
    };
    let disk = LazyDisk::open(Path::new(&root), Arc::new(Published::default())).unwrap();
    disk.discard(0, BLOCK as usize).unwrap();
    println!("DISCARD_ACKNOWLEDGED");
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn discarding_damaged_base_bytes_never_reads_them_and_keeps_live_neighbours() {
    let root = tempfile::tempdir().unwrap();
    let bytes = vec![0x4a; BLOCK as usize];
    let manifest = local_base(root.path(), &[Some(bytes.clone())]);
    let file = root.path().join("cache").join(block_digest(&bytes));
    std::fs::write(&file, vec![0x4b; BLOCK as usize]).unwrap();
    let disk = LazyDisk::create(root.path(), &manifest, Arc::new(Published::default())).unwrap();
    disk.discard(1024, 4096).unwrap();
    let mut discarded = [1; 4096];
    disk.read_at(1024, &mut discarded).unwrap();
    assert_eq!(discarded, [0; 4096]);
    assert!(disk.read_at(0, &mut [0; 1024]).is_err());
    disk.seal().unwrap();
    assert!(
        disk.consolidate().is_err(),
        "damaged live neighbours must still fail consolidation"
    );
    disk.discard(0, BLOCK as usize).unwrap();
    let generation = disk.seal().unwrap();
    assert_eq!(disk.consolidate().unwrap(), Some(generation));
    assert!(disk.base.lock().unwrap()["blocks"][0]["hash"].is_null());
}

#[test]
fn no_op_discard_waits_for_a_staged_zero_write_to_be_durable() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    let disk = Arc::new(fixture(root.path(), Arc::new(Published::default())));
    let commit = disk.commit.lock().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::scope(|scope| {
        let writer = disk.clone();
        scope.spawn(move || writer.write_at(0, &[0; 4096]).unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while disk.journal.lock().unwrap().next != 2 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let trimmer = disk.clone();
        scope.spawn(move || send.send(trimmer.discard(0, 4096)).unwrap());
        assert!(
            receive.recv_timeout(Duration::from_millis(50)).is_err(),
            "discard acknowledged an uncommitted zero hint"
        );
        drop(commit);
        receive
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
    });
}
