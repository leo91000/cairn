//! Reclaim immutable runtime images only while no references are being created.
use crate::error::Result;
use std::path::Path;

pub(super) static CONTROL: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

pub(super) async fn collect(_state: &Path, _current: &Path) -> Result<usize> {
    let _exclusive = CONTROL.write().await;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collection_preserves_current_and_all_pins_and_removes_unused_images() {
        let root = tempfile::tempdir().unwrap();
        for runtime in [
            "current",
            "disk-pin",
            "environment-pin",
            "template-pin",
            "unused",
        ] {
            let path = root.path().join("images").join(runtime);
            tokio::fs::create_dir_all(&path).await.unwrap();
            tokio::fs::write(path.join("root.ext4"), b"image")
                .await
                .unwrap();
        }
        for (directory, file, record) in [
            (
                "disks/conversation",
                "runtime.json",
                r#"{"runtimeId":"disk-pin"}"#,
            ),
            (
                "environments/environment",
                "runtime.json",
                r#"{"runtimeId":"environment-pin"}"#,
            ),
            (
                "templates/template",
                "key.json",
                r#"{"runtime":"template-pin"}"#,
            ),
        ] {
            let path = root.path().join(directory);
            tokio::fs::create_dir_all(&path).await.unwrap();
            tokio::fs::write(path.join(file), record).await.unwrap();
        }
        let current = root.path().join("images/current");
        assert_eq!(collect(root.path(), &current).await.unwrap(), 1);
        for runtime in ["current", "disk-pin", "environment-pin", "template-pin"] {
            assert!(
                root.path()
                    .join("images")
                    .join(runtime)
                    .join("root.ext4")
                    .exists(),
                "{runtime}"
            );
        }
        assert!(!root.path().join("images/unused").exists());
    }
}
