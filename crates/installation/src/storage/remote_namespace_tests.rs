//! Thread signal masks safely model the namespace inheritance of a new worker.
//! No mount namespace, system service or live storage is modified by this test.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

fn backend_marker() -> bool {
    unsafe {
        let mut mask = std::mem::zeroed();
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask),
            0
        );
        libc::sigismember(&mask, libc::SIGUSR2) == 1
    }
}

#[test]
fn direct_reads_do_not_create_host_workers_from_backend_thread_context() {
    assert!(!backend_marker());
    let inherited_backend_context = Arc::new(AtomicBool::new(false));
    let observed = inherited_backend_context.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .on_thread_start(move || {
            if backend_marker() {
                observed.store(true, Ordering::SeqCst);
            }
        })
        .build()
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let plaintext = b"verified direct block from the private backend caller";
    let hash = super::super::digest::block(plaintext);
    let object = crate::config::id();
    let key = [7; 32];
    let scope = crate::nodes::shared_blocks::object_key(3, &hash, &object);
    let encoded = super::super::block_codec::encode_direct(&key, &scope, &hash, plaintext).unwrap();
    let descriptor = serde_json::to_vec(&super::super::direct::Read {
        object,
        hash: hash.clone(),
        size: plaintext.len() as u64,
        key: STANDARD.encode(key),
        expires_at: crate::config::now() + 30_000,
        get: super::super::direct::Request {
            url: format!("{origin}object"),
            headers: BTreeMap::new(),
            installation_local: false,
        },
    })
    .unwrap();
    let expected = format!("/internal/node-restore/{hash}?direct=1");
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        for index in 0..2 {
            let mut connection = loop {
                match listener.accept() {
                    Ok((connection, _)) => break connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "Missing direct-read fixture request"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("Direct-read fixture failed: {error}"),
                }
            };
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(connection.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(
                line.split_whitespace().nth(1),
                Some(if index == 0 {
                    expected.as_str()
                } else {
                    "/object"
                })
            );
            loop {
                line.clear();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            let (body, marker) = if index == 0 {
                (descriptor.as_slice(), "x-cairn-storage-transfer: v3\r\n")
            } else {
                (encoded.as_slice(), "")
            };
            write!(
                connection,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{marker}Connection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            connection.write_all(body).unwrap();
        }
    });
    let stop = CancellationToken::new();
    let (finished, completion) = std::sync::mpsc::channel();
    let deadline = stop.clone();
    let watchdog = std::thread::spawn(move || {
        if completion.recv_timeout(Duration::from_secs(10)).is_err() {
            deadline.cancel();
        }
    });
    let source = RemoteSource::new(
        &json!({ "master": origin, "grant": "synthetic-direct-read-grant" }),
        runtime.handle().clone(),
        stop,
    )
    .unwrap();
    let read = std::thread::spawn(move || {
        // Like CLONE_NEWNS, a signal mask is inherited by new OS threads. The
        // marker exists only on this disposable caller and its descendants.
        unsafe {
            let mut mask = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGUSR2);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()),
                0
            );
        }
        source.fetch(&hash)
    })
    .join();
    let _ = finished.send(());
    watchdog.join().unwrap();
    server.join().unwrap();
    assert_eq!(read.unwrap().unwrap(), plaintext);
    assert!(
        !inherited_backend_context.load(Ordering::SeqCst),
        "Direct read created a reusable host-runtime worker from the backend thread's private execution context"
    );
}
