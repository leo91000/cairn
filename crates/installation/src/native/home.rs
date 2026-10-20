//! Runtime configuration stays outside the journal. Bind only the generated
//! files into the durable home; skills are one immutable per-attempt directory.
use crate::{
    error::{Error, Result},
    skills::private_dir,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
pub struct Binding {
    pub source: PathBuf,
    pub target: PathBuf,
    pub read_only: bool,
}

pub async fn prepare(source: &Path, home: &Path, uid: u32, gid: u32) -> Result<Vec<Binding>> {
    // Validate ancestors before creating generated directories: staging is input,
    // and chmod/create_dir_all must never follow a redirected source.
    for directory in [
        source.to_owned(),
        source.join(".agents"),
        source.join(".agents/skills"),
    ] {
        match tokio::fs::symlink_metadata(&directory).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(Error::bad("Native runtime seed was redirected.")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                private_dir(&directory).await?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut bindings = Vec::new();
    let mut pending = vec![source.to_owned()];
    while let Some(path) = pending.pop() {
        let relative = path.strip_prefix(source).map_err(Error::internal)?;
        let destination = home.join(relative);
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        if metadata.file_type().is_symlink() {
            return Err(Error::bad("Native runtime seed contains a symlink."));
        }
        if metadata.is_dir() {
            match tokio::fs::symlink_metadata(&destination).await {
                Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                    return Err(Error::bad("Native runtime home was redirected."));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    private_dir(&destination).await?;
                }
                Err(error) => return Err(error.into()),
            }
            std::os::unix::fs::lchown(&destination, Some(uid), Some(gid))?;
            if relative == Path::new(".claude") {
                claude(&path, &destination, uid, gid, &mut bindings).await?;
                continue;
            }
            if relative == Path::new(".agents/skills") {
                bindings.push(Binding {
                    source: path.clone(),
                    target: Path::new("/home/node").join(relative),
                    read_only: true,
                });
                continue;
            }
            let mut entries = tokio::fs::read_dir(path).await?;
            while let Some(entry) = entries.next_entry().await? {
                pending.push(entry.path());
            }
        } else if metadata.is_file() {
            if let Ok(metadata) = tokio::fs::symlink_metadata(&destination).await {
                if metadata.is_dir() {
                    return Err(Error::bad("Native configuration target is a directory."));
                }
                tokio::fs::remove_file(&destination).await?;
            }
            // Only an empty mount target reaches the persistent filesystem.
            // Config bytes, credentials and frozen skills remain in staging.
            tokio::fs::File::create(&destination).await?;
            bindings.push(Binding {
                source: path.clone(),
                target: Path::new("/home/node").join(relative),
                read_only: true,
            });
        }
    }
    Ok(bindings)
}

/// Claude's broker atomically replaces credentials at runtime. Its parent must
/// be writable ephemeral storage; a readonly file mount cannot intercept rename.
/// Persist session/tool directories through child binds, keeping the runtime
/// credential and generated account metadata outside the journal throughout.
async fn claude(
    source: &Path,
    home: &Path,
    uid: u32,
    gid: u32,
    bindings: &mut Vec<Binding>,
) -> Result<()> {
    bindings.push(Binding {
        source: source.into(),
        target: "/home/node/.claude".into(),
        read_only: false,
    });
    let mut directories = std::collections::BTreeSet::new();
    // Existing private user/plugin directories survive conversion even when
    // their names are not part of the coding engine's standard session layout.
    let mut existing = tokio::fs::read_dir(home).await?;
    while let Some(entry) = existing.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            directories.insert(entry.file_name());
        }
    }
    for name in [
        "sessions",
        "archived_sessions",
        "projects",
        "todos",
        "session-env",
        "shell-snapshots",
        "plans",
        "tasks",
        "plugins",
        "skills",
        "agents",
        "commands",
        "output-styles",
        "hooks",
    ] {
        directories.insert(std::ffi::OsString::from(name));
    }
    for name in directories {
        for path in [home.join(&name), source.join(&name)] {
            match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(Error::bad(
                        "Native Claude session directory was redirected.",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    private_dir(&path).await?;
                }
                Err(error) => return Err(error.into()),
            }
            std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
        }
        bindings.push(Binding {
            source: home.join(&name),
            target: Path::new("/home/node/.claude").join(name),
            read_only: false,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn managed_claude_runtime_refresh_stays_ephemeral_while_sessions_are_durable() {
        use std::os::unix::fs::MetadataExt;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("stage");
        let home = root.path().join("persistent");
        private_dir(&source.join(".claude")).await.unwrap();
        private_dir(&home.join(".claude/projects")).await.unwrap();
        tokio::fs::write(home.join(".claude/projects/session"), b"durable session")
            .await
            .unwrap();
        let identity = std::fs::metadata(root.path()).unwrap();
        let bindings = prepare(&source, &home, identity.uid(), identity.gid())
            .await
            .unwrap();
        assert!(
            bindings
                .iter()
                .any(|b| b.source == source.join(".claude") && !b.read_only)
        );
        assert!(
            bindings
                .iter()
                .any(|b| b.source == home.join(".claude/projects") && !b.read_only)
        );
        let listener =
            tokio::net::UnixListener::bind(source.join(".claude/cairn-auth.sock")).unwrap();
        let broker = tokio::spawn(async move {
            for token in ["synthetic-claude-first", "synthetic-claude-refreshed"] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = tokio::io::BufReader::new(stream);
                let mut request = String::new();
                stream.read_line(&mut request).await.unwrap();
                let response = serde_json::json!({ "claudeAiOauth": {
                    "accessToken": token, "expiresAt": crate::config::now() + 3600000,
                    "refreshToken": "manager-only-refresh"
                } });
                stream
                    .get_mut()
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let mut client = crate::accounts::claude::Client::new(&source.join(".claude"));
        client.sync().await.unwrap();
        client.sync().await.unwrap();
        broker.await.unwrap();
        let bytes = tokio::fs::read(source.join(".claude/.credentials.json"))
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["claudeAiOauth"]["accessToken"],
            "synthetic-claude-refreshed"
        );
        assert!(value["claudeAiOauth"]["refreshToken"].is_null());
        assert!(!home.join(".claude/.credentials.json").exists());
        assert_eq!(
            tokio::fs::read(home.join(".claude/projects/session"))
                .await
                .unwrap(),
            b"durable session"
        );
    }

    #[tokio::test]
    async fn generated_secrets_and_frozen_skills_never_reach_persistent_targets() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("stage");
        let home = root.path().join("persistent");
        private_dir(&source.join(".codex")).await.unwrap();
        private_dir(&source.join(".config/gh")).await.unwrap();
        private_dir(&home.join(".codex/sessions")).await.unwrap();
        tokio::fs::write(source.join(".codex/auth.json"), b"synthetic account secret")
            .await
            .unwrap();
        tokio::fs::write(
            source.join(".config/gh/hosts.yml"),
            b"synthetic github secret",
        )
        .await
        .unwrap();
        tokio::fs::write(home.join(".codex/sessions/user"), b"durable session")
            .await
            .unwrap();
        let metadata = std::fs::metadata(root.path()).unwrap();
        let bindings = prepare(&source, &home, metadata.uid(), metadata.gid())
            .await
            .unwrap();
        for path in [".codex/auth.json", ".config/gh/hosts.yml"] {
            assert!(tokio::fs::read(home.join(path)).await.unwrap().is_empty());
            assert!(
                bindings
                    .iter()
                    .any(|binding| binding.source == source.join(path)
                        && binding.target == Path::new("/home/node").join(path))
            );
        }
        assert!(
            bindings
                .iter()
                .any(|binding| binding.target == Path::new("/home/node/.agents/skills"))
        );
        assert_eq!(
            tokio::fs::read(home.join(".codex/sessions/user"))
                .await
                .unwrap(),
            b"durable session"
        );
    }

    #[tokio::test]
    async fn runtime_bindings_refuse_redirected_source_before_creating_skills() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("stage");
        let home = root.path().join("persistent");
        let outside = root.path().join("outside");
        for directory in [&source, &home, &outside] {
            private_dir(directory).await.unwrap();
        }
        std::os::unix::fs::symlink(&outside, source.join(".agents")).unwrap();
        let metadata = std::fs::metadata(root.path()).unwrap();
        assert!(
            prepare(&source, &home, metadata.uid(), metadata.gid())
                .await
                .is_err()
        );
        assert!(!outside.join("skills").exists());
    }

    #[tokio::test]
    async fn runtime_bindings_refuse_redirected_home_ancestors() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("stage");
        let home = root.path().join("persistent");
        let outside = root.path().join("outside");
        for directory in [&source.join(".config/gh"), &home, &outside] {
            private_dir(directory).await.unwrap();
        }
        tokio::fs::write(source.join(".config/gh/hosts.yml"), b"synthetic secret")
            .await
            .unwrap();
        tokio::fs::write(outside.join("keep"), b"owner files")
            .await
            .unwrap();
        std::os::unix::fs::symlink(&outside, home.join(".config")).unwrap();
        let metadata = std::fs::metadata(root.path()).unwrap();
        assert!(
            prepare(&source, &home, metadata.uid(), metadata.gid())
                .await
                .is_err()
        );
        assert_eq!(
            tokio::fs::read(outside.join("keep")).await.unwrap(),
            b"owner files"
        );
        assert!(!outside.join("gh").exists());
    }
}
