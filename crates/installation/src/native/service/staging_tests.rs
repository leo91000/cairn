//! Real HTTP seed reception, authoritative budget endpoint and native admission.
use super::*;
use crate::native::staging::Storage;
use axum::routing::{get, post};
use tokio::sync::{mpsc, watch};

const PAYLOAD: u64 = 1_048_576;

#[derive(Clone)]
struct Server {
    controller: Controller,
    entered: mpsc::UnboundedSender<()>,
    release: watch::Receiver<bool>,
    finish: Option<Finish>,
}

#[derive(Clone)]
struct Finish {
    entered: mpsc::UnboundedSender<()>,
    release: watch::Receiver<bool>,
}

async fn seed(State(server): State<Server>) -> Response {
    let header = json!({ "kind": "file", "path": "asset", "size": PAYLOAD, "mode": 384 });
    let data = json!({ "data": STANDARD.encode(vec![37; 65536]) });
    let first = Bytes::from(format!("{header}\n{data}\n"));
    let messages = format!(
        "{}\n{}\n",
        json!({ "kind": "file", "path": "messages.json", "size": 2, "mode": 384 }),
        json!({ "data": STANDARD.encode(b"[]") })
    );
    let tail = Bytes::from(format!(
        "{}{messages}{{\"complete\":true}}\n",
        format!("{data}\n").repeat(15)
    ));
    let stream = futures_util::stream::unfold(
        (Some(first), Some(tail), server),
        |(mut first, mut tail, mut server)| async move {
            if let Some(first) = first.take() {
                return Some((Ok::<_, std::io::Error>(first), (None, tail, server)));
            }
            if let Some(tail) = tail.take() {
                let _ = server.entered.send(());
                server.release.wait_for(|released| *released).await.unwrap();
                return Some((Ok(tail), (None, None, server)));
            }
            if let Some(mut finish) = server.finish.take() {
                let _ = finish.entered.send(());
                finish.release.wait_for(|released| *released).await.unwrap();
                let error = std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "synthetic HTTP failure after the completion marker",
                );
                return Some((Err(error), (None, None, server)));
            }
            None
        },
    );
    Body::from_stream(stream).into_response()
}

struct Fixture {
    _root: tempfile::TempDir,
    controller: Controller,
    storage: Storage,
    server: tokio::task::JoinHandle<()>,
    entered: mpsc::UnboundedReceiver<()>,
    release: watch::Sender<bool>,
}

impl Fixture {
    async fn new(files: u64) -> Self {
        Self::with_finish(files, None).await
    }

    async fn with_finish(files: u64, finish: Option<Finish>) -> Self {
        let (root, controller) = super::tests::fixture().await;
        // Account for the metadata reserve and runner secret independently.
        controller.budget.lock().await["limits"]["diskMiB"] = (files + 2).into();
        let (entered_tx, entered) = mpsc::unbounded_channel();
        let (release, release_rx) = watch::channel(false);
        let app = Router::new()
            .route("/internal/node-workspace/{attempt}/seed", post(seed))
            .route(
                "/staging-budget",
                get(|State(server): State<Server>| async move {
                    Json(
                        json!({ "availableBytes": server.controller.disk_growth().await.unwrap() }),
                    )
                }),
            )
            .with_state(Server {
                controller: controller.clone(),
                entered: entered_tx,
                release: release_rx,
                finish,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let storage = Storage {
            data: controller.data.clone(),
            state: controller.state.clone(),
            client: reqwest::Client::new(),
            origin,
        };
        Self {
            _root: root,
            controller,
            storage,
            server,
            entered,
            release,
        }
    }

    fn transfer(&self, name: &str, inbox: bool) -> (PathBuf, tokio::task::JoinHandle<Result<()>>) {
        let target = self.controller.data.join("runs").join(name);
        let destination = target.clone();
        let storage = self.storage.clone();
        let task = tokio::spawn(async move {
            crate::nodes::workspace::seed_with_storage(
                &storage.client,
                &url::Url::parse(&storage.origin).unwrap(),
                "synthetic-node",
                &crate::config::id(),
                json!({ "kind": if inbox { "inbox" } else { "initial" } }),
                &destination,
                Some(storage.clone()),
            )
            .await
        });
        (target, task)
    }

    async fn receiving(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.recv())
            .await
            .unwrap()
            .unwrap();
        self.received_file().await;
    }

    async fn received_file(&self) {
        // Observe reception, not just the server's first yield. The reservation
        // must already charge the entire announced payload while its tail waits.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(mut entries) =
                    tokio::fs::read_dir(self.controller.data.join("native-staging")).await
                {
                    while let Some(entry) = entries.next_entry().await.unwrap() {
                        if let Ok(bytes) = tokio::fs::read(entry.path().join("payload/asset")).await
                            && bytes.first() == Some(&37)
                        {
                            return;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test]
async fn queued_native_start_overtakes_new_seed_reservations_without_retaining_the_pool_gate() {
    let mut fixture = Fixture::new(4).await;
    let intent = crate::file_lock::shared(
        &fixture.controller.state.join("storage-startup.lock"),
        "fixture startup intent",
    )
    .unwrap();
    let (target, transfer) = fixture.transfer("queued", false);
    tokio::time::timeout(Duration::from_secs(5), fixture.entered.recv())
        .await
        .unwrap()
        .unwrap();
    // The waiting seed must yield the global gate to the announced starter.
    let owner = tokio::time::timeout(
        Duration::from_secs(1),
        disk::lock(&fixture.controller.state),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!target.exists());
    drop(owner);
    drop(intent);
    fixture.received_file().await;
    fixture.release.send(true).unwrap();
    transfer.await.unwrap().unwrap();
    assert_eq!(
        tokio::fs::read(target.join("asset")).await.unwrap(),
        vec![37; PAYLOAD as usize]
    );
}

#[tokio::test]
async fn a_blocked_network_seed_allows_another_native_admission_and_preserves_bytes() {
    let mut fixture = Fixture::new(4).await;
    let (target, transfer) = fixture.transfer("first", false);
    fixture.receiving().await;
    let id = crate::config::id();
    fixture
        .controller
        .leases
        .lock()
        .await
        .insert(id.clone(), crate::nodes::boot_ms() + 60_000);
    let started = std::time::Instant::now();
    let admitted = tokio::time::timeout(
        Duration::from_secs(1),
        fixture.controller.start_storage(&id, &json!({})),
    )
    .await;
    let elapsed = started.elapsed();
    // Always release the controlled server before asserting, including on the
    // reference failure, so the regression leaves no hanging owned receiver.
    fixture.release.send(true).unwrap();
    let free = matches!(&admitted, Ok(Ok(_)));
    drop(admitted);
    transfer.await.unwrap().unwrap();
    assert_eq!(
        tokio::fs::read(target.join("asset")).await.unwrap(),
        vec![37; PAYLOAD as usize]
    );
    assert!(
        free,
        "another native admission waited for the network seed: {elapsed:?}"
    );
}

#[tokio::test]
async fn native_seed_keeps_the_old_tree_until_http_eof_and_retires_failed_promises() {
    let (entered, mut finishing) = mpsc::unbounded_channel();
    let (release, receiver) = watch::channel(false);
    let mut fixture = Fixture::with_finish(
        4,
        Some(Finish {
            entered,
            release: receiver,
        }),
    )
    .await;
    let target = fixture.controller.data.join("runs/retained");
    private_dir(&target).await.unwrap();
    tokio::fs::write(target.join("asset"), b"previous files")
        .await
        .unwrap();

    let (_, mut transfer) = fixture.transfer("retained", false);
    fixture.receiving().await;
    fixture.release.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), finishing.recv())
        .await
        .unwrap()
        .unwrap();
    // All protocol frames have crossed the server, but HTTP has not ended.
    // Always release its controlled failure before asserting either outcome.
    let pending = tokio::time::timeout(Duration::from_secs(1), &mut transfer).await;
    release.send(true).unwrap();
    let waited_for_http = pending.is_err();
    let result = match pending {
        Ok(result) => result.unwrap(),
        Err(_) => tokio::time::timeout(Duration::from_secs(5), transfer)
            .await
            .unwrap()
            .unwrap(),
    };

    assert!(
        waited_for_http,
        "protocol completion cannot publish before HTTP EOF"
    );
    assert_eq!(result.unwrap_err().status, 502);
    assert_eq!(
        tokio::fs::read(target.join("asset")).await.unwrap(),
        b"previous files"
    );
    assert!(
        disk::used(&fixture.controller.data, &fixture.controller.state)
            .await
            .unwrap()
            < PAYLOAD
    );
}

#[tokio::test]
async fn concurrent_seed_promises_cannot_overbook_the_shared_pool_and_cancel_releases_them() {
    let mut fixture = Fixture::new(1).await;
    let (first, transfer) = fixture.transfer("first", false);
    fixture.receiving().await;
    assert!(
        disk::used(&fixture.controller.data, &fixture.controller.state)
            .await
            .unwrap()
            >= PAYLOAD
    );
    let (second, competing) = fixture.transfer("second", false);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), competing)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(!second.exists());
    transfer.abort();
    let _ = transfer.await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if disk::used(&fixture.controller.data, &fixture.controller.state)
                .await
                .unwrap()
                < PAYLOAD
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!first.exists());
    fixture.release.send(true).unwrap();
    let (third, retry) = fixture.transfer("third", false);
    retry.await.unwrap().unwrap();
    assert_eq!(
        tokio::fs::read(third.join("asset")).await.unwrap(),
        vec![37; PAYLOAD as usize]
    );
}

#[tokio::test]
async fn inbox_receive_charges_its_second_copy_before_network_and_keeps_the_mounted_inode() {
    use std::os::unix::fs::MetadataExt;
    let mut fixture = Fixture::new(3).await;
    let target = fixture.controller.data.join("runs/inbox");
    private_dir(&target).await.unwrap();
    let inode = tokio::fs::metadata(&target).await.unwrap().ino();
    let (_, transfer) = fixture.transfer("inbox", true);
    fixture.receiving().await;
    assert!(
        disk::used(&fixture.controller.data, &fixture.controller.state)
            .await
            .unwrap()
            >= 2 * PAYLOAD
    );
    fixture.release.send(true).unwrap();
    transfer.await.unwrap().unwrap();
    assert_eq!(tokio::fs::metadata(&target).await.unwrap().ino(), inode);
    assert_eq!(
        tokio::fs::read(target.join("asset")).await.unwrap(),
        vec![37; PAYLOAD as usize]
    );
    assert!(
        disk::used(&fixture.controller.data, &fixture.controller.state)
            .await
            .unwrap()
            < 2 * PAYLOAD
    );
}
