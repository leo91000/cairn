//! Bounded guest connections, independent of the Unix/vsock transport.
use std::{future::Future, io, time::Duration};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub(super) async fn serve<T, A, F, H, R>(
    mut accept: A,
    handle: H,
    limit: usize,
    stop: CancellationToken,
) -> io::Result<()>
where
    T: Send + 'static,
    A: FnMut() -> F,
    F: Future<Output = io::Result<T>>,
    H: Fn(T) -> R,
    R: Future<Output = ()> + Send + 'static,
{
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            _ = connections.join_next(), if !connections.is_empty() => {},
            accepted = accept(), if connections.len() < limit => {
                match accepted {
                    Ok(stream) => {
                        connections.spawn(handle(stream));
                    },
                    Err(error) if matches!(
                        error.raw_os_error(),
                        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS
                            | libc::ENOMEM | libc::EINTR | libc::ECONNABORTED)
                    ) => {
                        tracing::warn!(%error, "Guest connection acceptance will retry");
                        tokio::select! {
                            () = stop.cancelled() => return Ok(()),
                            () = tokio::time::sleep(Duration::from_millis(250)) => {},
                        }
                    },
                    Err(error) => return Err(error),
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{UnixListener, UnixStream},
    };

    #[tokio::test]
    async fn descriptor_exhaustion_does_not_stop_guest_connections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let attempts = Arc::new(AtomicUsize::new(0));
        let accepted = attempts.clone();
        let server = tokio::spawn(async move {
            serve(
                || async {
                    if accepted.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(io::Error::from_raw_os_error(libc::EMFILE));
                    }
                    listener.accept().await.map(|(stream, _)| stream)
                },
                |mut stream: UnixStream| async move {
                    stream.write_all(b"ready").await.unwrap();
                },
                2,
                stopping,
            )
            .await
        });
        let mut client = UnixStream::connect(path).await.unwrap();
        let mut answer = [0; 5];
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.read_exact(&mut answer),
        )
        .await;
        stop.cancel();
        let result = server.await.unwrap();
        assert!(result.is_ok(), "guest listener exited: {result:?}");
        received.unwrap().unwrap();
        assert_eq!(&answer, b"ready");
        assert!(attempts.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn slow_connections_are_bounded_and_shutdown_cancels_them() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let (started, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            serve(
                || listener.accept(),
                |(mut stream, _)| {
                    let started = started.clone();
                    async move {
                        started.send(()).unwrap();
                        let mut byte = [0];
                        let _ = stream.read_exact(&mut byte).await;
                    }
                },
                2,
                stopping,
            )
            .await
        });
        let mut clients = Vec::new();
        for _ in 0..8 {
            clients.push(UnixStream::connect(&path).await.unwrap());
        }
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(1), requests.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), requests.recv())
                .await
                .is_err()
        );
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(requests.recv().await.is_none());
    }
}
