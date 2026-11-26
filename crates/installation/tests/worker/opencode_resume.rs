use super::*;

#[test]
fn opencode_resume_error_classification_is_exact_bounded_and_redacted() {
    let cases = [
        (
            "Choose an OpenCode model in provider/model format.",
            "opencode_model_format_invalid",
        ),
        (
            "OpenCode currently requires YOLO access in an isolated execution environment. It does not implement the read-only or workspace-write sandbox.",
            "opencode_sandbox_invalid",
        ),
        (
            "OpenCode MCP server names must remain distinct after native normalization.",
            "opencode_mcp_server_name_collision",
        ),
        (
            "OpenCode MCP tool names must remain distinct after native normalization.",
            "opencode_mcp_tool_name_collision",
        ),
        (
            "Connect an OpenCode model-provider API key before starting this agent.",
            "opencode_account_lease_missing",
        ),
        ("Account not found.", "opencode_account_record_missing"),
        (
            "This OpenCode connection no longer offers the selected model. Choose another model or connection.",
            "opencode_auto_route_unavailable",
        ),
        (
            "Agent access was reduced after this run was queued. Run the task again with the current policy.",
            "agent_access_reduced",
        ),
        (
            "Isolated runner is not configured. This agent will not fall back to shared execution.",
            "isolated_runner_unconfigured",
        ),
        (
            "Configure a valid STORAGE_S3_BUCKET on the server.",
            "storage_bucket_invalid",
        ),
        (
            "Confirm that R2 public domains and bucket locks are disabled in the Cloudflare dashboard.",
            "storage_private_bucket_unconfirmed",
        ),
        (
            "STORAGE_S3_ENDPOINT must be an https:// URL.",
            "storage_endpoint_requires_https",
        ),
        (
            "Remote workspace preparation interrupted.",
            "remote_workspace_interrupted",
        ),
        (
            "Remote workspace preparation timed out after 280 seconds; source files are preserved.",
            "remote_workspace_timeout",
        ),
        (
            "An internal error occurred. Check the server logs.",
            "internal_error",
        ),
    ];
    for (error, expected_class) in cases {
        assert_eq!(classify_opencode_resume_error(Some(error)), expected_class);
    }
    assert_eq!(
        classify_opencode_resume_error(Some(
            "Remote workspace preparation failed (HTTP 404); source files are preserved.",
        )),
        "remote_workspace_http_error"
    );
    assert_eq!(classify_opencode_resume_error(None), "unknown");
    assert_eq!(
        classify_opencode_resume_error(Some("remote workspace preparation timed out")),
        "unknown"
    );
    assert_eq!(
        classify_opencode_resume_error(Some("unrecognized provider diagnostic")),
        "unknown"
    );
    assert_eq!(
        classify_opencode_resume_error(Some(
            "Remote workspace preparation failed (HTTP 40x); source files are preserved.",
        )),
        "unknown"
    );
    assert_eq!(
        classify_opencode_resume_error(Some(
            "Remote workspace preparation failed (HTTP 404); source files are preserved. extra",
        )),
        "unknown"
    );
    let boundary_error = "x".repeat(512);
    assert_eq!(
        classify_opencode_resume_error(Some(boundary_error.as_str())),
        "unknown"
    );
    let oversized_error = "x".repeat(513);
    assert_eq!(
        classify_opencode_resume_error(Some(oversized_error.as_str())),
        "unknown"
    );
}

#[tokio::test]
async fn controller_reads_do_not_launch_vm_attempts() {
    let root = TempDir::new().unwrap();
    let controller = Arc::new(Controller {
        data: root.path().to_owned(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 1,
        hold: std::sync::atomic::AtomicBool::new(false),
    });
    let request = |path| {
        Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap()
    };
    let response = serve_controller(
        State(controller.clone()),
        request("/runs/fixture/administrative-pause"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["administrativePausedMs"], 0);

    let unknown = serve_controller(
        State(controller.clone()),
        request("/runs/fixture/unknown-read"),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert!(controller.plans.lock().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn controller_held_attempt_keeps_output_open_until_released() {
    use futures_util::StreamExt;

    let root = TempDir::new().unwrap();
    let controller = Arc::new(Controller {
        data: root.path().to_owned(),
        plans: tokio::sync::Mutex::new(vec![json!({ "id": "fixture" })]),
        failures: 0,
        slots: 1,
        hold: std::sync::atomic::AtomicBool::new(true),
    });
    let request = Request::builder()
        .uri("/runs/fixture/logs")
        .body(Body::empty())
        .unwrap();
    let response = serve_controller(State(controller.clone()), request).await;
    let mut stream = response.into_body().into_data_stream();
    let output: Value = serde_json::from_slice(&stream.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(output["type"], "output");

    // The runner reconciles a clean output EOF after five seconds.
    tokio::time::advance(Duration::from_secs(6)).await;
    let heartbeat = stream
        .next()
        .await
        .expect("a held attempt must keep its output connection alive")
        .unwrap();
    let heartbeat: Value = serde_json::from_slice(&heartbeat).unwrap();
    assert_eq!(heartbeat["type"], "heartbeat");

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn node_slots_exceed_manager_concurrency_without_overpreparing_the_queue() {
    let mut fixture = Fixture::new().await;
    // One extra node slot exercises the manager concurrency boundary.
    let slots = fixture.service.config.concurrency + 1;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots,
        hold: std::sync::atomic::AtomicBool::new(true),
    });
    let app = Router::new()
        .fallback(any(serve_controller))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let home = fixture.service.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let mut node = fixture
        .service
        .store
        .node_boundary(LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    node["slots"] = slots.into();
    fixture.service.store.put_node_fixture(node).await.unwrap();
    let mut runs = Vec::new();
    for _ in 0..=slots {
        runs.push(fixture.enqueue("Inspect the VM fixture").await);
    }
    fixture.start().await;
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || (controller.plans.lock().await.len() == slots).then_some(()),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let mut queued = Vec::new();
    let mut running = 0;
    for run in &runs {
        let current = fixture.service.store.run(text(run, "id")).await.unwrap();
        if current["status"] == RunStatus::Queued {
            queued.push(current);
        } else {
            assert_eq!(current["status"], RunStatus::Running, "{current}");
            running += 1;
        }
    }
    assert_eq!(running, slots);
    assert_eq!(queued.len(), 1);
    assert!(
        queued[0]["startedAt"].is_null(),
        "queued work must never enter recovery: {}",
        queued[0]
    );
    assert_eq!(queued[0]["recoveryPending"], false);
    assert_eq!(controller.plans.lock().await.len(), slots);
    let attempts = fixture.service.store.attempts().await.unwrap();
    assert_eq!(attempts.len(), slots);
    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    for run in &runs {
        assert_eq!(
            fixture.until_finished(text(run, "id")).await["status"],
            RunStatus::Succeeded
        );
    }
    assert_eq!(controller.plans.lock().await.len(), slots + 1);
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn vm_agents_call_workspace_tools_through_their_run_channel_without_a_public_url() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: std::sync::atomic::AtomicBool::new(false),
    });
    // A token granted to another run of the same installation.
    let other_run = json!({ "id": id(), "snapshot": { "agent": { "id": MAIN_AGENT_ID, "provider": "codex" } } });
    let other = fixture
        .service
        .mcps
        .run_configuration(&fixture.service, &other_run)
        .await
        .unwrap();
    let probe = Arc::new(WorkspaceProbe {
        controller: controller.clone(),
        other_run_token: text(&other["env"], "CAIRN_MCP_RUN_TOKEN").to_owned(),
        calls: tokio::sync::Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .fallback(any(serve_workspace_probe))
        .with_state(probe.clone());
    let (listener, address) = common::bind().await;
    // The installer's private manager origin: unreachable from inside a VM.
    common::reconfigure(&mut fixture.service, |config| {
        config.public_url = "http://manager:4310".into();
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let home = fixture.service.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let run = fixture.enqueue("Open a project").await;
    fixture.start().await;

    let finished = fixture.until_finished(text(&run, "id")).await;

    assert_eq!(finished["status"], RunStatus::Succeeded, "{finished}");
    let calls = probe.calls.lock().await.clone();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let call = &calls[0];
    assert_eq!(call["url"], "http://127.0.0.1:5202/mcp-workspace", "{call}");
    assert_eq!(call["reachable"], true, "{call}");
    assert_eq!(call["status"], 200, "{call}");
    assert!(call["reply"]["error"].is_null(), "{call}");
    assert_ne!(call["reply"]["result"]["isError"], true, "{call}");
    assert_eq!(call["otherRun"], 401, "{call}");
    for outside in call["outside"].as_array().unwrap() {
        assert_eq!(outside[1], 404, "{outside}");
    }
    // Disk reads still use the private origin; the agent never does.
    let args = controller.plans.lock().await[0]["args"].to_string();
    assert!(!args.contains("manager:4310"), "{args}");
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn refused_local_budget_keeps_presence_and_exposes_the_reason() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: std::sync::atomic::AtomicBool::new(false),
    });
    let app = Router::new()
        .fallback(any(serve_controller))
        .with_state(controller);
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let mut node = fixture
        .service
        .store
        .node_boundary(LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    node["limits"]["memoryMiB"] = 256.into();
    node["lastSeen"] = 0.into();
    fixture.service.store.put_node_fixture(node).await.unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let node = fixture
        .service
        .store
        .node_boundary(LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    assert!(node["lastSeen"].as_i64().unwrap() > now() - 1000);
    assert_eq!(node["executionReady"], false);
    assert_eq!(node["capabilities"]["cpu"], 8);
    assert!(text(&node, "budgetError").contains("below current usage"));
    let mut restored = node;
    restored["limits"]["memoryMiB"] = 15872.into();
    fixture
        .service
        .store
        .put_node_fixture(restored)
        .await
        .unwrap();
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let node = fixture
        .service
        .store
        .node_boundary(LOCAL_NODE_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(node["executionReady"], true);
    assert_eq!(node["budgetError"], Value::Null);
    server.abort();
}

#[tokio::test]
async fn paid_recovery_survives_confirmed_capacity_rejection_and_restart() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for provider in [false, true] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        });
        let busy = Arc::new(AtomicBool::new(true));
        let rejections = Arc::new(AtomicUsize::new(0));
        let (reject, seen) = (busy.clone(), rejections.clone());
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let (reject, seen) = (reject.clone(), seen.clone());
                    async move {
                        let path = request.uri().path();
                        if request.method() == "POST"
                            && path.starts_with("/runs/")
                            && path.trim_matches('/').split('/').count() == 2
                            && reject.load(Ordering::SeqCst)
                        {
                            seen.fetch_add(1, Ordering::SeqCst);
                            return (
                                StatusCode::SERVICE_UNAVAILABLE,
                                Json(json!({ "error": "All VM slots are occupied." })),
                            )
                                .into_response();
                        }
                        serve_controller(state, request).await
                    }
                },
            ))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let service = fixture.service.clone();
        let home = service.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        let run = fixture
            .enqueue("Preserve paid admission after refusal")
            .await;
        let id = text(&run, "id").to_owned();
        fixture.start().await;
        let first = fixture
            .until(&id, |run| {
                finished(run)
                    || (rejections.load(Ordering::SeqCst) >= 1
                        && run["status"] == RunStatus::Queued
                        && !run["recoveryPending"].as_bool().unwrap_or(true))
            })
            .await;
        assert_eq!(first["status"], RunStatus::Queued, "{first}");
        fixture.stop(false).await;
        let mut saved = fixture.checkpoint(&id).await;
        let old_attempt = text(&saved, "runnerId").to_owned();
        assert_eq!(saved["startRejected"], true);
        saved["capacityRetryAt"] = (now() - 1).into();
        saved["controllerRecoveries"] = 3.into();
        if provider {
            let cause: cairn_installation::run_retry::Cause = serde_json::from_value(json!({
                "kind": "server", "message": "Provider busy", "startup": true
            }))
            .unwrap();
            let mut retry = cairn_installation::run_retry::Retry::next(None, cause, now()).unwrap();
            retry.next_attempt_at = Some(now() - 1);
            saved["retryCause"] = serde_json::to_value(&retry.cause).unwrap();
            saved["providerRetryReservation"] = old_attempt.into();
            service
                .store
                .change_run(
                    &id,
                    cairn_installation::runs::RunChanges {
                        retry: Some(Some(retry)),
                        recovery_pending: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        } else {
            saved["controllerRecoveryReservation"] = old_attempt.into();
            service
                .store
                .change_run(
                    &id,
                    cairn_installation::runs::RunChanges {
                        recovery_pending: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        service
            .store
            .save_checkpoint_boundary(&id, saved)
            .await
            .unwrap();
        fixture.start().await;
        let waiting = fixture
            .until(&id, |run| {
                finished(run)
                    || (rejections.load(Ordering::SeqCst) >= 2
                        && run["status"] == RunStatus::Queued
                        && !run["recoveryPending"].as_bool().unwrap_or(true))
            })
            .await;
        assert_eq!(
            waiting["status"],
            RunStatus::Queued,
            "provider={provider}: {waiting}"
        );
        fixture.stop(false).await;
        let mut saved = fixture.checkpoint(&id).await;
        assert_eq!(saved["startRejected"], true);
        assert_eq!(saved["controllerRecoveries"], 3);
        assert!(controller.plans.lock().await.is_empty());
        saved["capacityRetryAt"] = (now() - 1).into();
        service
            .store
            .save_checkpoint_boundary(&id, saved)
            .await
            .unwrap();
        busy.store(false, Ordering::SeqCst);
        fixture.start().await;
        let completed = fixture.until_finished(&id).await;
        assert_eq!(
            completed["status"],
            RunStatus::Succeeded,
            "provider={provider}: {completed}"
        );
        assert_eq!(fixture.checkpoint(&id).await["controllerRecoveries"], 3);
        assert_eq!(controller.plans.lock().await.len(), 1);
        fixture.stop(false).await;
        server.abort();
    }
}

#[tokio::test]
async fn transferred_paid_move_launches_once_without_resetting_controller_count() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(false),
    });
    let busy = Arc::new(AtomicBool::new(true));
    let rejections = Arc::new(AtomicUsize::new(0));
    let (reject, seen) = (busy.clone(), rejections.clone());
    let app = Router::new()
        .fallback(any(
            move |state: State<Arc<Controller>>, request: Request| {
                let (reject, seen) = (reject.clone(), seen.clone());
                async move {
                    let path = request.uri().path();
                    if request.method() == "POST"
                        && path.starts_with("/runs/")
                        && path.trim_matches('/').split('/').count() == 2
                        && reject.load(Ordering::SeqCst)
                    {
                        seen.fetch_add(1, Ordering::SeqCst);
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({ "error": "All VM slots are occupied." })),
                        )
                            .into_response();
                    }
                    serve_controller(state, request).await
                }
            },
        ))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let service = fixture.service.clone();
    let home = service.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    let run = fixture.enqueue("Resume paid move").await;
    let id = text(&run, "id").to_owned();
    fixture.start().await;
    let waiting = fixture
        .until(&id, |run| {
            finished(run)
                || (rejections.load(Ordering::SeqCst) >= 1
                    && run["status"] == RunStatus::Queued
                    && run["recoveryPending"] == false)
        })
        .await;
    assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
    fixture.stop(false).await;
    let mut saved = fixture.checkpoint(&id).await;
    let old = text(&saved, "runnerId").to_owned();
    saved["startRejected"] = false.into();
    saved["terminalCode"] = 75.into();
    saved["capacityRetryAt"] = (now() - 1).into();
    saved["controllerRecoveries"] = 4.into();
    saved["controllerRecoveryReservation"] = old.clone().into();
    saved["moveRecoveryReservation"] = old.into();
    service
        .store
        .save_checkpoint_boundary(&id, saved)
        .await
        .unwrap();
    service
        .store
        .change_run(
            &id,
            cairn_installation::runs::RunChanges {
                recovery_pending: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    busy.store(false, Ordering::SeqCst);
    fixture.start().await;
    let completed = fixture.until_finished(&id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    let saved = fixture.checkpoint(&id).await;
    assert_eq!(saved["controllerRecoveries"], 4);
    assert!(saved["moveRecoveryReservation"].is_null());
    assert_eq!(controller.plans.lock().await.len(), 1);
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn a_post_expiration_protocol_failure_releases_its_reservation_and_retries() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(false),
    });
    let unavailable = Arc::new(AtomicBool::new(false));
    let rejected = Arc::new(AtomicUsize::new(0));
    let owner = Arc::new(tokio::sync::Mutex::new(None::<String>));
    let (fail, observed, target, store) = (
        unavailable.clone(),
        rejected.clone(),
        owner.clone(),
        fixture.service.store.clone(),
    );
    let app = Router::new()
        .fallback(any(
            move |state: State<Arc<Controller>>, request: Request| {
                let (fail, observed, target, store) = (
                    fail.clone(),
                    observed.clone(),
                    target.clone(),
                    store.clone(),
                );
                async move {
                    if request.uri().path() == "/health"
                        && request.headers().contains_key("authorization")
                        && fail.load(Ordering::SeqCst)
                    {
                        let run = target.lock().await.clone().unwrap();
                        let reserved = store.attempts().await.unwrap().into_iter().any(|attempt| {
                            attempt.run_id == run && !attempt.released && !attempt.disk_materialized
                        });
                        if reserved {
                            observed.fetch_add(1, Ordering::SeqCst);
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                    }
                    serve_controller(state, request).await
                }
            },
        ))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let s = fixture.service.clone();
    let home = s.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket":"fixture-storage","endpoint":"https://127.0.0.1:1" }).to_string(),
    )
    .await
    .unwrap();
    fixture.start().await;
    let chat = fixture.start_chat(message("First healthy VM turn")).await;
    let run = fixture.chat_run(&chat).await;
    assert_eq!(
        fixture.until_finished(&run).await["status"],
        RunStatus::Succeeded
    );
    fixture.until_worker_quiescent().await;
    fixture.stop(false).await;
    // This is an ordinary turn of an already admitted fresh environment, rather
    // than the first admission path which binds in resolve_backend.
    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["access"]["backend"] = "firecracker".into();
    s.store.put_agent_fixture(agent.clone()).await.unwrap();
    let run_id = run.clone();
    s.store.transaction(move |db| {
        let mut snapshot = db.run(&run_id)?.unwrap()["snapshot"].clone();
        snapshot["agent"] = agent;
        db.change_run(&run_id,cairn_installation::runs::RunChanges { snapshot:Some(serde_json::from_value(snapshot)?), ..Default::default() })?;
        db.update_run_boundary(&run_id,&json!({ "environmentExpiredAt":1,"environmentExpirationCause":"node-change","environmentState":null }))
    }).await.unwrap();
    *owner.lock().await = Some(run.clone());
    unavailable.store(true, Ordering::SeqCst);
    s.chat_send(&chat, message("Retry after protocol health recovers"))
        .await
        .unwrap();
    fixture.start().await;
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || (rejected.load(Ordering::SeqCst) > 0).then_some(()),
    )
    .await;
    fixture
        .until(&run, |run| run["status"] == RunStatus::Queued)
        .await;
    fixture.until_worker_quiescent().await;
    assert!(
        s.store
            .attempts()
            .await
            .unwrap()
            .iter()
            .all(|attempt| attempt.run_id != run || attempt.released),
        "a failed health check retained unbound capacity"
    );
    unavailable.store(false, Ordering::SeqCst);
    s.store
        .change_run(
            &run,
            cairn_installation::runs::RunChanges {
                capacity_wait_until: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let resumed = fixture.until_finished(&run).await;
    assert_eq!(resumed["status"], RunStatus::Succeeded, "{resumed}");
    assert_eq!(controller.plans.lock().await.len(), 2);
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn a_first_expired_admission_failure_preserves_its_replaceable_destination() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let controller = Arc::new(Controller {
        data: fixture.service.config.data_dir.clone(),
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(false),
    });
    let unavailable = Arc::new(AtomicBool::new(false));
    let rejected = Arc::new(AtomicUsize::new(0));
    let owner = Arc::new(tokio::sync::Mutex::new(None::<String>));
    let (fail, observed, target, store) = (
        unavailable.clone(),
        rejected.clone(),
        owner.clone(),
        fixture.service.store.clone(),
    );
    let app = Router::new()
        .fallback(any(
            move |state: State<Arc<Controller>>, request: Request| {
                let (fail, observed, target, store) = (
                    fail.clone(),
                    observed.clone(),
                    target.clone(),
                    store.clone(),
                );
                async move {
                    if request.uri().path() == "/health"
                        && request.headers().contains_key("authorization")
                        && fail.load(Ordering::SeqCst)
                    {
                        let run = target.lock().await.clone().unwrap();
                        let reserved = store.attempts().await.unwrap().into_iter().any(|attempt| {
                            attempt.run_id == run && !attempt.released && !attempt.disk_materialized
                        });
                        if reserved {
                            observed.fetch_add(1, Ordering::SeqCst);
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                    }
                    serve_controller(state, request).await
                }
            },
        ))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(listener, app);
    let s = fixture.service.clone();
    let home = s.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    tokio::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({ "bucket":"fixture-storage","endpoint":"https://127.0.0.1:1" }).to_string(),
    )
    .await
    .unwrap();
    fixture.start().await;
    let chat = fixture.start_chat(message("First healthy VM turn")).await;
    let run = fixture.chat_run(&chat).await;
    assert_eq!(
        fixture.until_finished(&run).await["status"],
        RunStatus::Succeeded
    );
    fixture.until_worker_quiescent().await;
    fixture.stop(false).await;
    // Start a real first admission after inactivity expiration, with an imposed
    // destination that must remain replaceable throughout preparation.
    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["access"]["backend"] = "firecracker".into();
    s.store.put_agent_fixture(agent.clone()).await.unwrap();
    let (run_id, chat_id) = (run.clone(), chat.clone());
    s.store
        .transaction(move |db| {
            let mut snapshot = db.run(&run_id)?.unwrap()["snapshot"].clone();
            snapshot["agent"] = agent;
            let old = now() - 8 * 86_400_000;
            db.change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    snapshot: Some(serde_json::from_value(snapshot)?),
                    finished_at: Some(Some(old)),
                    ..Default::default()
                },
            )?;
            let mut retained = db.conversation(&chat_id)?.unwrap();
            retained.created_at = old;
            retained.updated_at = old;
            retained.last_activity_at = Some(old);
            db.save_conversation(&retained)?;
            Ok(())
        })
        .await
        .unwrap();
    s.save_conversation_retention(json!({ "inactivityDays":7 }))
        .await
        .unwrap();
    s.cleanup_conversations().await.unwrap();
    s.detach_stopped_environments().await.unwrap();
    s.retire_environments().await.unwrap();
    let expired = s.store.run(&run).await.unwrap();
    assert_eq!(expired["environmentState"], "expired");
    cairn_installation::nodes::moves::request(&s, &expired, &json!({ "nodeId":LOCAL_NODE_ID }))
        .await
        .unwrap();
    assert_eq!(
        s.store.run(&run).await.unwrap()["targetNodeId"],
        LOCAL_NODE_ID
    );
    *owner.lock().await = Some(run.clone());
    unavailable.store(true, Ordering::SeqCst);
    s.chat_send(&chat, message("Retry after protocol health recovers"))
        .await
        .unwrap();
    fixture.start().await;
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || (rejected.load(Ordering::SeqCst) > 0).then_some(()),
    )
    .await;
    fixture
        .until(&run, |run| run["status"] == RunStatus::Queued)
        .await;
    fixture.until_worker_quiescent().await;
    assert!(
        s.store
            .attempts()
            .await
            .unwrap()
            .iter()
            .all(|attempt| attempt.run_id != run || attempt.released),
        "a failed health check retained unbound capacity"
    );
    let pending = s.store.run(&run).await.unwrap();
    assert_eq!(pending["environmentState"], "expired");
    assert_eq!(pending["targetNodeId"], LOCAL_NODE_ID);
    assert!(pending["nodeId"].is_null());
    // Reconfiguration is still the expired branch: no second discard confirmation.
    s.store
        .change_run(
            &run,
            cairn_installation::runs::RunChanges {
                capacity_wait_until: Some(Some(now() + 60_000)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    cairn_installation::nodes::moves::request(&s, &pending, &json!({ "nodeId":LOCAL_NODE_ID }))
        .await
        .unwrap();
    unavailable.store(false, Ordering::SeqCst);
    s.store
        .change_run(
            &run,
            cairn_installation::runs::RunChanges {
                capacity_wait_until: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let resumed = fixture.until_finished(&run).await;
    assert_eq!(resumed["status"], RunStatus::Succeeded, "{resumed}");
    assert_eq!(controller.plans.lock().await.len(), 2);
    assert!(resumed["environmentState"].is_null());
    assert!(resumed["targetNodeId"].is_null());
    assert!(resumed["pinnedNodeId"].is_null());
    assert_eq!(resumed["nodeId"], LOCAL_NODE_ID);
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn controller_capacity_rejection_waits_and_retries_without_an_agent_session() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    for (status, reason, capacity, deferred_fence) in [
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "All VM slots are occupied.",
            true,
            true,
        ),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Shared node resources are under pressure.",
            true,
            false,
        ),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Temporary admission failure.",
            false,
            false,
        ),
        (
            StatusCode::FORBIDDEN,
            "All VM slots are occupied.",
            false,
            false,
        ),
    ] {
        let mut fixture = Fixture::new().await;
        fixture.stop(false).await;
        let controller = Arc::new(Controller {
            data: fixture.service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        });
        let busy = Arc::new(AtomicBool::new(true));
        let rejections = Arc::new(AtomicUsize::new(0));
        let fence_blocked = Arc::new(AtomicBool::new(deferred_fence));
        let reject = busy.clone();
        let count = rejections.clone();
        let fence = fence_blocked.clone();
        let app = Router::new()
            .fallback(any(
                move |state: State<Arc<Controller>>, request: Request| {
                    let busy = reject.clone();
                    let rejections = count.clone();
                    let fence_blocked = fence.clone();
                    async move {
                        let path = request.uri().path();
                        if request.method() == "DELETE" && fence_blocked.load(Ordering::SeqCst) {
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        let starting = request.method() == "POST"
                            && path.starts_with("/runs/")
                            && path.trim_matches('/').split('/').count() == 2;
                        if starting && busy.load(Ordering::SeqCst) {
                            rejections.fetch_add(1, Ordering::SeqCst);
                            return (status, Json(json!({ "error": reason }))).into_response();
                        }
                        serve_controller(state, request).await
                    }
                },
            ))
            .with_state(controller.clone());
        let (listener, address) = common::bind().await;
        common::reconfigure(&mut fixture.service, |config| {
            config.runner_url = format!("http://{address}");
        })
        .await;
        let server = common::serve(listener, app);
        let s = fixture.service.clone();
        let home = s.config.home.join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        tokio::fs::write(home.join("auth.json"), "{}")
            .await
            .unwrap();
        let run = fixture.enqueue("Wait for an available VM slot").await;
        let run_id = text(&run, "id");
        fixture.start().await;
        let waiting = fixture
            .until(run_id, |run| {
                finished(run)
                    || (run["status"] == RunStatus::Queued
                        && (text(run, "accountWaitReason").starts_with("Waiting for capacity.")
                            || (deferred_fence
                                && rejections.load(Ordering::SeqCst) > 0
                                && run["recoveryPending"] == true)))
            })
            .await;
        if !capacity {
            assert_eq!(waiting["status"], RunStatus::Failed, "{waiting}");
            fixture.stop(false).await;
            server.abort();
            continue;
        }
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        assert!(waiting["sessionId"].is_null());
        assert!(waiting["error"].is_null());
        assert!(waiting["finishedAt"].is_null());
        assert!(waiting["retry"].is_null());
        assert_eq!(fixture.checkpoint(run_id).await["launched"], false);
        if deferred_fence {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            assert_eq!(
                rejections.load(Ordering::SeqCst),
                1,
                "No relaunch before fencing"
            );
            assert!(fixture.checkpoint(run_id).await["settled"].is_null());
            fence_blocked.store(false, Ordering::SeqCst);
        }
        let waiting = fixture
            .until(run_id, |run| {
                finished(run) || text(run, "accountWaitReason").starts_with("Waiting for capacity.")
            })
            .await;
        assert_eq!(waiting["status"], RunStatus::Queued, "{waiting}");
        assert!(text(&waiting, "accountWaitReason").contains(reason));
        let workspace = text(&waiting, "workspace").to_owned();
        let marker = Path::new(&workspace).join("preserved.txt");
        std::fs::write(&marker, "preserved while waiting").unwrap();

        // Capacity waits survive a manager restart and never consume the bounded
        // recovery budget intended for interrupted native agent sessions.
        fixture.stop(false).await;
        fixture.start().await;
        let previous_rejections = rejections.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(
            rejections.load(Ordering::SeqCst),
            previous_rejections,
            "Capacity wait must survive restart without relaunching immediately"
        );
        for _ in 0..4 {
            fixture.stop(false).await;
            let mut saved = fixture.checkpoint(run_id).await;
            saved["capacityRetryAt"] = (cairn_installation::config::now() - 1).into();
            s.store
                .save_checkpoint_boundary(run_id, saved)
                .await
                .unwrap();
            let previous = rejections.load(Ordering::SeqCst);
            fixture.start().await;
            eventually(
                Duration::from_secs(10),
                Duration::from_millis(30),
                async || (rejections.load(Ordering::SeqCst) > previous).then_some(()),
            )
            .await;
            fixture
                .until(run_id, |run| run["status"] == RunStatus::Queued)
                .await;
        }
        assert!(controller.plans.lock().await.is_empty());
        fixture.stop(false).await;
        let mut saved = fixture.checkpoint(run_id).await;
        saved["capacityRetryAt"] = (cairn_installation::config::now() - 1).into();
        s.store
            .save_checkpoint_boundary(run_id, saved)
            .await
            .unwrap();
        busy.store(false, Ordering::SeqCst);
        fixture.start().await;
        let completed = fixture.until_finished(run_id).await;
        assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
        assert_eq!(completed["workspace"], workspace);
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "preserved while waiting"
        );
        assert_eq!(controller.plans.lock().await.len(), 1);
        fixture.stop(false).await;
        server.abort();
    }
}
