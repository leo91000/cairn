//! Guest mounts and barriers over both views of the conversation's journal.
use crate::error::{Error, Result};
use std::{
    os::unix::fs::FileTypeExt,
    path::Path,
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::process::Command;

const SYSTEM: &str = "/oldroot/run/data";
const WORKSPACE: &str = "/data";
static MOUNTED: AtomicBool = AtomicBool::new(false);

pub(super) fn mounted() -> bool {
    MOUNTED.load(Ordering::Acquire)
}

fn targets() -> Vec<&'static str> {
    if MOUNTED.load(Ordering::Acquire) {
        vec![WORKSPACE, SYSTEM]
    } else {
        vec![SYSTEM]
    }
}

pub(super) async fn mount() -> Result<()> {
    if MOUNTED.load(Ordering::Acquire) {
        return Ok(());
    }
    if !tokio::fs::metadata("/dev/vdc")
        .await?
        .file_type()
        .is_block_device()
        || !tokio::fs::symlink_metadata(WORKSPACE).await?.is_dir()
    {
        return Err(Error::bad("Invalid guest workspace mount."));
    }
    // PATCH changes the host backing while this guest device is unmounted.
    // Discard any buffers populated by kernel discovery before reading ext4.
    run("blockdev", &["--flushbufs", "/dev/vdc"]).await?;
    run(
        "mount",
        &["-t", "ext4", "-o", "noatime", "/dev/vdc", WORKSPACE],
    )
    .await?;
    let mut bound = Vec::new();
    let mounted = async {
        let private = Path::new("/data/.leo-system");
        crate::skills::private_dir(private).await?;
        for directory in ["docker", "containerd"] {
            let source = format!("/data/.leo-system/{directory}");
            let target = format!("/var/lib/{directory}");
            tokio::fs::create_dir_all(&source).await?;
            run("mount", &["--bind", &source, &target]).await?;
            bound.push(target);
        }
        std::os::unix::fs::chown(WORKSPACE, Some(1000), Some(1000))?;
        Ok::<(), Error>(())
    }
    .await;
    if let Err(error) = mounted {
        for target in bound.iter().rev() {
            let _ = run("umount", &[target]).await;
        }
        let _ = run("umount", &[WORKSPACE]).await;
        return Err(error);
    }
    MOUNTED.store(true, Ordering::Release);
    Ok(())
}

async fn run(program: &str, arguments: &[&str]) -> Result<()> {
    if Command::new(program)
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?
        .success()
    {
        return Ok(());
    }
    Err(Error::unavailable("Guest filesystem operation failed."))
}

pub(super) async fn sync(program: &str) -> Result<()> {
    for target in targets() {
        run(program, &["--file-system", target]).await?;
    }
    Ok(())
}

pub(super) async fn freeze(freeze: bool) -> Result<bool> {
    let targets = targets();
    if !freeze {
        for target in targets.iter().rev() {
            let _ = run("fsfreeze", &["--unfreeze", target]).await;
        }
        return Ok(true);
    }
    for target in &targets {
        if run("fsfreeze", &["--freeze", target]).await.is_err() {
            // An interrupted/failed response can still have frozen a mount.
            // Roll back every view before reporting failure to the host.
            for target in targets.iter().rev() {
                let _ = run("fsfreeze", &["--unfreeze", target]).await;
            }
            return Ok(false);
        }
    }
    Ok(true)
}
