//! Real broker/capture rotations, pipe readers, worker wait and durable publication.
//! All values are synthetic; failures report booleans/counts, never token content.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::sync::Mutex;
use tokio::io::AsyncWriteExt;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::{Layer, layer::SubscriberExt};

const KNOWN: &str = "2026-11-01T00:19:49Z INFO cairn_performance: operation=\"codex_rpc\" method=\"thread/items/list\" request_id=1 event=\"started\" timeout_ms=20000\n";
const WAIT_ERROR: &str = "{\"type\":\"fixture.wait_error\"}";

const UNKNOWN: &str =
    "2026-11-01T00:19:49Z INFO cairn_performance: operation=\"fixture_unknown\" note=";

pub(super) struct PrivacyProbe {
    known_recorded: CancellationToken,
    canonical_recorded: CancellationToken,
    diagnostic_recorded: CancellationToken,
    unknown_recorded: CancellationToken,
    error: Error,
}

impl PrivacyProbe {
    pub(super) fn before_record(&self, output: &output::Output) -> Result<()> {
        // Actual wait error boundary, after all independently acknowledged Store
        // writes. This is not a SQLite/provider-cause reproduction.
        if output.raw == WAIT_ERROR {
            return Err(self.error.clone());
        }
        Ok(())
    }

    pub(super) fn recorded(&self, output: &output::Output) {
        if output.raw == KNOWN.trim_end() {
            self.known_recorded.cancel();
        }
        if output.kind == output::OutputKind::Stdout && output.raw.contains("history-answer") {
            self.canonical_recorded.cancel();
        }
        if output.kind == output::OutputKind::Diagnostic {
            self.diagnostic_recorded.cancel();
        }
        if output.kind == output::OutputKind::Performance && output.raw.starts_with(UNKNOWN) {
            self.unknown_recorded.cancel();
        }
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

struct CaptureRecord<'a>(&'a Capture);

impl tracing::field::Visit for CaptureRecord<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "record" {
            let value = format!("{value:?}");
            if value.starts_with(UNKNOWN) {
                self.0.0.lock().unwrap().push(value);
            }
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        event.record(&mut CaptureRecord(self));
    }
}

#[test]
fn independent_rotations_preserve_future_output_redaction() {
    for child in [
        "worker::launch::stream_tests::credential_history::legacy_rotations_child",
        "worker::launch::stream_tests::credential_history::aggregated_rotations_child",
    ] {
        crate::test_process::run_ignored(child);
        println!("history_rotation_control passed={child}");
    }
}

#[tokio::test]
#[ignore = "owns process-global token patterns and tracing capture"]
async fn legacy_rotations_child() {
    bounded_rotation_fixture(false).await;
}

#[tokio::test]
#[ignore = "owns process-global token patterns and tracing capture"]
async fn aggregated_rotations_child() {
    bounded_rotation_fixture(true).await;
}

async fn bounded_rotation_fixture(aggregate: bool) {
    // Bound a broken test handshake without changing any product timeout.
    tokio::time::timeout(
        Duration::from_secs(30),
        historical_rotation_fixture(aggregate),
    )
    .await
    .expect("credential-history test handshake must finish");
}

fn generation(label: &str) -> Value {
    // Same identity, independent opaque values. Neither a format masker nor an
    // older value as a substring is allowed to make the negative control green.
    let claims =
        URL_SAFE_NO_PAD.encode(json!({ "sub": "history-subject", "nonce": label }).to_string());
    json!({ "tokens": {
        "access_token": format!("opaque{label}AccessValue9wQ2uL7h"),
        "refresh_token": format!("opaque{label}RefreshValue4xT8mN6v"),
        "id_token": format!("head{label}.{claims}.signature{label}"),
        "account_id": "history-identity",
    } })
}

fn values(auth: &Value) -> Vec<String> {
    ["access_token", "refresh_token", "id_token"]
        .into_iter()
        .map(|field| auth["tokens"][field].as_str().unwrap().to_owned())
        .collect()
}

async fn rotation_control(s: &Service, id: &str, auth: &Value, refresh: bool) {
    let home = s.config.data_dir.join("codex-monitor").join(id);
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(
        home.join("fixture-token-rotation.json"),
        json!({ "refreshToken": refresh, "tokens": auth["tokens"] }).to_string(),
    )
    .await
    .unwrap();
}

async fn historical_rotation_fixture(aggregate: bool) {
    crate::run_output::set_plugin_secret_patterns(Vec::new());
    let a = generation("A7");
    let b = generation("B4");
    let c = generation("C9");
    let all = [values(&a), values(&b), values(&c)].concat();
    for (index, value) in all.iter().enumerate() {
        assert!(
            all.iter().enumerate().all(|(other, candidate)| {
                index == other || (!candidate.contains(value) && !value.contains(candidate))
            }),
            "synthetic generations must be pairwise non-containing"
        );
    }
    let ac = [values(&a), values(&c)].concat();
    let negative = values(&b)
        .iter()
        .all(|value| run_output::redact(value, &ac) == *value);
    let positive = values(&b)
        .iter()
        .all(|value| run_output::redact(value, &all) == "[redacted]");
    assert!(
        negative && positive,
        "actual redactor controls must distinguish B"
    );
    let body = values(&b).join(" | ");
    let expected = "[redacted] | [redacted] | [redacted]";
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/codex.mjs");
    let (root, service) = service_with_codex(fixture.to_str().unwrap()).await;
    let account = crate::accounts::Account::new(
        Provider::Codex,
        "Generation history fixture",
        crate::accounts::AccountState::Pending,
    );
    service.store.save_coding_account(&account).await.unwrap();
    let initial = root.path().join("initial-auth");
    tokio::fs::create_dir_all(&initial).await.unwrap();
    tokio::fs::write(initial.join("auth.json"), a.to_string())
        .await
        .unwrap();
    crate::accounts::codex::capture_fixture(&service, &account.id, &initial)
        .await
        .unwrap();
    service
        .accounts
        .refresh(&service, &account.id)
        .await
        .unwrap();
    assert!(
        service
            .accounts
            .account(&service, &account.id)
            .await
            .unwrap()
            .is_ready()
    );
    let id = crate::config::id();
    let row = json!({ "id": id, "taskId": id, "status": "running" });
    let lease = Lease {
        account_id: account.id.clone(),
        provider: Provider::Codex,
        run_id: id.clone(),
        home: root.path().join("run-codex"),
        model: "fixture-fast".into(),
    };
    let saved_lease = lease.clone();
    service
        .store
        .transaction(move |db| {
            db.add_run_fixture(&row, None)?;
            db.reserve_account_lease(&saved_lease)
        })
        .await
        .unwrap();
    let mut sensitive = service.accounts.redactions(&service, &lease).await.unwrap();
    assert!(values(&a).iter().all(|value| sensitive.contains(value)));
    let broker = crate::accounts::broker::serve(&service, &lease)
        .await
        .unwrap();
    let mut client = crate::accounts::codex::Client::from_socket(
        lease.home.join(crate::accounts::broker::SOCKET),
    )
    .unwrap();
    let response = client.tokens(false).await.unwrap();
    assert!(response["accessToken"] == a["tokens"]["access_token"]);
    let checkpoint = Checkpoint::new(
        service.store.clone(),
        id.clone(),
        RunCheckpoint::fresh(None),
    );
    let mut run = service.store.run(&id).await.unwrap();
    let mut held = Some(lease.clone());
    let cancel = CancellationToken::new();
    let history = Arc::new(PrivacyProbe {
        known_recorded: CancellationToken::new(),
        canonical_recorded: CancellationToken::new(),
        diagnostic_recorded: CancellationToken::new(),
        unknown_recorded: CancellationToken::new(),
        error: Error::conflict(format!("outer {body}")),
    });
    let probe = OutputProbe {
        refreshes: Arc::new(AtomicU64::new(0)),
        before_canonical: Arc::new(AtomicU64::new(u64::MAX)),
        history: Some(history.clone()),
    };
    let capture = Capture::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(capture.clone()));
    let observations = async {
        let mut execution = Execution {
            s: &service,
            id: id.clone(),
            run: &mut run,
            account: &mut held,
            preparation: None,
            cancel: &cancel,
            checkpoint: &checkpoint,
            sensitive: &mut sensitive,
            provider: Provider::Codex,
            directory: root.path().into(),
            attempt_id: crate::config::id(),
            reservation: None,
            output_probe: Some(probe.clone()),
        };
        let (mut stdout, stdout_reader) = tokio::io::duplex(8192);
        let (mut stderr, stderr_reader) = tokio::io::duplex(8192);
        let (sender, mut events) = mpsc::channel(32);
        let out_sender = sender.clone();
        let out = tokio::spawn(async move {
            output::read_output(stdout_reader, false, out_sender, Vec::new()).await;
        });
        let accepted = CancellationToken::new();
        let mut publisher = output::Publisher::new(&id, &crate::config::id());
        publisher.accepted_fixture(accepted.clone());
        let err = tokio::spawn(async move {
            if aggregate {
                output::read_agent_diagnostics(stderr_reader, sender, publisher).await;
            } else {
                // Exact original diagnostic reader + consumer refresh path.
                output::read_output(stderr_reader, true, sender, Vec::new()).await;
            }
        });
        let produce = async {
            rotation_control(&service, &account.id, &b, true).await;
            let response = client.tokens(true).await.unwrap();
            assert!(response["accessToken"] == b["tokens"]["access_token"]);
            let current = service.accounts.redactions(&service, &lease).await.unwrap();
            assert!(values(&b).iter().all(|value| current.contains(value)));
            stderr.write_all(KNOWN.as_bytes()).await.unwrap();
            if aggregate {
                accepted.cancelled().await;
            } else {
                history.known_recorded.cancelled().await;
            }
            // C is a real manager-only refresh/capture, with no canonical B frame.
            rotation_control(&service, &account.id, &c, false).await;
            service
                .accounts
                .refresh(&service, &account.id)
                .await
                .unwrap();
            let current = service.accounts.redactions(&service, &lease).await.unwrap();
            assert!(values(&c).iter().all(|value| current.contains(value)));
            assert!(values(&b).iter().all(|value| !current.contains(value)));
            let message = json!({ "type": "item.completed", "item": {
                "id": "history-answer", "type": "agent_message", "text": body,
            } });
            stdout
                .write_all(format!("{message}\n").as_bytes())
                .await
                .unwrap();
            history.canonical_recorded.cancelled().await;
            stderr
                .write_all(format!("diagnostic {body}\n").as_bytes())
                .await
                .unwrap();
            history.diagnostic_recorded.cancelled().await;
            stderr
                .write_all(format!("{UNKNOWN}{body}\n").as_bytes())
                .await
                .unwrap();
            history.unknown_recorded.cancelled().await;
            stdout
                .write_all(format!("{WAIT_ERROR}\n").as_bytes())
                .await
                .unwrap();
            drop(stdout);
            drop(stderr);
        };
        let mut child = Supervised::exited_fixture();
        let mut log_total = 0;
        let (_, result) = tokio::join!(
            produce,
            execution.wait(&mut child, &mut events, &mut log_total, false)
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("the explicit wait boundary must fail"),
        };
        drop(execution);
        // Even the failing fixture joins both real readers before the outer
        // failure publication. Production lifecycle/revocation needs its own tests.
        out.await.unwrap();
        err.await.unwrap();
        assert!(!checkpoint.read(RunCheckpoint::completed).await);
        assert!(checkpoint.read(|saved| saved.terminal_code.is_none()).await);
        super::super::super::execution::record_failure_fixture(
            &service,
            &checkpoint,
            &cancel,
            &error,
            &sensitive,
        )
        .await
        .unwrap();
        let read_id = id.clone();
        let events = service
            .store
            .read(move |db| db.events_fixture(&read_id, 0, 100))
            .await
            .unwrap();
        let messages: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "item.completed")
            .collect();
        let diagnostics: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "diagnostic")
            .collect();
        let errors: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "error")
            .collect();
        let manager = capture.0.lock().unwrap().clone();
        let final_run = service.store.run(&id).await.unwrap();
        let counts = (
            messages.len(),
            diagnostics.len(),
            manager.len(),
            errors.len(),
        );
        assert_eq!(
            counts,
            (1, 1, 1, 1),
            "all publication paths must exist exactly once"
        );
        let canonical =
            messages[0]["text"] == expected && messages[0]["payload"]["item"]["text"] == expected;
        let diagnostic = diagnostics[0]["text"] == format!("diagnostic {expected}\n");
        let unknown = manager[0] == format!("{UNKNOWN}{expected}");
        let outer = errors[0]["text"] == format!("outer {expected}")
            && final_run["error"] == format!("outer {expected}")
            && final_run["summary"] == format!("outer {expected}");
        let kinds: Vec<_> = events.iter().map(|event| text(event, "type")).collect();
        assert_eq!(kinds, ["item.completed", "diagnostic", "error"]);
        (canonical, diagnostic, unknown, outer)
    }
    .with_subscriber(dispatch)
    .await;
    drop(broker);
    assert!(!lease.home.join(crate::accounts::broker::SOCKET).exists());
    service.shutdown.cancel();
    let (canonical, diagnostic, unknown, outer) = observations;
    println!(
        "history_rotation aggregate={aggregate} independent_negative={negative} full_history_positive={positive} canonical_masked={canonical} diagnostic_masked={diagnostic} unknown_performance_masked={unknown} outer_error_masked={outer}"
    );
    assert!(
        canonical && diagnostic && unknown && outer,
        "all four actual publication paths must mask independent historical B"
    );
}
