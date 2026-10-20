//! Runtime configuration stays outside the journal. Bind only the generated
//! files into the durable home; skills are one immutable per-attempt directory.
use crate::{
    error::{Error, Result},
    skills::private_dir,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(super) const CLAUDE_AUTH_DIRECTORY: &str = "/run/cairn-claude-auth";

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
        if relative == Path::new(".claude/.credentials.json") {
            // The secure-store override reads this through transient storage.
            // Never bind a generated credential onto the journal.
            if !metadata.is_file() {
                return Err(Error::bad("Native Claude credential seed was redirected."));
            }
            continue;
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
                // Keep all user/provider state durable, including loose files.
                // The credential store needs its own transient writable parent
                // for atomic replacements by the broker and CLI.
                bindings.push(Binding {
                    source: path.clone(),
                    target: CLAUDE_AUTH_DIRECTORY.into(),
                    read_only: false,
                });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn claude_loose_user_files_survive_fresh_attempt_staging() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("persistent");
        private_dir(&home.join(".claude")).await.unwrap();
        tokio::fs::write(
            home.join(".claude/CLAUDE.md"),
            b"migrated user instructions",
        )
        .await
        .unwrap();
        let owner = std::fs::metadata(root.path()).unwrap();

        for attempt in ["first", "resumed"] {
            let source = root.path().join(attempt);
            private_dir(&source.join(".claude")).await.unwrap();
            tokio::fs::write(source.join(".claude/.credentials.json"), b"managed secret")
                .await
                .unwrap();
            let bindings = prepare(&source, &home, owner.uid(), owner.gid())
                .await
                .unwrap();

            for file in ["CLAUDE.md", "user-state.sqlite"] {
                let target = Path::new("/home/node/.claude").join(file);
                assert!(
                    bindings
                        .iter()
                        .all(|binding| !target.starts_with(&binding.target))
                );
            }
            assert_eq!(
                tokio::fs::read(home.join(".claude/CLAUDE.md"))
                    .await
                    .unwrap(),
                b"migrated user instructions"
            );
            if attempt == "first" {
                tokio::fs::write(home.join(".claude/user-state.sqlite"), b"new durable state")
                    .await
                    .unwrap();
            } else {
                assert_eq!(
                    tokio::fs::read(home.join(".claude/user-state.sqlite"))
                        .await
                        .unwrap(),
                    b"new durable state"
                );
            }
            assert!(!home.join(".claude/.credentials.json").exists());
        }
    }

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
        assert!(bindings.iter().any(|b| b.source == source.join(".claude")
            && b.target == Path::new(CLAUDE_AUTH_DIRECTORY)
            && !b.read_only));
        assert!(
            bindings
                .iter()
                .all(|b| b.target != Path::new("/home/node/.claude"))
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
