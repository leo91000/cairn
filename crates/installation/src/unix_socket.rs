//! Unix sockets retain their normal filesystem location even when a long data
//! directory would exceed the kernel's socket-address limit.
use std::{io, path::Path};
use tokio::net::{UnixListener, UnixStream};

#[cfg(target_os = "linux")]
fn address(path: &Path) -> io::Result<(std::path::PathBuf, Option<std::fs::File>)> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    // Linux sockaddr_un has 108 bytes, including the terminating NUL.
    if path.as_os_str().as_bytes().len() < 108 {
        return Ok((path.to_owned(), None));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no name"))?;
    let directory = std::fs::File::open(parent)?;
    let address =
        std::path::PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
    Ok((address, Some(directory)))
}

#[cfg(not(target_os = "linux"))]
fn address(path: &Path) -> io::Result<(std::path::PathBuf, Option<std::fs::File>)> {
    Ok((path.to_owned(), None))
}

pub(crate) fn bind(path: &Path) -> io::Result<UnixListener> {
    let (address, _directory) = address(path)?;
    UnixListener::bind(address)
}

pub(crate) async fn connect(path: &Path) -> io::Result<UnixStream> {
    let (address, _directory) = address(path)?;
    UnixStream::connect(address).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn long_paths_bind_and_connect_at_the_original_location() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("a".repeat(110));
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("auth.sock");
        let listener = bind(&path).unwrap();
        assert!(path.exists());
        let mut client = connect(&path).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        client.write_all(b"ok").await.unwrap();
        let mut bytes = [0; 2];
        server.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ok");
    }
}
