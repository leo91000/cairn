use super::*;
use serde_json::json;

async fn convert(
    image: &Path,
    manifest: &Value,
    prepared: &Value,
    backend: &str,
    staging: &Path,
) -> Result<PathBuf> {
    super::convert(image, manifest, prepared, backend, staging, 0).await
}

const SIZE: u64 = 128 * 1024 * 1024;

async fn image(root: &Path, tree: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    File::create(&path).unwrap().set_len(SIZE).unwrap();
    crate::microvm::host::command(
        "mkfs.ext4",
        &[
            "-q",
            "-F",
            "-d",
            tree.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    path
}

fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn prepared() -> Value {
    json!({ "projectRoot": "/data/runs/test/workspace", "output": "/data/runs/test/output/result.md" })
}

#[tokio::test]
async fn vm_to_native_and_back_preserves_work_history_tools_and_links_without_copying_the_os() {
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("vm");
    write(
        &tree,
        "upper/home/node/.codex/sessions/thread",
        b"provider history",
    );
    write(
        &tree,
        "upper/home/node/.local/bin/tool",
        b"#!/bin/sh\nexit 0\n",
    );
    std::fs::set_permissions(
        tree.join("upper/home/node/.local/bin/tool"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    write(
        &tree,
        "upper/data/runs/test/workspace/project/uncommitted",
        b"\0dirty\xff\n",
    );
    std::os::unix::fs::symlink(
        "uncommitted",
        tree.join("upper/data/runs/test/workspace/project/link"),
    )
    .unwrap();
    write(
        &tree,
        "upper/data/runs/test/output/result.md",
        b"previous result",
    );
    write(&tree, "upper/usr/lib/os-specific", b"not portable");
    let original = image(root.path(), &tree, "vm.ext4").await;
    let hash = crate::storage::digest::block(&std::fs::read(&original).unwrap());
    let native = root.path().join("native-migration");
    std::fs::create_dir(&native).unwrap();
    let disk = convert(
        &original,
        &json!({ "size": SIZE }),
        &prepared(),
        "native",
        &native,
    )
    .await
    .unwrap();
    let fs = filesystem(&disk, 0, SIZE).unwrap();
    assert_eq!(
        fs.read("/home/.codex/sessions/thread").unwrap(),
        b"provider history"
    );
    assert_eq!(
        fs.read("/workspaces/project/uncommitted").unwrap(),
        b"\0dirty\xff\n"
    );
    assert_eq!(
        fs.read_link("/workspaces/project/link").unwrap().as_ref(),
        b"uncommitted"
    );
    assert_eq!(
        fs.metadata("/home/.local/bin/tool").unwrap().mode() & 0o111,
        0o110
    );
    assert!(!fs.exists("/usr").unwrap());
    let vm = root.path().join("vm-migration");
    std::fs::create_dir(&vm).unwrap();
    let disk = convert(
        &disk,
        &json!({ "kind": "native-ext4-v1", "size": SIZE }),
        &prepared(),
        "firecracker",
        &vm,
    )
    .await
    .unwrap();
    let fs = filesystem(&disk, 0, SIZE).unwrap();
    assert_eq!(
        fs.read("/upper/data/runs/test/workspace/project/uncommitted")
            .unwrap(),
        b"\0dirty\xff\n"
    );
    assert_eq!(
        fs.read("/upper/home/node/.codex/sessions/thread").unwrap(),
        b"provider history"
    );
    assert_eq!(fs.read("/upper/var/lib/cairn/initialized").unwrap(), b"1");
    assert_eq!(
        crate::storage::digest::block(&std::fs::read(&original).unwrap()),
        hash
    );
    assert_eq!(
        fs.metadata("/upper/home/node/.local/bin/tool")
            .unwrap()
            .uid(),
        1000
    );
}

#[tokio::test]
async fn copied_links_cannot_redirect_later_migration_writes_onto_the_host() {
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("vm");
    write(&tree, "upper/home/node/file", b"kept");
    std::fs::create_dir_all(tree.join("upper/data/runs/test/workspace")).unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, tree.join("upper/data/runs/test/workspace/escape"))
        .unwrap();
    write(&tree, "upper/data/runs/test/output/result.md", b"result");
    let original = image(root.path(), &tree, "vm.ext4").await;
    let staging = root.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let disk = convert(
        &original,
        &json!({ "size": SIZE }),
        &prepared(),
        "native",
        &staging,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    let fs = filesystem(&disk, 0, SIZE).unwrap();
    assert!(
        fs.symlink_metadata("/workspaces/escape")
            .unwrap()
            .is_symlink()
    );
    // Mapping a later directory through an existing copied link is refused.
    let mut copy = Copy {
        root: staging.join("tree"),
        remaining: SIZE,
        entries: 0,
        minimum_free: 0,
    };
    assert!(
        copy.tree(
            &fs,
            Path::new("/home"),
            &staging.join("tree/workspaces/escape/home")
        )
        .is_err()
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
}

#[tokio::test]
async fn selected_roots_cannot_be_symlinks_or_files_in_either_backend() {
    for native in [false, true] {
        for name in ["home", "workspaces", "output"] {
            for symlink in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let tree = root.path().join("source");
                for (directory, vm) in [
                    ("home", "upper/home/node"),
                    ("workspaces", "upper/data/runs/test/workspace"),
                    ("output", "upper/data/runs/test/output"),
                ] {
                    let path = tree.join(if native { directory } else { vm });
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    if directory != name {
                        std::fs::create_dir(path).unwrap();
                    } else if symlink {
                        std::os::unix::fs::symlink("/", path).unwrap();
                    } else {
                        std::fs::write(path, b"not a directory").unwrap();
                    }
                }
                let original = image(root.path(), &tree, "source.ext4").await;
                let staging = root.path().join("staging");
                std::fs::create_dir(&staging).unwrap();
                let mut manifest = json!({ "size": SIZE });
                if native {
                    manifest["kind"] = "native-ext4-v1".into();
                }
                let error = convert(
                    &original,
                    &manifest,
                    &prepared(),
                    if native { "firecracker" } else { "native" },
                    &staging,
                )
                .await
                .unwrap_err();
                assert!(
                    error.to_string().contains("root must be a directory"),
                    "{error}"
                );
                assert!(!staging.join("converted.ext4").exists());
            }
        }
    }
}

#[tokio::test]
async fn paired_vm_copies_workspace_and_redirected_home_caches() {
    let root = tempfile::tempdir().unwrap();
    let system_tree = root.path().join("system-tree");
    write(&system_tree, "upper/home/node/.codex/history", b"history");
    let work_tree = root.path().join("work-tree");
    write(&work_tree, "runs/test/workspace/dirty", b"workspace");
    write(&work_tree, "runs/test/output/result.md", b"output");
    write(&work_tree, ".cairn-system/home-cache/tool", b"cache");
    write(&work_tree, ".cairn-system/home-share/data", b"share");
    write(&work_tree, ".cairn-system/android/emulator", b"android");
    let system = image(root.path(), &system_tree, "system.ext4").await;
    let work = image(root.path(), &work_tree, "work.ext4").await;
    let paired = root.path().join("paired.ext4");
    let mut file = File::create(&paired).unwrap();
    std::io::copy(&mut File::open(system).unwrap(), &mut file).unwrap();
    std::io::copy(&mut File::open(work).unwrap(), &mut file).unwrap();
    let staging = root.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let disk = convert(
        &paired,
        &json!({ "version": 2, "size": 2 * SIZE,
        "layout": { "kind": "paired-ext4-v1", "systemBytes": SIZE } }),
        &prepared(),
        "native",
        &staging,
    )
    .await
    .unwrap();
    let fs = filesystem(&disk, 0, 2 * SIZE).unwrap();
    for (path, expected) in [
        ("/workspaces/dirty", b"workspace".as_slice()),
        ("/output/result.md", b"output"),
        ("/home/.cache/tool", b"cache"),
        ("/home/.local/share/data", b"share"),
        ("/home/.android/emulator", b"android"),
    ] {
        assert_eq!(fs.read(path).unwrap(), expected);
    }
}

#[tokio::test]
async fn malformed_filesystem_and_missing_work_never_produce_a_destination() {
    let root = tempfile::tempdir().unwrap();
    let raw = root.path().join("invalid.ext4");
    File::create(&raw).unwrap().set_len(SIZE).unwrap();
    let first = root.path().join("first");
    std::fs::create_dir(&first).unwrap();
    assert!(
        convert(
            &raw,
            &json!({ "size": SIZE }),
            &prepared(),
            "native",
            &first
        )
        .await
        .is_err()
    );
    assert!(!first.join("converted.ext4").exists());
    let tree = root.path().join("tree");
    write(&tree, "upper/home/node/file", b"home");
    let raw = image(root.path(), &tree, "incomplete.ext4").await;
    let second = root.path().join("second");
    std::fs::create_dir(&second).unwrap();
    assert!(
        convert(
            &raw,
            &json!({ "size": SIZE }),
            &prepared(),
            "native",
            &second
        )
        .await
        .is_err()
    );
    assert!(!second.join("converted.ext4").exists());
}

#[tokio::test]
#[ignore = "local measurement; run backend_conversion_measurement with --ignored --nocapture --test-threads=1"]
async fn backend_conversion_measurement() {
    use std::{os::fd::AsRawFd, time::Instant};
    fn cpu() -> f64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        let usage = unsafe { usage.assume_init() };
        (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0
    }
    async fn reads(disk: &Path, path: &str) -> Value {
        let file = File::open(disk).unwrap();
        unsafe {
            libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
        let mut samples = Vec::new();
        for _ in 0..2 {
            let start = Instant::now();
            let before = cpu();
            let fs = filesystem(disk, 0, SIZE).unwrap();
            assert_eq!(fs.read(path).unwrap().len(), 8 * 1024 * 1024);
            samples.push(json!({ "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu() - before }));
        }
        json!(samples)
    }
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("source-tree");
    let start = Instant::now();
    let before = cpu();
    let mut bytes = vec![0; 8 * 1024 * 1024];
    // Fixed pseudo-random payload, reproducible without an OS random source.
    let mut value = 42u64;
    for byte in &mut bytes {
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        *byte = value as u8;
    }
    write(&tree, "upper/data/runs/test/workspace/asset", &bytes);
    for index in 0..128 {
        write(
            &tree,
            &format!("upper/data/runs/test/workspace/text/{index}"),
            &vec![b'x'; 4096],
        );
    }
    write(&tree, "upper/home/node/.codex/history", b"session");
    write(&tree, "upper/data/runs/test/output/result.md", b"result");
    let source = image(root.path(), &tree, "source.ext4").await;
    let write_reference =
        json!({ "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu() - before });
    let reference_reads = reads(&source, "/upper/data/runs/test/workspace/asset").await;
    let staging = root.path().join("conversion");
    std::fs::create_dir(&staging).unwrap();
    let start = Instant::now();
    let before = cpu();
    let target = convert(
        &source,
        &json!({ "size": SIZE }),
        &prepared(),
        "native",
        &staging,
    )
    .await
    .unwrap();
    let conversion =
        json!({ "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu() - before });
    let converted_reads = reads(&target, "/workspaces/asset").await;
    let store = crate::store::Store::open(&root.path().join("fixture.sqlite")).unwrap();
    let vault = crate::vault::Vault::new(store, root.path()).unwrap();
    let codec = super::super::block_codec::BlockCodec::default();
    let mut accounting = Vec::new();
    for disk in [&source, &target] {
        let data = std::fs::read(disk).unwrap();
        let mut encoded = 0;
        let mut decoded = 0;
        for chunk in data
            .chunks(4 * 1024 * 1024)
            .filter(|chunk| chunk.iter().any(|byte| *byte != 0))
        {
            decoded += chunk.len();
            encoded += codec
                .encode(vault.clone(), "measurement".into(), chunk.to_vec())
                .await
                .unwrap()
                .len();
        }
        accounting.push(json!({ "imageBytes": SIZE, "nonzeroBlockBytes": decoded, "encodedBlockBytes": encoded }));
    }
    println!(
        "CONVERSION_MEASUREMENT {}",
        json!({ "referenceWrite": write_reference, "referenceReads": reference_reads,
        "conversion": conversion, "convertedReads": converted_reads, "accounting": accounting, "payloadBytes": bytes.len() + 128 * 4096 + 13 })
    );
}

#[tokio::test]
async fn sparse_work_remains_sparse_and_manager_reserve_is_enforced() {
    use std::os::unix::fs::MetadataExt;
    const SPARSE_SIZE: u64 = 2 * SIZE;
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("vm");
    write(&tree, "upper/home/node/file", b"home");
    write(&tree, "upper/data/runs/test/output/result.md", b"result");
    write(&tree, "upper/data/runs/test/workspace/sparse", b"start");
    let sparse = tree.join("upper/data/runs/test/workspace/sparse");
    let file = OpenOptions::new().write(true).open(sparse).unwrap();
    file.set_len(SPARSE_SIZE).unwrap();
    file.write_all_at(b"end", SPARSE_SIZE - 3).unwrap();
    let original = image(root.path(), &tree, "vm.ext4").await;
    let staging = root.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    let disk = convert(
        &original,
        &json!({ "size": SIZE }),
        &prepared(),
        "native",
        &staging,
    )
    .await
    .unwrap();
    let copied = std::fs::metadata(staging.join("tree/workspaces/sparse")).unwrap();
    assert_eq!(copied.len(), SPARSE_SIZE);
    assert!(
        copied.blocks() * 512 < 1024 * 1024,
        "sparse file was expanded on manager"
    );
    let fs = filesystem(&disk, 0, SIZE).unwrap();
    let mut saved = fs.open("/workspaces/sparse").unwrap();
    let mut bytes = [0; 5];
    saved.read_bytes(&mut bytes).unwrap();
    assert_eq!(&bytes, b"start");
    saved.seek_to(SPARSE_SIZE - 3).unwrap();
    assert_eq!(saved.read_bytes(&mut bytes).unwrap(), 3);
    assert_eq!(&bytes[..3], b"end");
    let reverse = root.path().join("reverse");
    std::fs::create_dir(&reverse).unwrap();
    let roundtrip = convert(
        &disk,
        &json!({ "kind": "native-ext4-v1", "size": SIZE }),
        &prepared(),
        "firecracker",
        &reverse,
    )
    .await
    .unwrap();
    let fs = filesystem(&roundtrip, 0, SIZE).unwrap();
    assert_eq!(
        fs.metadata("/upper/data/runs/test/workspace/sparse")
            .unwrap()
            .len(),
        SPARSE_SIZE
    );
    let copied =
        std::fs::metadata(reverse.join("tree/upper/data/runs/test/workspace/sparse")).unwrap();
    assert!(copied.blocks() * 512 < 1024 * 1024);
    let refused = root.path().join("refused");
    std::fs::create_dir(&refused).unwrap();
    let error = super::convert(
        &original,
        &json!({ "size": SIZE }),
        &prepared(),
        "native",
        &refused,
        u64::MAX,
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, 507);
    assert!(!refused.join("converted.ext4").exists());
}
