use super::*;

fn lease(account: &str, run: &str) -> Lease {
    Lease {
        account_id: account.into(),
        run_id: run.into(),
        home: format!("/fixture/{run}").into(),
        provider: Provider::Codex,
        model: "fixture".into(),
    }
}

#[test]
fn pending_generations_join_initial_registration_without_latest_only_loss() {
    let registry = Registry::default();
    let a = registry.begin(
        "account",
        vec!["independent-A".into(), "independent-B".into()],
    );
    let history = History::default();
    let observation = registry.register(&lease("account", "run"), "attempt", history.clone());
    history.extend(["independent-C".into()]);
    assert_eq!(
        history.snapshot().as_slice(),
        ["independent-A", "independent-B", "independent-C"]
    );
    drop(a);
    observation.revoke();
    assert!(locked(&registry.0).is_empty());
}

#[test]
fn revocation_keeps_outer_history_but_rejects_new_admission_and_prunes_registry() {
    let registry = Registry::default();
    let history = History::default();
    let lease = lease("account", "run");
    let observation = registry.register(&lease, "attempt", history.clone());
    let handle = observation.handle();
    let delivery = handle.admit(&lease, vec!["delivered-B".into()]).unwrap();
    observation.revoke();
    assert!(handle.admit(&lease, vec!["late-C".into()]).is_err());
    assert_eq!(history.snapshot().as_slice(), ["delivered-B"]);
    drop(delivery);
    drop(observation);
    drop(handle);
    assert!(locked(&registry.0).is_empty());
    assert_eq!(history.snapshot().as_slice(), ["delivered-B"]);
}

#[test]
fn account_isolation_shared_history_and_independent_attempt_owners() {
    let registry = Registry::default();
    let first = History::default();
    let second = History::default();
    let unrelated = History::default();
    let a = registry.register(&lease("same", "one"), "reused-attempt", first.clone());
    let b = registry.register(&lease("same", "two"), "reused-attempt", second.clone());
    let c = registry.register(&lease("other", "three"), "attempt", unrelated.clone());
    a.revoke();
    drop(registry.begin("same", vec!["next-generation".into()]));
    assert!(first.snapshot().is_empty());
    assert_eq!(second.snapshot().as_slice(), ["next-generation"]);
    assert!(unrelated.snapshot().is_empty());
    drop((a, b, c));
    assert!(locked(&registry.0).is_empty());
}

pub(super) struct CommitGate {
    entered: tokio_util::sync::CancellationToken,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl CommitGate {
    pub(super) fn wait(&self) {
        self.entered.cancel();
        locked(&self.release).recv().unwrap();
    }
}

async fn fixture() -> (tempfile::TempDir, Arc<Service>, Lease) {
    let root = tempfile::tempdir().unwrap();
    let s = Service::new(crate::config::Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().into()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        codex_bin: "unused".into(),
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
    let account = super::super::Account::new(
        Provider::Codex,
        "History test",
        super::super::AccountState::Ready,
    );
    s.store.save_coding_account(&account).await.unwrap();
    let lease = Lease {
        account_id: account.id,
        provider: Provider::Codex,
        run_id: crate::config::id(),
        home: root.path().join("run"),
        model: "fixture".into(),
    };
    let stored = lease.clone();
    s.store
        .transaction(move |db| db.reserve_account_lease(&stored))
        .await
        .unwrap();
    (root, s, lease)
}

fn auth(value: &str) -> serde_json::Value {
    serde_json::json!({ "tokens": { "access_token": value, "refresh_token": format!("refresh-{value}"), "id_token": format!("id-{value}"), "account_id": "same-identity" } })
}

#[tokio::test]
async fn real_store_job_retains_generation_after_its_caller_is_aborted() {
    let (_root, s, lease) = fixture().await;
    super::super::codex::replace_credentials_fixture(&s, &lease.account_id, Some(&auth("first-A")))
        .await
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let gate = Arc::new(CommitGate {
        entered: tokio_util::sync::CancellationToken::new(),
        release: Mutex::new(wait),
    });
    *locked(&s.accounts.output_credentials.1) = Some(gate.clone());
    let writer_service = s.clone();
    let account = lease.account_id.clone();
    let writer = tokio::spawn(async move {
        super::super::codex::replace_credentials_fixture(
            &writer_service,
            &account,
            Some(&auth("second-B")),
        )
        .await
    });
    gate.entered.cancelled().await;
    // The actual SQLite actor has taken the guarded production write closure.
    // Its awaiting task can disappear without dropping the pending generation.
    writer.abort();
    assert!(writer.await.unwrap_err().is_cancelled());
    let history = History::default();
    let observer = s
        .accounts
        .observe_output_credentials(&s, &lease, "attempt", history.clone())
        .await
        .unwrap()
        .unwrap();
    let current = s.accounts.redactions(&s, &lease).await.unwrap();
    assert!(current.contains(&"first-A".to_owned()));
    assert!(!current.contains(&"second-B".to_owned()));
    assert!(history.snapshot().contains(&"second-B".to_owned()));
    release.send(()).unwrap();
    // Same FIFO writer: return only after the abandoned actual job committed.
    s.store.write(|_| Ok(())).await.unwrap();
    let current = s.accounts.redactions(&s, &lease).await.unwrap();
    assert!(current.contains(&"second-B".to_owned()));
    observer.revoke();
    drop(observer);
    assert!(locked(&s.accounts.output_credentials.0).is_empty());
    s.shutdown.cancel();
}

#[tokio::test]
async fn real_ciphertext_replacement_rollback_and_deletion_preserve_active_history() {
    let (_root, s, lease) = fixture().await;
    super::super::codex::replace_credentials_fixture(&s, &lease.account_id, Some(&auth("first-A")))
        .await
        .unwrap();
    let history = History::default();
    let observer = s
        .accounts
        .observe_output_credentials(&s, &lease, "attempt", history.clone())
        .await
        .unwrap()
        .unwrap();
    for generation in [Some(auth("second-B")), Some(auth("first-A")), None] {
        super::super::codex::replace_credentials_fixture(
            &s,
            &lease.account_id,
            generation.as_ref(),
        )
        .await
        .unwrap();
    }
    assert!(s.accounts.redactions(&s, &lease).await.unwrap().is_empty());
    assert_eq!(history.snapshot().len(), 6);
    assert!(history.snapshot().contains(&"second-B".to_owned()));
    observer.revoke();
    drop(observer);
    assert!(locked(&s.accounts.output_credentials.0).is_empty());
    s.shutdown.cancel();
}

#[test]
fn lifetime_history_is_not_evicted_or_stopped_at_a_new_quota() {
    let history = History::default();
    history.extend(["initial-generation".into()]);
    let old_snapshot = history.snapshot();
    history.extend((0..4200).map(|n| format!("generation-{n}")));
    assert_eq!(history.snapshot().len(), 4201);
    assert_eq!(old_snapshot.as_slice(), ["initial-generation"]);
    assert!(history.snapshot().contains(&"generation-0".into()));
    assert!(history.snapshot().contains(&"generation-4199".into()));
}

#[tokio::test]
async fn uncovered_providers_never_enable_codex_prefix_aggregation() {
    let (_root, s, mut lease) = fixture().await;
    for provider in [Provider::Claude, Provider::Opencode] {
        lease.provider = provider;
        assert!(
            s.accounts
                .observe_output_credentials(&s, &lease, "attempt", History::default())
                .await
                .unwrap()
                .is_none()
        );
    }
    s.shutdown.cancel();
}

#[tokio::test]
async fn exact_selected_broker_response_is_observed_even_after_vault_moves_on() {
    use crate::accounts::{broker, codex};
    let (_root, s, lease) = fixture().await;
    codex::replace_credentials_fixture(&s, &lease.account_id, Some(&auth("selected-B")))
        .await
        .unwrap();
    let original = History::default();
    let observer = s
        .accounts
        .observe_output_credentials(&s, &lease, "original", original)
        .await
        .unwrap()
        .unwrap();
    let probe = Arc::new(broker::DeliveryProbe::default());
    let broker = broker::serve_observed_fixture(&s, &lease, observer.handle(), probe.clone())
        .await
        .unwrap();
    let socket = lease.home.join(broker::SOCKET);
    let request = tokio::spawn(async move {
        broker::request(
            &socket,
            &broker::Request::default(),
            std::time::Duration::from_secs(9),
        )
        .await
    });
    probe.selected.cancelled().await;
    codex::replace_credentials_fixture(&s, &lease.account_id, Some(&auth("current-C")))
        .await
        .unwrap();
    // A different active execution starts AFTER C, while response B is already
    // selected. Its initial snapshot cannot have learned B by writer fanout.
    let late = History::default();
    let late_observer = s
        .accounts
        .observe_output_credentials(&s, &lease, "late", late.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(!late.snapshot().contains(&"selected-B".to_owned()));
    probe.resume.cancel();
    let response = request.await.unwrap().unwrap();
    assert!(response["accessToken"] == "selected-B");
    assert!(late.snapshot().contains(&"selected-B".to_owned()));
    observer.revoke();
    late_observer.revoke();
    broker.close().await;
    drop((observer, late_observer));
    assert!(locked(&s.accounts.output_credentials.0).is_empty());
    s.shutdown.cancel();
}

#[tokio::test]
async fn revoked_scope_and_broker_close_form_a_no_late_delivery_barrier() {
    use crate::accounts::{broker, codex};
    let (_root, s, lease) = fixture().await;
    codex::replace_credentials_fixture(&s, &lease.account_id, Some(&auth("selected-B")))
        .await
        .unwrap();
    let history = History::default();
    let observer = s
        .accounts
        .observe_output_credentials(&s, &lease, "attempt", history.clone())
        .await
        .unwrap()
        .unwrap();
    let handle = observer.handle();
    let weak = Arc::downgrade(&handle.scope);
    let probe = Arc::new(broker::DeliveryProbe::default());
    let broker = broker::serve_observed_fixture(&s, &lease, handle, probe.clone())
        .await
        .unwrap();
    let socket = lease.home.join(broker::SOCKET);
    let request = tokio::spawn(async move {
        broker::request(
            &socket,
            &broker::Request::default(),
            std::time::Duration::from_secs(9),
        )
        .await
    });
    probe.selected.cancelled().await;
    observer.revoke();
    broker.close().await;
    drop(observer);
    // The selected response cannot wake later and publish after close returns.
    probe.resume.cancel();
    assert!(request.await.unwrap().is_err());
    assert!(weak.upgrade().is_none());
    assert!(locked(&s.accounts.output_credentials.0).is_empty());
    assert!(history.snapshot().contains(&"selected-B".to_owned()));
    assert!(!lease.home.join(broker::SOCKET).exists());
    s.shutdown.cancel();
}

#[test]
fn account_switch_reuses_the_execution_history_without_retaining_old_subscription() {
    let registry = Registry::default();
    let history = History::default();
    let first = registry.register(
        &lease("old-account", "run"),
        "first-attempt",
        history.clone(),
    );
    drop(registry.begin("old-account", vec!["old-account-generation".into()]));
    first.revoke();
    drop(first);
    let second = registry.register(
        &lease("new-account", "run"),
        "second-attempt",
        history.clone(),
    );
    drop(registry.begin("new-account", vec!["new-account-generation".into()]));
    assert_eq!(
        history.snapshot().as_slice(),
        ["old-account-generation", "new-account-generation"]
    );
    assert!(!locked(&registry.0).contains_key("old-account"));
    drop(second);
    assert!(locked(&registry.0).is_empty());
}

#[tokio::test]
async fn failed_initial_snapshot_revokes_registration_before_a_reader_can_be_eligible() {
    let (_root, s, lease) = fixture().await;
    let key = format!("mcp-secret:codex-account:{}", lease.account_id);
    s.store
        .set(&key, serde_json::json!("invalid-encrypted-fixture"), None)
        .await
        .unwrap();
    let history = History::default();
    let result = s
        .accounts
        .observe_output_credentials(&s, &lease, "attempt", history)
        .await;
    assert!(result.is_err());
    assert!(locked(&s.accounts.output_credentials.0).is_empty());
    s.shutdown.cancel();
}
