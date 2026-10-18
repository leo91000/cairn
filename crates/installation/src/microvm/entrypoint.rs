//! Reuse the running guest's executable only when its bytes match the controller.
use crate::error::{Error, Result};
use std::{fs, io, os::unix::fs::PermissionsExt, path::Path};
use tokio::sync::OnceCell;

static DIGEST: OnceCell<String> = OnceCell::const_new();

pub(super) async fn digest() -> Result<&'static str> {
    DIGEST
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| {
                let mut file = fs::File::open("/proc/self/exe")?;
                let mut digest = blake3::Hasher::new();
                digest.update_reader(&mut file)?;
                Ok::<_, io::Error>(digest.finalize().to_hex().to_string())
            })
            .await
            .map_err(Error::internal)?
            .map_err(Error::from)
        })
        .await
        .map(String::as_str)
}

pub(super) async fn reuse(expected: &str) -> Result<bool> {
    if digest().await? != expected {
        return Ok(false);
    }
    // /proc/self/exe pins the running inode even if a saved overlay shadows the
    // original image path. Never trust /usr/local/bin/cairn on the writable disk.
    tokio::task::spawn_blocking(|| {
        install(
            Path::new("/proc/self/exe"),
            Path::new(super::plan::ENTRYPOINT),
        )
    })
    .await
    .map_err(Error::internal)??;
    Ok(true)
}

fn install(source: &Path, target: &Path) -> io::Result<()> {
    let directory = target.parent().unwrap();
    let staging = tempfile::Builder::new()
        .prefix(".cairn-entrypoint-")
        .tempdir_in(directory.parent().unwrap())?;
    let binary = staging.path().join(target.file_name().unwrap());
    fs::copy(source, &binary)?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o755))?;
    match fs::remove_dir_all(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(staging.path(), directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mismatch_does_not_install_an_obsolete_executable() {
        assert!(!reuse("different-controller").await.unwrap());
    }

    #[test]
    fn installation_replaces_untrusted_directory_without_following_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, "current-runner").unwrap();
        let private = root.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::write(private.join("cairn"), "preserved").unwrap();
        let directory = root.path().join("entrypoint");
        std::os::unix::fs::symlink(&private, &directory).unwrap();

        let target = directory.join("cairn");
        install(&source, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "current-runner");
        assert_eq!(
            fs::read_to_string(private.join("cairn")).unwrap(),
            "preserved"
        );
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!directory.is_symlink());

        fs::write(&target, "modified-runner").unwrap();
        install(&source, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "current-runner");
    }
}
