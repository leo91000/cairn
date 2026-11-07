//! Guest barriers over the conversation filesystem.
use crate::error::{Error, Result};
use std::process::Stdio;
use tokio::process::Command;

const SYSTEM: &str = "/oldroot/run/data";

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
    for target in [SYSTEM] {
        run(program, &["--file-system", target]).await?;
    }
    Ok(())
}

pub(super) async fn freeze(freeze: bool) -> Result<bool> {
    let targets = [SYSTEM];
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
