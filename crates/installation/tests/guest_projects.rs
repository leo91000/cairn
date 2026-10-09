//! Run as root in a disposable mount namespace; never mount over the host's state.
use cairn_installation::microvm::projects;
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

/// Watches `<root>/project` as an unprivileged user until `<root>/done` exists:
/// the project must never be writable and must only ever show complete data.
const OBSERVER: &str = r"
import errno, os, pathlib, sys, time
root = pathlib.Path(sys.argv[1]); target = root / 'project'
print('ready', flush=True)
seen = False
while not (root / 'done').exists():
    try:
        fd = os.open(target / 'changed', os.O_CREAT | os.O_WRONLY, 0o600)
    except OSError as e:
        assert e.errno in (errno.ENOENT, errno.EACCES, errno.EROFS), e
    else:
        os.close(fd)
        raise AssertionError('read-only project was writable during publication')
    if (target / 'sentinel').exists():
        assert (target / 'sentinel').read_text() == 'complete data'
        seen = True
    time.sleep(0.001)
assert seen, 'published data never became readable'
";

async fn unmount(target: &Path) {
    assert!(
        Command::new("umount")
            .arg(target)
            .status()
            .await
            .unwrap()
            .success()
    );
}

fn assert_read_only(directory: &Path) {
    assert_eq!(
        std::fs::write(directory.join("changed"), "no")
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EROFS)
    );
}

fn assert_complete(target: &Path) {
    assert_eq!(
        std::fs::read_to_string(target.join("sentinel")).unwrap(),
        "complete data"
    );
}

/// Puts a `mount` first in `PATH` that waits before mounting, so that an
/// incorrectly published directory is observably writable, independent of
/// scheduler timing.
fn slow_down_mount(bin: &Path) {
    std::fs::create_dir(bin).unwrap();
    std::fs::write(
        bin.join("mount"),
        "#!/bin/sh\nsleep 0.1\nexec /usr/bin/mount \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(bin.join("mount"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let prior = std::env::var("PATH").unwrap();
    // This test is invoked alone with --test-threads=1 in a disposable process.
    unsafe {
        std::env::set_var("PATH", format!("{}:{prior}", bin.display()));
    }
}

#[tokio::test]
#[ignore = "requires root and a private mount namespace with isolated /var/lib"]
async fn read_only_publication_never_exposes_writable_data_and_recovers_after_restart() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_eq!(
        std::env::var("CAIRN_PROJECT_MOUNT_TEST").as_deref(),
        Ok("1")
    );
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let private = path.join("private");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&private)
        .unwrap();
    let source = private.join("content");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("sentinel"), "complete data").unwrap();
    std::os::unix::fs::chown(&source, Some(1000), Some(1000)).unwrap();
    let target = path.join("project");
    slow_down_mount(&path.join("bin"));
    let mut child = Command::new("setpriv")
        .args([
            "--reuid=1000",
            "--regid=1000",
            "--clear-groups",
            "python3",
            "-c",
            OBSERVER,
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).await.unwrap();
    assert_eq!(line.trim(), "ready");
    projects::publish(&source, &target, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    std::fs::write(path.join("done"), "1").unwrap();
    let result = child.wait_with_output().await.unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!source.join("changed").exists());
    // A reboot loses mounts, and a crash before mkdir can leave only the policy.
    unmount(&target).await;
    std::fs::remove_dir(&target).unwrap();
    assert!(projects::reopen(&target, true).await.unwrap());
    assert_complete(&target);
    assert_read_only(&target);
    unmount(&target).await;
    projects::restore().await.unwrap();
    assert_complete(&target);
    assert_read_only(&target);
    unmount(&target).await;
    // An explicit writable policy still reconstructs the retained backing data.
    assert!(projects::reopen(&target, false).await.unwrap());
    std::fs::write(target.join("changed"), "authorized").unwrap();
    assert_eq!(
        std::fs::read_to_string(source.join("changed")).unwrap(),
        "authorized"
    );
    unmount(&target).await;
    // Legacy disks store data directly at the public path and have no source map.
    let legacy_source = private.join("legacy");
    std::fs::create_dir(&legacy_source).unwrap();
    let legacy = path.join("legacy");
    projects::publish(&legacy_source, &legacy, false)
        .await
        .unwrap();
    assert!(projects::reopen(&legacy, true).await.unwrap());
    assert_read_only(&legacy);
    assert!(projects::reopen(&legacy, false).await.unwrap());
    std::fs::write(legacy.join("changed"), "authorized").unwrap();
    unmount(&legacy).await;
}
