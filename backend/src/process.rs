use crate::{
    config::Config,
    error::{Error, Result},
};
use std::{collections::HashMap, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
pub type Environment = HashMap<String, String>;
pub fn storage_key(key: &str) -> bool {
    key.starts_with("AWS_") || key.starts_with("ARCHIVE_") || key.starts_with("STORAGE_S3_")
}
pub fn remove_storage_environment(env: &mut Environment) {
    env.retain(|key, _| !storage_key(key));
}
pub fn codex_environment(config: &Config, home: &Path) -> Environment {
    let mut env = std::env::vars().collect::<Environment>();
    remove_storage_environment(&mut env);
    env.insert("HOME".into(), config.home.to_string_lossy().into_owned());
    env.insert("CODEX_HOME".into(), home.to_string_lossy().into_owned());
    for key in ["CODEX_API_KEY", "OPENAI_API_KEY", "CODEX_THREAD_ID"] {
        env.remove(key);
    }
    env
}
pub fn command(binary: &str, args: &[String], env: &Environment, cwd: Option<&Path>) -> Command {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_clear()
        // Archive credentials stay with the archive storage's own AWS CLI calls.
        .envs(env.iter().filter(|(key, _)| !storage_key(key)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
}
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}
pub async fn bounded_output(
    mut command: Command,
    timeout: Duration,
    limit: usize,
) -> Result<Output> {
    let mut child = command
        .spawn()
        .map_err(|_| Error::new(503, "Unable to start the requested executable."))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let operation = async {
        let (stdout, stderr, status) = tokio::try_join!(
            read_bounded(stdout, limit),
            read_bounded(stderr, limit),
            async { child.wait().await.map_err(Error::from) }
        )?;
        Ok(Output {
            success: status.success(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    };
    match tokio::time::timeout(timeout, operation).await {
        Ok(Ok(output)) => Ok(output),
        result => {
            let _ = child.kill().await;
            match result {
                Ok(Err(error)) => Err(error),
                _ => Err(Error::new(504, "The executable timed out.")),
            }
        }
    }
}
pub async fn read_bounded(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(Error::new(
            502,
            "Executable output exceeded the supported limit.",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawned_processes_never_receive_archive_credentials() {
        let env = Environment::from([
            ("AWS_SECRET_ACCESS_KEY".into(), "secret".into()),
            ("ARCHIVE_S3_BUCKET".into(), "bucket".into()),
            ("STORAGE_S3_BUCKET".into(), "bucket".into()),
            ("KEPT".into(), "visible".into()),
        ]);
        let output = command("env", &[], &env, None).output().await.unwrap();
        let printed = String::from_utf8(output.stdout).unwrap();
        assert!(printed.contains("KEPT=visible"));
        assert!(!printed.contains("AWS_"), "{printed}");
        assert!(!printed.contains("ARCHIVE_"), "{printed}");
        assert!(!printed.contains("STORAGE_S3_"), "{printed}");
    }
}
