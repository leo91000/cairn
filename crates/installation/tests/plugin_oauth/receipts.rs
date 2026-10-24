//! A cached mutation receipt must retain the effective personal authorization.
use super::*;

#[tokio::test]
async fn personal_http_receipts_cannot_be_borrowed_by_another_requester() {
    let (_root, s, instance, resource, path, app) = configured_fixture(false, true).await;
    let base = registry::active(&s, "example").await.unwrap();
    let mut manifest = serde_json::to_value(base.manifest).unwrap();
    manifest["version"] = json!("2");
    manifest["operations"][0]["requests"][0]["method"] = json!("POST");
    let package = registry::install(&s, &json!({
        "manifest": manifest,
        "source": r#"return {tool = function(event)
            return cairn.http.request({method = 'POST', url = 'https://api.example.test/messages',
                auth = {resource = event.input.resource, oauth = 'bot'},
                idempotencyKey = 'same-logical-request', idempotencyInput = {purpose = 'read-own-receipt'}}).json
        end}"#,
    })).await.unwrap();
    registry::approve(&s, "example", "2", text(&package, "digest"))
        .await
        .unwrap();

    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(|| async {
            axum::Json(json!({
                "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                "members": [
                    {"account_id": "alice", "access_id": "alice-membership-1"},
                    {"account_id": "bob", "access_id": "bob-membership-1"}
                ]
            }))
        }),
    );
    let (origin, authority_server) = common::serve_locally(authority).await;
    let identity_file = s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(&identity_file).unwrap()).unwrap();
    identity["origin"] = json!(origin);
    std::fs::write(identity_file, serde_json::to_vec(&identity).unwrap()).unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let provider = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(
                |axum::Form(form): axum::Form<HashMap<String, String>>| async move {
                    let (subject, token) = match form["code"].as_str() {
                        "alice-code" => ("U111", "receipt-alice-fixture"),
                        "bob-code" => ("U222", "receipt-bob-fixture"),
                        code => panic!("Unexpected fixture code: {code}"),
                    };
                    axum::Json(json!({"access_token": token, "scope": "messages:read",
                "team": {"id": "T111"}, "authed_user": {"id": subject}}))
                },
            ),
        )
        .route(
            "/messages",
            axum::routing::post(move |headers: axum::http::HeaderMap| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let subject = match headers["authorization"].to_str().unwrap() {
                        "Bearer receipt-alice-fixture" => "U111",
                        "Bearer receipt-bob-fixture" => "U222",
                        _ => panic!("Unexpected personal credential"),
                    };
                    axum::Json(json!({"subject": subject}))
                }
            }),
        );
    let (origin, provider_server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    for account in ["alice", "bob"] {
        let started = common::send(
            &app,
            member_request("POST", &path, json!({"scopes": ["messages:read"]}), account),
        )
        .await;
        assert_eq!(started.status(), StatusCode::OK);
        let started = common::read_json(started).await;
        let callback = format!(
            "/api/plugin-oauth/{instance}/callback?state={}&code={account}-code",
            text(&started, "state")
        );
        assert_eq!(
            common::send(&app, request("GET", &callback, Value::Null, None))
                .await
                .status(),
            StatusCode::OK
        );
        let finished = common::send(
            &app,
            member_request(
                "POST",
                &format!("{path}/finish"),
                json!({"state": started["state"]}),
                account,
            ),
        )
        .await;
        assert_eq!(
            finished.status(),
            StatusCode::OK,
            "{}",
            common::read_text(finished).await
        );
    }
    let grant_path = format!(
        "/api/plugins/example/connections/{instance}/resources/workspace/{resource}/grants"
    );
    let granted = common::send(
        &app,
        request(
            "POST",
            &grant_path,
            json!({"agentId": MAIN_AGENT_ID, "profile": "read"}),
            Some("fixture-owner"),
        ),
    )
    .await;
    assert_eq!(granted.status(), StatusCode::OK);

    for (account, subject) in [("alice", "U111"), ("bob", "U222")] {
        let chat = s
            .chat_create(json!({"agentId": MAIN_AGENT_ID}))
            .await
            .unwrap();
        let message_path = format!("/api/chats/{}/messages", text(&chat, "id"));
        let sent = common::send(
            &app,
            member_request(
                "POST",
                &message_path,
                json!({"id": cairn_installation::config::id(), "text": "Read only my own receipt"}),
                account,
            ),
        )
        .await;
        assert_eq!(sent.status(), StatusCode::OK);
        s.chat_tick(&HashSet::new()).await.unwrap();
        let detail = s.chat_detail(text(&chat, "id")).await.unwrap();
        let run = s
            .store
            .patch_run(
                text(&detail["run"], "id"),
                json!({"status": "running", "isolated": true}),
            )
            .await
            .unwrap();
        let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
        let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
        let result =
            agents::call_tool(&s, bearer, "example_read", &json!({"resource": resource})).await;
        match result {
            Ok(Some(result)) => assert_eq!(
                result["subject"], subject,
                "another requester's cached personal result escaped"
            ),
            Err(error) if account == "bob" => assert_eq!(
                error.status, 409,
                "a conflicting key must refuse rather than borrow another person's result"
            ),
            other => panic!("Unexpected personal tool result: {other:?}"),
        }
        if account == "alice" {
            let result =
                agents::call_tool(&s, bearer, "example_read", &json!({"resource": resource}))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(result["subject"], subject);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "an authorized replay repeated provider I/O"
            );
        }
    }
    assert!((1..=2).contains(&calls.load(Ordering::SeqCst)));
    provider_server.abort();
    authority_server.abort();
}

#[tokio::test]
async fn an_implicit_get_cannot_claim_a_mutation_receipt() {
    let (_root, s, instance, _resource, _path, _app) = configured_fixture(false, false).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let provider = axum::Router::new().route(
        "/messages",
        axum::routing::get(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"ok": true}))
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let package = registry::active(&s, "example").await.unwrap();
    let context = capabilities::Context {
        management: None,
        connection: connections::get(&s, "example", &instance).await.unwrap(),
        manifest: package.manifest,
        event: Value::Null,
        link: None,
        draft: None,
        resource_grant: None,
    };
    let error = capabilities::checked(
        &s,
        &context,
        "http.request",
        json!({
            "url": "https://api.example.test/messages", "idempotencyKey": "implicit-get",
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, 400);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        s.store
            .list("plugin-http-receipts")
            .await
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "local receipt storage observation; run alone with --nocapture --test-threads=1"]
async fn measure_authority_receipt_storage_shape() {
    use cairn_installation::store::Store;
    use std::time::Instant;

    fn cpu_ms() -> f64 {
        let mut clock = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock is writable and CLOCK_PROCESS_CPUTIME_ID is supported.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut clock) },
            0
        );
        clock.tv_sec as f64 * 1000.0 + clock.tv_nsec as f64 / 1_000_000.0
    }

    let mut cases = Vec::new();
    for authority_bound in [false, true] {
        let root = TempDir::new().unwrap();
        let input = json!({"method": "POST", "url": "https://api.example.test/messages", "input": {"message": "fixture"}});
        let mut fingerprint = input.clone();
        if authority_bound {
            fingerprint["authority"] = json!({"connectionRevision": "00000000-0000-4000-8000-000000000002",
                "grant": {"id": "fixture-grant", "revision": "fixture-revision", "requester": {"account": "alice", "accessId": "membership-1"}},
                "resource": [["fixture-resource", "resource-revision"]],
                "oauth": {"key": "fixture-authorization", "revision": "oauth-revision"}});
        }
        let fingerprint =
            cairn_installation::auth::hex_digest(&serde_json::to_string(&fingerprint).unwrap());
        let rows = (0..1000)
            .map(|index| {
                json!({
                    "id": format!("00000000-0000-4000-8000-000000000001:{index:064x}"),
                    "connectionId": "00000000-0000-4000-8000-000000000001",
                    "fingerprint": fingerprint,
                    "state": "completed", "createdAt": 1000,
                    "response": {"status": 200, "headers": {}, "json": {"ok": true}, "text": null},
                })
            })
            .collect::<Vec<_>>();
        let bytes: usize = rows
            .iter()
            .map(|row| text(row, "id").len() + serde_json::to_vec(row).unwrap().len())
            .sum();
        let store = Store::open(root.path()).unwrap();
        let seed = rows.clone();
        store
            .transaction(move |db| {
                for row in seed {
                    db.put("plugin-http-receipts", &row)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        drop(store);
        let store = Store::open(root.path()).unwrap();
        let value = rows.last().unwrap().clone();
        let key = text(&value, "id").to_owned();
        let lookup = key.clone();
        let reads = store.read(move |db| {
            let mut samples = Vec::new();
            for sample in 0..6 {
                let cpu = cpu_ms();
                let at = Instant::now();
                let value = db.get("plugin-http-receipts", &lookup)?.unwrap();
                samples.push(json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64()*1000.0,
                    "processCpuMs": cpu_ms()-cpu, "returnedJsonBytes": serde_json::to_vec(&value)?.len()}));
            }
            Ok(samples)
        }).await.unwrap();
        let mut writes = Vec::new();
        for sample in 0..6 {
            let cpu = cpu_ms();
            let at = Instant::now();
            store
                .put("plugin-http-receipts", value.clone())
                .await
                .unwrap();
            writes.push(json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64()*1000.0, "processCpuMs": cpu_ms()-cpu}));
        }
        cases.push(json!({"authorityBound": authority_bound, "records": rows.len(), "encodedIdAndJsonBytes": bytes, "reads": reads, "writes": writes}));
    }
    eprintln!(
        "AUTHORITY_RECEIPT_STORAGE {}",
        json!({
            "cases": cases,
            "workload": "1000 representative completed native records of identical shape, key lengths and response; only the SHA256 fingerprint input changes; six reads and same-value writes per case",
            "cache": "new Store after seeding, first connection-cold then five warm reads; OS cache retained",
            "limits": "encoded IDs and JSON exclude SQLite pages, WAL, indices and kind string; reads exclude actor scheduling, writes include it; does not measure authority lookup/hash, network or full plugin call; no speedup claim",
            "host": {"parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
                "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(), "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()}
        })
    );
}
