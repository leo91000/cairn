//! Controlled phases inside actual provider/session operations, never after-only mocks.
use super::*;
use crate::{
    accounts::{
        Account, AccountState, codex,
        output_redactions::{History, Observation},
    },
    provider::Provider,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
};
use tokio::{io::AsyncReadExt, net::UnixListener};

pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    pub(crate) service: Arc<Service>,
    pub(crate) lease: Lease,
}

impl Fixture {
    pub(crate) async fn new(provider: Provider) -> Self {
        let root = tempfile::tempdir().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/codex.mjs");
        let s = Service::new(crate::config::Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().into()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: fixture.to_str().unwrap().into(),
            claude_bin: "unused".into(),
            opencode_bin: "unused".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        s.accounts.initialize(&s).await.unwrap();
        let account = add_account(&s, provider, "Initial account").await;
        let lease = Lease {
            account_id: account,
            provider,
            run_id: crate::config::id(),
            home: root.path().join("run"),
            model: "fixture-fast".into(),
        };
        let saved = lease.clone();
        s.store
            .transaction(move |db| db.reserve_account_lease(&saved))
            .await
            .unwrap();
        Self {
            _root: root,
            service: s,
            lease,
        }
    }
}

pub(crate) async fn add_account(s: &Service, provider: Provider, name: &str) -> String {
    let account = Account::new(provider, name, AccountState::Ready);
    let id = account.id.clone();
    s.store
        .transaction(move |db| {
            db.save_coding_account(&account)?;
            provider.driver().added(db)
        })
        .await
        .unwrap();
    id
}

pub(crate) async fn seed_codex(s: &Service, id: &str, label: &str, identity: &str) {
    let home = s.config.data_dir.join(format!("fixture-auth-{id}"));
    private_dir(&home).await.unwrap();
    tokio::fs::write(
        home.join("auth.json"),
        codex_auth(label, identity).to_string(),
    )
    .await
    .unwrap();
    codex::capture_fixture(s, id, &home).await.unwrap();
    s.accounts.refresh(s, id).await.unwrap();
    assert!(s.accounts.account(s, id).await.unwrap().is_ready());
}

fn codex_auth(label: &str, identity: &str) -> Value {
    let claims = URL_SAFE_NO_PAD.encode(json!({ "sub": identity, "nonce": label }).to_string());
    json!({ "tokens": { "access_token": format!("opaque-{label}-access"), "refresh_token": format!("opaque-{label}-refresh"), "id_token": format!("head.{claims}.signature-{label}"), "account_id": identity } })
}

pub(crate) async fn pending_once<F: Future>(mut future: Pin<&mut F>) -> bool {
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await
}

pub(crate) struct Gate(BufReader<UnixStream>);

impl Gate {
    async fn accept(listener: &UnixListener) -> Self {
        Self(BufReader::new(listener.accept().await.unwrap().0))
    }

    pub(crate) async fn event(&mut self) -> Value {
        line(&mut self.0).await.unwrap()
    }

    pub(crate) async fn release(&mut self) {
        self.0.get_mut().write_all(b"continue\n").await.unwrap();
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = self.0.get_mut().try_write(b"continue\n");
    }
}

pub(crate) struct ActiveCodex {
    pub(crate) fixture: Fixture,
    pub(crate) history: History,
    pub(crate) observation: Observation,
    pub(crate) broker: Broker,
    pub(crate) stopped: CancellationToken,
    pub(crate) request: tokio::task::JoinHandle<Result<Value>>,
    pub(crate) refresh_gate: Gate,
    pub(crate) close_gate: Gate,
    pub(crate) materialize: Arc<codex::MaterializeProbe>,
    pub(crate) pid: u32,
}

pub(crate) async fn active_codex() -> ActiveCodex {
    let fixture = Fixture::new(Provider::Codex).await;
    let s = &fixture.service;
    let lease = &fixture.lease;
    seed_codex(s, &lease.account_id, "A", "first-identity").await;
    let history = History::default();
    let observation = s
        .accounts
        .observe_output_credentials(s, lease, "attempt", history.clone())
        .await
        .unwrap()
        .unwrap();
    let broker = serve_observed(s, lease, observation.handle())
        .await
        .unwrap();
    let stopped = broker.stopped_fixture();
    let mut client = codex::Client::from_socket(lease.home.join(SOCKET)).unwrap();
    client.tokens(false).await.unwrap();
    let base = &s.config.data_dir;
    let refresh_path = base.join("refresh.sock");
    let close_path = base.join("closing.sock");
    let refresh_listener = UnixListener::bind(&refresh_path).unwrap();
    let close_listener = UnixListener::bind(&close_path).unwrap();
    let home = base.join("codex-monitor").join(&lease.account_id);
    private_dir(&home).await.unwrap();
    tokio::fs::write(
        home.join("fixture-token-rotation.json"),
        json!({ "refreshToken": true, "tokens": codex_auth("B", "first-identity")["tokens"] })
            .to_string(),
    )
    .await
    .unwrap();
    tokio::fs::write(
        home.join("fixture-account-read-gate.json"),
        json!({ "socket": refresh_path }).to_string(),
    )
    .await
    .unwrap();
    tokio::fs::write(
        home.join("fixture-session-close-gate.json"),
        json!({ "socket": close_path }).to_string(),
    )
    .await
    .unwrap();
    let materialize = Arc::new(codex::MaterializeProbe::default());
    s.accounts
        .codex_materialize_probes
        .lock()
        .unwrap()
        .insert(lease.account_id.clone(), materialize.clone());
    let request = tokio::spawn(async move { client.tokens(true).await });
    let mut close_gate = Gate::accept(&close_listener).await;
    let started = close_gate.event().await;
    assert!(started["event"] == "started");
    let pid = started["pid"].as_u64().unwrap() as u32;
    let mut refresh_gate = Gate::accept(&refresh_listener).await;
    let ready = refresh_gate.event().await;
    assert!(ready["event"] == "refresh_ready" && ready["pid"] == pid);
    materialize.old_pid.store(pid, Ordering::Relaxed);
    ActiveCodex {
        fixture,
        history,
        observation,
        broker,
        stopped,
        request,
        refresh_gate,
        close_gate,
        materialize,
        pid,
    }
}

#[tokio::test]
async fn legacy_rotation_persists_before_account_reuse_even_if_close_waiter_disappears() {
    use crate::accounts::claude;
    for abandon_close in [false, true] {
        let fixture = Fixture::new(Provider::Claude).await;
        let s = &fixture.service;
        let lease = &fixture.lease;
        let home = claude::account_home(&s.config, &lease.account_id);
        private_dir(&home).await.unwrap();
        let original = json!({ "claudeAiOauth": { "accessToken": "previous-access", "refreshToken": "previous-refresh", "expiresAt": crate::config::now() - 1000, "scopes": ["user:inference"] } });
        tokio::fs::write(home.join(".credentials.json"), original.to_string())
            .await
            .unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let router = axum::Router::new().route("/token", axum::routing::post(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::Relaxed);
                axum::Json(json!({ "access_token": "rotated-access", "refresh_token": "rotated-refresh", "expires_in": 3600 }))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        let probe = Arc::new(claude::RotationProbe {
            endpoint,
            before_persist: CancellationToken::new(),
            resume: CancellationToken::new(),
        });
        s.accounts
            .claude_rotation_probes
            .lock()
            .unwrap()
            .insert(lease.account_id.clone(), probe.clone());
        let broker = serve(s, lease).await.unwrap();
        let stop = broker.stopped_fixture();
        let socket = lease.home.join(SOCKET);
        let request = tokio::spawn(async move {
            super::request(&socket, &Request::default(), Duration::from_secs(9)).await
        });
        probe.before_persist.cancelled().await;
        let before: Value = serde_json::from_slice(
            &tokio::fs::read(home.join(".credentials.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(before == original);
        let mut close = Some(tokio::spawn(broker.close()));
        stop.cancelled().await;
        let next_request = json!({});
        let mut next = Box::pin(s.accounts.access(s, lease, &next_request));
        assert!(
            pending_once(next.as_mut()).await,
            "rotation lock must cover actual provider response through persistence"
        );
        if abandon_close {
            let close = close.take().unwrap();
            close.abort();
            assert!(close.await.unwrap_err().is_cancelled());
        }
        probe.resume.cancel();
        let next = next.await.unwrap();
        if let Some(close) = close {
            close.await.unwrap();
        }
        assert!(
            next["claudeAiOauth"]["accessToken"] == "rotated-access",
            "subsequent access must use the durably rotated credential"
        );
        let persisted: Value = serde_json::from_slice(
            &tokio::fs::read(home.join(".credentials.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(
            persisted["claudeAiOauth"]["refreshToken"] == "rotated-refresh",
            "refresh state must persist before account reuse"
        );
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        assert!(
            request.await.unwrap().is_err(),
            "stop must prevent new credential delivery after durable completion"
        );
        server.abort();
        let _ = server.await;
        s.shutdown.cancel();
    }
}

#[tokio::test]
async fn real_codex_session_finishes_before_account_home_reuse_on_close_or_waiter_cancel() {
    for abandon_close in [false, true] {
        let mut active = active_codex().await;
        let s = &active.fixture.service;
        let lease = &active.fixture.lease;
        active.observation.revoke();
        let mut close = Some(tokio::spawn(active.broker.close()));
        active.stopped.cancelled().await;
        let mut refresh = Box::pin(s.accounts.refresh(s, &lease.account_id));
        assert!(pending_once(refresh.as_mut()).await);
        if abandon_close {
            let close = close.take().unwrap();
            close.abort();
            assert!(close.await.unwrap_err().is_cancelled());
        }
        active.refresh_gate.release().await;
        let closing = active.close_gate.event().await;
        assert!(closing["event"] == "terminating" && closing["pid"] == active.pid);
        assert!(
            pending_once(refresh.as_mut()).await,
            "session close must still own the account guard"
        );
        assert_eq!(active.materialize.entries.load(Ordering::Relaxed), 1);
        assert!(!active.materialize.overlapped.load(Ordering::Relaxed));
        active.close_gate.release().await;
        refresh.await.unwrap();
        if let Some(close) = close {
            close.await.unwrap();
        }
        assert!(unsafe { libc::kill(active.pid as libc::pid_t, 0) } != 0);
        assert!(!active.materialize.overlapped.load(Ordering::Relaxed));
        assert!(active.materialize.entries.load(Ordering::Relaxed) >= 2);
        assert!(
            s.accounts
                .account(s, &lease.account_id)
                .await
                .unwrap()
                .is_ready()
        );
        assert!(active.request.await.unwrap().is_err());
        assert!(
            active
                .history
                .snapshot()
                .iter()
                .all(|value| !value.is_empty())
        );
        s.shutdown.cancel();
    }
}

#[tokio::test]
async fn partial_socket_delivery_is_cancelled_without_losing_history_or_waiting_for_client_reads() {
    let fixture = Fixture::new(Provider::Codex).await;
    let s = &fixture.service;
    let lease = &fixture.lease;
    let token = "opaque-long-access-".repeat(2048);
    let mut auth = codex_auth("large", "first-identity");
    auth["tokens"]["access_token"] = token.clone().into();
    codex::replace_credentials_fixture(s, &lease.account_id, Some(&auth))
        .await
        .unwrap();
    let history = History::default();
    let observation = s
        .accounts
        .observe_output_credentials(s, lease, "attempt", history.clone())
        .await
        .unwrap()
        .unwrap();
    let probe = Arc::new(DeliveryProbe {
        small_socket: true,
        ..Default::default()
    });
    probe.resume.cancel();
    let broker = serve_observed_fixture(s, lease, observation.handle(), probe.clone())
        .await
        .unwrap();
    let mut client = UnixStream::connect(lease.home.join(SOCKET)).await.unwrap();
    client.write_all(b"{}\n").await.unwrap();
    probe.partial.cancelled().await;
    assert!(probe.written_bytes.load(Ordering::Relaxed) > 0);
    assert!(
        probe.written_bytes.load(Ordering::Relaxed) < probe.total_bytes.load(Ordering::Relaxed)
    );
    observation.revoke();
    broker.close().await;
    let mut received = Vec::new();
    client.read_to_end(&mut received).await.unwrap();
    assert!(
        !received.is_empty() && received.len() < probe.total_bytes.load(Ordering::Relaxed) as usize
    );
    assert!(history.snapshot().contains(&token));
    assert!(!lease.home.join(SOCKET).exists());
    s.shutdown.cancel();
}
