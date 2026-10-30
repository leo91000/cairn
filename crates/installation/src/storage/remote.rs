//! Cancellable block transfers; a transient outage never becomes a zero-filled read.
use super::{
    BlockSource,
    reports::{Report, Reports},
};
use serde_json::{Value, json};
use std::{
    io,
    path::Path,
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct RemoteSource {
    origin: url::Url,
    credential: String,
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
    stop: CancellationToken,
    read_stop: Option<CancellationToken>,
    waiting: Arc<AtomicUsize>,
    reports: Arc<Reports>,
}

struct Waiting<'a>(&'a AtomicUsize);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl RemoteSource {
    pub fn new(
        context: &Value,
        runtime: tokio::runtime::Handle,
        stop: CancellationToken,
    ) -> crate::error::Result<Self> {
        let credential = context["grant"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 1024)
            .ok_or_else(|| crate::error::Error::bad("Missing disk read authorization."))?
            .to_owned();
        Ok(Self {
            origin: crate::nodes::connector::disk_master(crate::validation::text(
                context, "master",
            ))?,
            credential,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(crate::error::Error::internal)?,
            runtime,
            stop,
            read_stop: None,
            waiting: Arc::new(AtomicUsize::new(0)),
            reports: Arc::new(Reports::default()),
        })
    }

    pub(crate) fn persistent(mut self, directory: &Path) -> crate::error::Result<Self> {
        self.reports = Arc::new(Reports::open(directory)?);
        Ok(self)
    }

    pub(crate) fn replay(&self) {
        let source = self.clone();
        self.runtime.spawn(async move {
            let _ = source.reconcile().await;
        });
    }

    pub(crate) fn acknowledge(
        &self,
        state: &Path,
        disk: &super::LazyDisk,
        generation: i64,
        publication: &str,
    ) -> crate::error::Result<()> {
        let fences = super::reports::fences(state)?;
        let until = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            // A corrupt read may be awaiting manager reconciliation while the
            // manager owns ACK admission. Refuse promptly rather than waiting
            // on that read's journal lock and creating a distributed cycle.
            if self.reports.pending() {
                return Err(crate::error::Error::unavailable(
                    "Disk integrity reports await manager acknowledgement.",
                ));
            }
            match disk.try_commit_published_guarded(generation, publication, || {
                let fence = fences.guard(publication).map_err(io::Error::other)?;
                let report = self.reports.guard(publication).map_err(io::Error::other)?;
                Ok((fence, report))
            }) {
                Ok(()) => return Ok(()),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < until =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Err(crate::error::Error::unavailable(
                        "Journal readers must finish before acknowledgement.",
                    ));
                }
                Err(error) => return Err(crate::error::Error::internal(error)),
            }
        }
    }

    async fn reconcile(&self) -> io::Result<()> {
        let _sending = self.unless_stopped(self.reports.sending.lock()).await?;
        let url = self
            .origin
            .join("internal/node-restore/report")
            .map_err(io::Error::other)?;
        let mut delay = Duration::from_millis(100);
        while let Some(report) = self.reports.next().map_err(io::Error::other)? {
            let response = self
                .unless_stopped(
                    self.client
                        .post(url.clone())
                        .bearer_auth(&self.credential)
                        .json(&report)
                        .send(),
                )
                .await?;
            let confirmed = match response {
                Ok(response) if response.status().is_success() => self
                    .unless_stopped(crate::nodes::snapshots::response_block(response))
                    .await?
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .is_some_and(|value| value["invalidated"] == true),
                _ => false,
            };
            if confirmed {
                let reports = self.reports.clone();
                tokio::task::spawn_blocking(move || reports.settled(&report))
                    .await
                    .map_err(io::Error::other)?
                    .map_err(io::Error::other)?;
                delay = Duration::from_millis(100);
                continue;
            }
            self.unless_stopped(tokio::time::sleep(delay)).await?;
            delay = (delay * 2).min(Duration::from_secs(5));
        }
        Ok(())
    }

    pub fn grant_id(&self) -> String {
        crate::auth::digest(&self.credential)
    }

    pub fn waiting(&self) -> bool {
        self.waiting.load(Ordering::SeqCst) > 0 || self.reports.pending()
    }

    fn interrupted() -> io::Error {
        io::Error::new(io::ErrorKind::Interrupted, "Disk read cancelled")
    }

    fn unavailable() -> io::Error {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "Storage authorization temporarily unavailable",
        )
    }

    fn revoked() -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Disk read authorization revoked",
        )
    }

    async fn unless_stopped<T>(&self, future: impl Future<Output = T>) -> io::Result<T> {
        let read_stopped = async {
            match &self.read_stop {
                Some(stop) => stop.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = self.stop.cancelled() => Err(Self::interrupted()),
            () = read_stopped => Err(Self::interrupted()),
            value = future => Ok(value),
        }
    }

    async fn renew(&self) -> io::Result<()> {
        let url = self
            .origin
            .join("internal/node-restore/renew")
            .map_err(io::Error::other)?;
        let request = self
            .client
            .post(url)
            .bearer_auth(&self.credential)
            .json(&json!({}))
            .send();
        let response = self
            .unless_stopped(request)
            .await?
            .map_err(|_| Self::unavailable())?;
        if response.status().is_success() {
            Ok(())
        } else if matches!(response.status().as_u16(), 401 | 403 | 404) {
            Err(Self::revoked())
        } else {
            Err(Self::unavailable())
        }
    }

    /// A block from a response, `None` when the read should be retried.
    async fn receive(&self, response: reqwest::Response) -> io::Result<Option<Vec<u8>>> {
        let status = response.status();
        if status.is_success() {
            if response
                .headers()
                .get(super::direct::RESPONSE_HEADER)
                .is_some_and(|value| value == "v3")
            {
                let descriptor = self
                    .unless_stopped(crate::nodes::snapshots::response_block(response))
                    .await?
                    .map_err(io::Error::other)?;
                let read: super::direct::Read =
                    serde_json::from_slice(&descriptor).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid direct storage authorization",
                        )
                    })?;
                let report = Report {
                    object: read.object.clone(),
                    hash: read.hash.clone(),
                };
                return match self.unless_stopped(super::direct::download(read)).await? {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(error) if matches!(error.status, 400 | 409) => {
                        let reports = self.reports.clone();
                        // Once corruption is detected, cancellation cannot drop
                        // its identity before durable persistence completes.
                        tokio::task::spawn_blocking(move || reports.record(&report))
                            .await
                            .map_err(io::Error::other)?
                            .map_err(io::Error::other)?;
                        self.reconcile().await?;
                        Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Direct storage block integrity failed",
                        ))
                    }
                    Err(_) => Ok(None),
                };
            }
            let transfer = crate::nodes::snapshots::response_block(response);
            return match self.unless_stopped(transfer).await? {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.status == 400 => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid remote disk block",
                )),
                Err(_) => Ok(None),
            };
        }
        match status.as_u16() {
            401 => match self.renew().await {
                Err(error) if error.kind() != io::ErrorKind::WouldBlock => Err(error),
                _ => Ok(None),
            },
            403 | 404 => Err(Self::revoked()),
            408 | 429 => Ok(None),
            _ if status.is_server_error() => Ok(None),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Remote disk block rejected",
            )),
        }
    }
}

impl BlockSource for RemoteSource {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        use crate::performance::publication::{self as diagnostic, Stage};

        if !crate::nodes::snapshots::valid_hash(hash) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid block digest",
            ));
        }
        let url = self
            .origin
            .join(&format!("internal/node-restore/{hash}?direct=1"))
            .map_err(io::Error::other)?;
        let sample = diagnostic::start(Stage::NodeBaseRead);
        let metrics = diagnostic::current();
        let mut source = self.clone();
        source.read_stop = super::read_scope::current();
        let result = self
            .runtime
            .block_on(self.runtime.spawn(diagnostic::scope(metrics, async move {
                // ublk callers have a private mount namespace. Poll storage work on
                // the host executor so lazy blocking-pool threads inherit that host
                // namespace, rather than the backend caller's namespace.
                source.reconcile().await?;
                let mut waiting = None;
                let mut delay = Duration::from_millis(100);
                loop {
                    let network = diagnostic::start(Stage::NodeBaseRequest);
                    let request = diagnostic::restore_request(
                        source
                            .client
                            .get(url.clone())
                            .bearer_auth(&source.credential),
                    )
                    .send();
                    if let Ok(response) = source.unless_stopped(request).await?
                        && let Some(bytes) = source.receive(response).await?
                    {
                        network.finish(bytes.len());
                        return Ok(bytes);
                    }
                    drop(network);
                    if waiting.is_none() {
                        source.waiting.fetch_add(1, Ordering::SeqCst);
                        waiting = Some(Waiting(&source.waiting));
                    }
                    let wait = diagnostic::start(Stage::NodeRetryWait);
                    source.unless_stopped(tokio::time::sleep(delay)).await?;
                    wait.finish(0);
                    delay = (delay * 2).min(Duration::from_secs(5));
                }
            })))
            .map_err(io::Error::other)?;
        if let Ok(bytes) = &result {
            sample.finish(bytes.len());
        }
        result
    }
}

#[cfg(test)]
#[path = "remote_namespace_tests.rs"]
mod namespace_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};

    #[tokio::test]
    async fn failed_report_survives_cancellation_and_replays_without_another_read() {
        use axum::{
            Json, Router,
            http::StatusCode,
            routing::{get, post},
        };
        use base64::{Engine, engine::general_purpose::STANDARD};
        let root = tempfile::tempdir().unwrap();
        let accepted = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let reported = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let object = crate::config::id();
        let hash = format!("b3-{}", "a".repeat(64));
        let descriptor = super::super::direct::Read {
            object: object.clone(),
            hash: hash.clone(),
            size: 4096,
            key: STANDARD.encode([7; 32]),
            expires_at: crate::config::now() + 60_000,
            get: super::super::direct::Request {
                url: format!("{origin}missing"),
                headers: std::collections::BTreeMap::new(),
                installation_local: false,
            },
        };
        let server = Router::new()
            .route(
                "/internal/node-restore/{hash}",
                get(move || {
                    let descriptor = descriptor.clone();
                    async move {
                        (
                            [(super::super::direct::RESPONSE_HEADER, "v3")],
                            Json(descriptor),
                        )
                    }
                }),
            )
            .route("/missing", get(|| async { StatusCode::NOT_FOUND }))
            .route(
                "/internal/node-restore/report",
                post({
                    let (accepted, requests, reported) =
                        (accepted.clone(), requests.clone(), reported.clone());
                    move |Json(report): Json<Value>| {
                        let (accepted, requests, reported) =
                            (accepted.clone(), requests.clone(), reported.clone());
                        let object = object.clone();
                        async move {
                            assert_eq!(report["object"], object);
                            requests.fetch_add(1, Ordering::SeqCst);
                            reported.notify_one();
                            if accepted.load(Ordering::SeqCst) {
                                (StatusCode::OK, Json(json!({"invalidated":true})))
                            } else {
                                (
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    Json(json!({"invalidated":false})),
                                )
                            }
                        }
                    }
                }),
            );
        let serving = tokio::spawn(async { axum::serve(listener, server).await.unwrap() });
        let context = json!({"master":origin,"grant":"synthetic-report-grant"});
        let stop = CancellationToken::new();
        let source = Arc::new(
            RemoteSource::new(&context, tokio::runtime::Handle::current(), stop.clone())
                .unwrap()
                .persistent(root.path())
                .unwrap(),
        );
        let fetching = source.clone();
        let read = tokio::task::spawn_blocking(move || fetching.fetch(&hash));
        tokio::time::timeout(Duration::from_secs(5), reported.notified())
            .await
            .unwrap();
        assert!(!read.is_finished());
        assert!(source.waiting());
        assert!(
            source
                .reports
                .acknowledge::<()>(|| panic!("Unreported corruption reclaimed its journal"))
                .is_err()
        );
        stop.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), read)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        drop(source);
        let restarted = RemoteSource::new(
            &context,
            tokio::runtime::Handle::current(),
            CancellationToken::new(),
        )
        .unwrap()
        .persistent(root.path())
        .unwrap();
        assert!(restarted.waiting());
        accepted.store(true, Ordering::SeqCst);
        restarted.replay();
        tokio::time::timeout(Duration::from_secs(5), async {
            while restarted.waiting() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(requests.load(Ordering::SeqCst) >= 2);
        restarted.reports.acknowledge(|| Ok(())).unwrap();
        assert!(!Reports::open(root.path()).unwrap().pending());
        serving.abort();
    }

    #[tokio::test]
    async fn outage_waits_then_recovers_and_cancellation_interrupts_a_waiting_read() {
        use axum::{Router, http::StatusCode, routing::get};
        let offline = Arc::new(AtomicBool::new(true));
        let flag = offline.clone();
        let server = Router::new().route(
            "/internal/node-restore/{hash}",
            get(move || {
                let flag = flag.clone();
                async move {
                    if flag.load(Ordering::SeqCst) {
                        (StatusCode::SERVICE_UNAVAILABLE, b"".as_slice())
                    } else {
                        (StatusCode::OK, b"verified later by the disk".as_slice())
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let serving = tokio::spawn(async { axum::serve(listener, server).await.unwrap() });
        let stop = CancellationToken::new();
        let source = Arc::new(
            RemoteSource::new(
                &json!({
                    "master": origin,
                    "grant": "fixture-scoped-token"
                }),
                tokio::runtime::Handle::current(),
                stop.clone(),
            )
            .unwrap(),
        );
        let read = {
            let source = source.clone();
            tokio::task::spawn_blocking(move || source.fetch(&format!("b3-{}", "a".repeat(64))))
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !source.waiting() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(!read.is_finished());
        offline.store(false, Ordering::SeqCst);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), read)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            b"verified later by the disk"
        );
        assert!(!source.waiting());
        offline.store(true, Ordering::SeqCst);
        let read = {
            let source = source.clone();
            tokio::task::spawn_blocking(move || source.fetch(&format!("b3-{}", "a".repeat(64))))
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !source.waiting() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        stop.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), read)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(!source.waiting());
        serving.abort();
    }
}
