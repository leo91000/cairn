//! One response owns a publication's reader across bounded transfer requests.
//! The registry is weak: disconnecting the response releases an idle journal.
use super::{Reader, diagnostic};
use crate::error::{Error, Result};
use axum::body::{Body, Bytes};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 16;

enum Entry {
    Opening(String),
    Open(String, Weak<Session>),
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    static READERS: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    READERS.get_or_init(Default::default)
}

struct Registration {
    directory: PathBuf,
    id: String,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut entries = registry().lock().unwrap();
        let matches = entries.get(&self.directory).is_some_and(|entry| {
            let (Entry::Opening(id) | Entry::Open(id, _)) = entry;
            id == &self.id
        });
        if matches {
            entries.remove(&self.directory);
        }
    }
}

pub(super) struct Session {
    // Drop the reader (including physical ownership) before removing its entry.
    pub(super) reader: Arc<Reader>,
    closed: CancellationToken,
    _registration: Registration,
}

impl Session {
    pub(super) fn read_stop(&self) -> CancellationToken {
        self.closed.child_token()
    }
}

struct ResponseOwner(Arc<Session>);

impl Drop for ResponseOwner {
    fn drop(&mut self) {
        // Uploads retain Session until their started PUTs drain, but abandoned
        // cold reads must stop even while those uploads still own the reader.
        self.0.closed.cancel();
    }
}

pub(super) fn get(directory: &Path) -> Option<Arc<Session>> {
    let session = {
        let entries = registry().lock().unwrap();
        match entries.get(directory)? {
            Entry::Open(_, session) => session.clone(),
            Entry::Opening(_) => return None,
        }
    };
    // A rejected last strong reference drops Registration and takes the map
    // mutex again, so never upgrade/filter while holding that mutex.
    session
        .upgrade()
        .filter(|session| !session.closed.is_cancelled())
}

async fn open(directory: PathBuf, request: Option<diagnostic::Request>) -> Result<Arc<Session>> {
    let registration = {
        let mut entries = registry().lock().unwrap();
        entries.retain(|_, entry| match entry {
            Entry::Opening(_) => true,
            Entry::Open(_, session) => session.strong_count() > 0,
        });
        if entries.contains_key(&directory) {
            return Err(Error::conflict(
                "Snapshot publication reader is already open.",
            ));
        }
        if entries.len() >= LIMIT {
            return Err(Error::unavailable(
                "Node publication reader capacity reached.",
            ));
        }
        let id = crate::config::id();
        entries.insert(directory.clone(), Entry::Opening(id.clone()));
        Registration {
            directory: directory.clone(),
            id,
        }
    };
    let snapshot = directory.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let mut summary = diagnostic::NodeSummary::begin(request, snapshot);
    let metrics = summary.as_ref().map(|s| s.metrics.clone());
    let reader = diagnostic::scope(metrics, async {
        let sample = diagnostic::start(diagnostic::Stage::NodeOpen);
        let reader = Reader::open(&directory).await?;
        sample.finish(0);
        Ok::<_, Error>(Arc::new(reader))
    })
    .await?;
    if let Some(summary) = &mut summary {
        summary.complete();
    }
    let session = Arc::new(Session {
        reader,
        closed: CancellationToken::new(),
        _registration: registration,
    });
    registry().lock().unwrap().insert(
        directory,
        Entry::Open(session._registration.id.clone(), Arc::downgrade(&session)),
    );
    Ok(session)
}

pub(super) async fn response(
    directory: &Path,
    request: Option<diagnostic::Request>,
) -> Result<Body> {
    let directory = directory.to_owned();
    // An interrupted HTTP caller must not release stopped ownership while a
    // blocking journal recovery is still running.
    let session = tokio::spawn(open(directory, request))
        .await
        .map_err(Error::internal)??;
    let stream = futures_util::stream::unfold(ResponseOwner(session), |owner| async move {
        tokio::select! {
            () = owner.0.closed.cancelled() => None,
            () = tokio::time::sleep(Duration::from_secs(1)) => {
                Some((Ok::<_, std::io::Error>(Bytes::from_static(b" ")), owner))
            }
        }
    });
    Ok(Body::from_stream(stream))
}

pub(super) async fn close(directory: &Path) -> Result<()> {
    let session = {
        let entries = registry().lock().unwrap();
        match entries.get(directory) {
            Some(Entry::Open(id, session)) => Some((id.clone(), session.clone())),
            Some(Entry::Opening(_)) => return Err(Error::conflict("Snapshot reader is opening.")),
            None => None,
        }
    };
    let Some((id, session)) = session else {
        return Ok(());
    };
    if let Some(open) = session.upgrade() {
        open.closed.cancel();
    }
    // The caller may commit/restore after this acknowledgement. Wait for both
    // the response and every already-started transfer to release their reader.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let retained = registry()
                .lock()
                .unwrap()
                .get(directory)
                .is_some_and(|entry| {
                    let (Entry::Opening(current) | Entry::Open(current, _)) = entry;
                    current == &id
                });
            if !retained {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| Error::unavailable("Snapshot reader is still finishing transfers."))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{self, Disk};
    use futures_util::StreamExt;
    use serde_json::{Value, json};

    struct Fixture {
        _root: tempfile::TempDir,
        state: PathBuf,
        run: String,
        disk: PathBuf,
        capture: PathBuf,
        hashes: Vec<String>,
    }

    impl Fixture {
        async fn cold(origin: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let state = root.path().to_owned();
            let run = crate::config::id();
            let directory = state.join("disks").join(&run);
            let hash = storage::digest::block(&[37; 4096]);
            let base = json!({
                "version": 1, "size": 4096, "blockSize": super::super::BLOCK,
                "blocks": [{ "offset": 0, "size": 4096, "hash": hash }]
            });
            let disk = storage::runtime::create(
                &directory.join("lazy"),
                &base,
                &json!({ "master": origin, "grant": "fixture" }),
            )
            .await
            .unwrap();
            let generation = disk.seal().unwrap();
            let mut manifest = disk.capture(generation).unwrap();
            manifest["onDemand"] = true.into();
            manifest["generation"] = generation.into();
            drop(disk);
            let capture = state.join("snapshots").join(crate::config::id());
            tokio::fs::create_dir_all(&capture).await.unwrap();
            tokio::fs::write(capture.join("run"), &run).await.unwrap();
            tokio::fs::write(capture.join("manifest.json"), manifest.to_string())
                .await
                .unwrap();
            Self {
                _root: root,
                state,
                run,
                disk: directory,
                capture,
                hashes: vec![hash],
            }
        }

        fn upload(&self, origin: &str, size: u64) -> tokio::task::JoinHandle<Result<Value>> {
            let input = json!({
                "publicationId": self.capture.file_name().unwrap().to_str().unwrap(),
                "requireReader": true,
                "objects": [{
                    "read": {
                        "object": crate::config::id(), "hash": self.hashes[0], "size": size,
                        "key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                        "expiresAt": crate::config::now() + 180_000,
                        "get": { "url": format!("{origin}/object"), "headers": {} }
                    },
                    "put": { "url": format!("{origin}/object"), "headers": { "if-none-match": "*" } }
                }]
            });
            let capture = self.capture.clone();
            tokio::spawn(async move {
                super::super::upload_direct(&capture, &serde_json::to_vec(&input).unwrap()).await
            })
        }

        async fn new() -> Self {
            Self::with_frames(8, 0).await
        }

        async fn with_frames(count: u64, overwrites: u64) -> Self {
            let root = tempfile::tempdir().unwrap();
            let state = root.path().to_owned();
            let run = crate::config::id();
            let directory = state.join("disks").join(&run);
            let block = super::super::BLOCK;
            let base = json!({
                "version": 1, "size": count * block, "blockSize": block,
                "blocks": (0..count).map(|i| json!({
                    "offset": i * block, "size": block, "hash": null
                })).collect::<Vec<_>>()
            });
            let disk = storage::runtime::create(
                &directory.join("lazy"),
                &base,
                &json!({ "master": "http://127.0.0.1:9/", "grant": "fixture" }),
            )
            .await
            .unwrap();
            let mut bytes = vec![7; block as usize];
            for i in 0..count {
                bytes[..8].copy_from_slice(&i.to_le_bytes());
                disk.write_at(i * block, &bytes).unwrap();
            }
            for i in 0..overwrites {
                bytes[..8].copy_from_slice(&i.to_le_bytes());
                disk.write_at((i % count) * block, &bytes[..4096]).unwrap();
            }
            let generation = disk.seal().unwrap();
            let mut manifest = disk.capture(generation).unwrap();
            manifest["onDemand"] = true.into();
            manifest["generation"] = generation.into();
            drop(disk);
            let capture = state.join("snapshots").join(crate::config::id());
            tokio::fs::create_dir_all(&capture).await.unwrap();
            tokio::fs::write(capture.join("run"), &run).await.unwrap();
            tokio::fs::write(capture.join("manifest.json"), manifest.to_string())
                .await
                .unwrap();
            let hashes = super::super::hashes(&manifest).map(str::to_owned).collect();
            Self {
                _root: root,
                state,
                run,
                disk: directory,
                capture,
                hashes,
            }
        }

        fn drain(body: Body) -> tokio::task::JoinHandle<()> {
            tokio::spawn(async move {
                let mut stream = body.into_data_stream();
                while let Some(chunk) = stream.next().await {
                    chunk.unwrap();
                }
            })
        }
    }

    async fn cold_peer() -> (
        String,
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{body::to_bytes, extract::Request, http::StatusCode, response::IntoResponse};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let outage = Arc::new(AtomicBool::new(true));
        let reads = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().fallback({
            let (outage, reads) = (outage.clone(), reads.clone());
            move |request: Request| {
                let (outage, reads) = (outage.clone(), reads.clone());
                async move {
                    if request.method() == "GET" {
                        reads.fetch_add(1, Ordering::SeqCst);
                        if outage.load(Ordering::SeqCst) {
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        return vec![37_u8; 4096].into_response();
                    }
                    to_bytes(request.into_body(), 8 * 1024 * 1024)
                        .await
                        .unwrap();
                    StatusCode::OK.into_response()
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (origin, outage, reads, server)
    }

    #[tokio::test]
    async fn cold_outage_does_not_starve_local_uploads_and_disconnect_drains_stopped_reads() {
        use std::sync::atomic::Ordering;
        let (origin, outage, reads, server) = cold_peer().await;
        let mut fixtures = Vec::new();
        let mut bodies = Vec::new();
        let mut uploads = Vec::new();
        for _ in 0..crate::storage::direct::BATCH {
            let fixture = Fixture::cold(&origin).await;
            bodies.push(response(&fixture.capture, None).await.unwrap());
            uploads.push(fixture.upload(&origin, 4096));
            fixtures.push(fixture);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while reads.load(Ordering::SeqCst) < crate::storage::direct::BATCH {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let healthy = Fixture::with_frames(1, 0).await;
        let healthy_body = response(&healthy.capture, None).await.unwrap();
        let mut healthy_upload = healthy.upload(&origin, super::super::BLOCK);
        let local_result = tokio::time::timeout(Duration::from_secs(3), &mut healthy_upload).await;
        drop(bodies);
        let closed =
            tokio::time::timeout(Duration::from_secs(3), close(&fixtures[0].capture)).await;
        let released = storage::environment::lock(&fixtures[0].state, &fixtures[0].run, "busy")
            .await
            .is_ok();
        let cancelled = tokio::time::timeout(Duration::from_secs(3), async {
            while !uploads.iter().all(tokio::task::JoinHandle::is_finished) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        // Restore the mock before assertions so even a regression can shut down
        // its blocking workers and report the actual assertion failure.
        outage.store(false, Ordering::SeqCst);
        for upload in uploads {
            let _ = tokio::time::timeout(Duration::from_secs(10), upload)
                .await
                .unwrap()
                .unwrap();
        }
        if local_result.is_err() {
            let _ = tokio::time::timeout(Duration::from_secs(10), healthy_upload)
                .await
                .unwrap();
        }
        assert!(
            matches!(local_result, Ok(Ok(Ok(_)))),
            "Local upload waited for unavailable cold bases"
        );
        assert!(
            matches!(closed, Ok(Ok(()))),
            "Abandoned cold reads retained their reader"
        );
        assert!(
            released,
            "Stopped disk ownership outlived close acknowledgement"
        );
        assert!(
            cancelled,
            "Disconnected cold batches did not finish during the outage"
        );
        drop(healthy_body);
        server.abort();
    }

    #[tokio::test]
    async fn cancelling_a_publication_cache_waiter_preserves_the_foreground_vm_read() {
        use std::sync::atomic::Ordering;
        let (origin, outage, reads, server) = cold_peer().await;
        let fixture = Fixture::cold(&origin).await;
        let lease = storage::environment::lock(&fixture.state, &fixture.run, "busy")
            .await
            .unwrap();
        let volume = storage::runtime::load(&fixture.disk).await.unwrap();
        let disk = volume.disk.clone();
        let foreground = tokio::task::spawn_blocking(move || {
            let mut bytes = vec![0; 4096];
            disk.read_at(0, &mut bytes).map(|()| bytes)
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while reads.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let body = response(&fixture.capture, None).await.unwrap();
        let reader = super::super::upload_reader(&fixture.capture, true)
            .await
            .unwrap();
        let hash = fixture.hashes[0].clone();
        let publication = tokio::spawn(async move { reader.block(&hash).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(body);
        let cancelled = tokio::time::timeout(Duration::from_secs(3), publication).await;
        let vm_retained = !foreground.is_finished() && !volume.stop.is_cancelled();
        outage.store(false, Ordering::SeqCst);
        let bytes = tokio::time::timeout(Duration::from_secs(10), foreground)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(cancelled, Ok(Ok(Err(_)))),
            "Publication remained behind the VM's cache lock"
        );
        assert!(
            vm_retained,
            "Publication cancellation interrupted the foreground VM"
        );
        assert_eq!(bytes, vec![37; 4096]);
        close(&fixture.capture).await.unwrap();
        drop(volume);
        drop(lease);
        server.abort();
    }

    #[tokio::test]
    async fn stopped_publication_reuses_one_volume_across_batches_and_releases_before_close_ack() {
        let fixture = Fixture::new().await;
        let body = response(&fixture.capture, None).await.unwrap();
        let drain = Fixture::drain(body);
        let volume = storage::runtime::load(&fixture.disk).await.unwrap();
        let original = Arc::downgrade(&volume);
        drop(volume);
        for batch in fixture.hashes.chunks(crate::storage::direct::BATCH) {
            let reader = super::super::upload_reader(&fixture.capture, true)
                .await
                .unwrap();
            for hash in batch {
                let bytes = reader.block(hash).await.unwrap();
                assert!(storage::digest::matches(hash, &bytes));
            }
            drop(reader);
            let reused = storage::runtime::load(&fixture.disk).await.unwrap();
            assert!(Weak::ptr_eq(&original, &Arc::downgrade(&reused)));
        }
        assert!(
            storage::environment::lock(&fixture.state, &fixture.run, "busy")
                .await
                .is_err()
        );
        close(&fixture.capture).await.unwrap();
        drain.await.unwrap();
        assert!(original.upgrade().is_none());
        assert!(
            storage::environment::lock(&fixture.state, &fixture.run, "busy")
                .await
                .is_ok()
        );
        assert!(
            super::super::upload_reader(&fixture.capture, true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn close_waits_for_started_reads_and_duplicate_sessions_do_not_share_cancellation() {
        let fixture = Fixture::new().await;
        let body = response(&fixture.capture, None).await.unwrap();
        assert!(response(&fixture.capture, None).await.is_err());
        let drain = Fixture::drain(body);
        let reader = super::super::upload_reader(&fixture.capture, true)
            .await
            .unwrap();
        let capture = fixture.capture.clone();
        let closing = tokio::spawn(async move { close(&capture).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!closing.is_finished());
        assert!(
            storage::environment::lock(&fixture.state, &fixture.run, "busy")
                .await
                .is_err()
        );
        drop(reader);
        closing.await.unwrap().unwrap();
        drain.await.unwrap();
        let replacement = response(&fixture.capture, None).await.unwrap();
        drop(replacement);
        assert!(
            storage::environment::lock(&fixture.state, &fixture.run, "busy")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn manager_http_disconnect_releases_idle_stopped_journal_without_a_timer() {
        use axum::response::IntoResponse;
        let fixture = Fixture::new().await;
        let capture = fixture.capture.clone();
        let app = axum::Router::new().fallback(move || {
            let capture = capture.clone();
            async move { response(&capture, None).await.unwrap().into_response() }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::Client::new().post(url).send().await.unwrap();
        let volume = storage::runtime::load(&fixture.disk).await.unwrap();
        let retained = Arc::downgrade(&volume);
        drop(volume);
        drop(response);
        tokio::time::timeout(Duration::from_secs(3), async {
            while retained.strong_count() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            storage::environment::lock(&fixture.state, &fixture.run, "busy")
                .await
                .is_ok()
        );
        server.abort();
    }

    #[tokio::test]
    async fn discarded_capture_releases_its_reader_before_removing_metadata() {
        let fixture = Fixture::new().await;
        let body = response(&fixture.capture, None).await.unwrap();
        let drain = Fixture::drain(body);
        super::super::discard(&fixture.capture).await.unwrap();
        drain.await.unwrap();
        assert!(!fixture.capture.exists());
        assert!(
            storage::environment::lock(&fixture.state, &fixture.run, "busy")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    #[ignore = "isolated stopped-journal reader CPU/I/O measurement"]
    async fn measure_stopped_direct_reader_recovery() {
        fn cpu_ms() -> f64 {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
            assert_eq!(
                unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
                0
            );
            let usage = unsafe { usage.assume_init() };
            (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
                + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0
        }
        let fixture = Fixture::with_frames(16, 4096).await;
        for trial in 0..4 {
            for pinned in if trial % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let wall = std::time::Instant::now();
                let cpu = cpu_ms();
                let drain = if pinned {
                    Some(Fixture::drain(
                        response(&fixture.capture, None).await.unwrap(),
                    ))
                } else {
                    None
                };
                let mut previous = Weak::new();
                let mut opens = 0;
                let mut bytes = 0;
                for batch in fixture.hashes.chunks(crate::storage::direct::BATCH) {
                    let reader = super::super::upload_reader(&fixture.capture, pinned)
                        .await
                        .unwrap();
                    let volume = storage::runtime::load(&fixture.disk).await.unwrap();
                    let current = Arc::downgrade(&volume);
                    if !Weak::ptr_eq(&previous, &current) {
                        opens += 1;
                    }
                    previous = current;
                    drop(volume);
                    for hash in batch {
                        let data = reader.block(hash).await.unwrap();
                        assert!(storage::digest::matches(hash, &data));
                        bytes += data.len();
                    }
                    drop(reader);
                    assert_eq!(previous.strong_count() > 0, pinned);
                }
                if let Some(drain) = drain {
                    close(&fixture.capture).await.unwrap();
                    drain.await.unwrap();
                }
                assert!(previous.upgrade().is_none());
                println!(
                    "DIRECT_READER_PERF {}",
                    json!({
                        "trial": trial, "warmup": trial == 0, "session": pinned,
                        "journalOpens": opens, "logicalBlocks": 16, "overwriteFrames": 4096,
                        "plaintextReadBytes": bytes, "wallMs": wall.elapsed().as_secs_f64() * 1000.0,
                        "cpuMs": cpu_ms() - cpu,
                        "conditions": "development profile; stopped journal in /tmp; warm OS cache; no HTTP/provider or encoding"
                    })
                );
            }
        }
    }
}
