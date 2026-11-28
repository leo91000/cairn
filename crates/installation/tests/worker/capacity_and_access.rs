use super::*;

#[tokio::test]
async fn chat_resume_selection_resolves_opencode_catalog_default_before_adapter_validation() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut fixture = Fixture::with_opencode().await;
    fixture.stop(false).await;
    let data_dir = fixture.service.config.data_dir.clone();
    let controller = Arc::new(Controller {
        data: data_dir,
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(true),
    });
    let log_request_seen = Arc::new(AtomicBool::new(false));
    let plan_received = Arc::new(AtomicBool::new(false));
    let app = Router::new()
        .fallback(any({
            let log_request_seen = log_request_seen.clone();
            let plan_received = plan_received.clone();
            move |state: State<Arc<Controller>>, request: Request| {
                let log_request_seen = log_request_seen.clone();
                let plan_received = plan_received.clone();
                async move {
                    let path = request.uri().path();
                    if path.starts_with("/runs/") && path.ends_with("/logs") {
                        log_request_seen.store(true, Ordering::Relaxed);
                    }
                    let is_plan_post = request.method() == axum::http::Method::POST
                        && path
                            .strip_prefix("/runs/")
                            .is_some_and(|attempt| !attempt.is_empty() && !attempt.contains('/'));
                    let response = serve_controller(state, request).await;
                    if is_plan_post && response.status().is_success() {
                        plan_received.store(true, Ordering::Relaxed);
                    }
                    response
                }
            }
        }))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let s = fixture.service.clone();
    let server = common::serve(listener, app);
    let account = s
        .accounts
        .connect_api_key(
            &s,
            Provider::Opencode,
            "OpenCode Zen",
            "opencode",
            "synthetic-resume-selection-key",
            None,
        )
        .await
        .unwrap();
    let account_id = account.id.to_string();
    let catalog = s.opencode.models(&s).await.unwrap();
    let default_catalog_entry = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["isDefault"] == true)
        .unwrap();
    let default_model = text(default_catalog_entry, "model").to_owned();
    assert_eq!(default_model, "auto/ling-3-1-flash-free");
    let expected_plan_model = default_catalog_entry["routes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|route| route["provider"] == "opencode" && route["hidden"] != true)
        .map(|route| text(route, "model"))
        .filter(|model| !model.is_empty())
        .expect("the default catalog alias must expose a visible OpenCode route")
        .to_owned();

    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["id"] = id().into();
    agent["name"] = "OpenCode resume fixture".into();
    agent["provider"] = "opencode".into();
    agent["model"] = "opencode/ling-3.1-flash-free".into();
    agent["reasoning"] = "high".into();
    agent["speed"] = "".into();
    agent["access"]["sandbox"] = "yolo".into();
    let agent = s.store.put_agent_fixture(agent).await.unwrap();
    let chat = s
        .chat_create(json!({ "agentId": text(&agent, "id") }))
        .await
        .unwrap();
    let chat_id = text(&chat, "id").to_owned();
    let mut sent = message("fixture:opencode-hang");
    sent["provider"] = "opencode".into();
    sent["model"] = "opencode/ling-3.1-flash-free".into();
    sent["reasoning"] = "high".into();
    let turn_id = text(&sent, "id").to_owned();
    s.chat_send(&chat_id, sent).await.unwrap();
    fixture.start().await;
    let run_id = fixture.chat_run(&chat_id).await;
    let running = await_opencode_resume_setup(
        &fixture,
        &run_id,
        "opencode_catalog_default",
        &plan_received,
        &log_request_seen,
    )
    .await;
    assert_eq!(running["snapshot"]["agent"]["provider"], "opencode");
    assert_eq!(running["snapshot"]["agent"]["access"]["sandbox"], "yolo");

    let session = common::relay_fixture::context(&s).await;
    let client = reqwest::Client::new();
    let relay_token = text(&session, "value").to_owned();
    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", &relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    eprintln!("CAIRN_RESUME_TEST_PHASE test=opencode_catalog_default phase=stopped_resumable");
    let cancelled = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    fixture.until_worker_quiescent().await;
    let checkpoint_before = fixture.checkpoint(&run_id).await;
    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for model in ["not-a-model", "openrouter/unlisted-model"] {
        let (status, response) = api_post(
            &app,
            &relay_token,
            &format!("/api/chats/{chat_id}/resume"),
            json!({
                "runId": run_id,
                "turnId": turn_id,
                "selection": { "model": model, "reasoning": "high", "speed": "" },
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{model}: {response}");
        let unchanged = s.store.run(&run_id).await.unwrap();
        assert_eq!(unchanged["status"], RunStatus::Failed);
        assert_eq!(unchanged["snapshot"], cancelled["snapshot"]);
    }

    common::account_fixture::update_account_fixture(&s, &account_id, &json!({ "enabled": false }))
        .await
        .unwrap();
    let (no_default_status, no_default_response) = api_post(
        &app,
        &relay_token,
        &format!("/api/chats/{chat_id}/resume"),
        json!({
            "runId": run_id,
            "turnId": turn_id,
            "selection": { "model": "", "reasoning": "high", "speed": "" },
        }),
    )
    .await;
    assert_eq!(
        no_default_status,
        StatusCode::BAD_REQUEST,
        "{no_default_response}"
    );
    let unchanged = s.store.run(&run_id).await.unwrap();
    assert_eq!(unchanged["status"], RunStatus::Failed);
    assert_eq!(unchanged["snapshot"], cancelled["snapshot"]);
    common::account_fixture::update_account_fixture(&s, &account_id, &json!({ "enabled": true }))
        .await
        .unwrap();

    let resumed_plan_boundary = controller.plans.lock().await.len();
    let (resume_status, resume_response) = api_post(
        &app,
        &relay_token,
        &format!("/api/chats/{chat_id}/resume"),
        json!({
            "runId": run_id,
            "turnId": turn_id,
            "selection": { "model": "", "reasoning": "high", "speed": "" },
        }),
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK, "{resume_response}");
    let mut expected_snapshot = cancelled["snapshot"].clone();
    expected_snapshot["agent"]["model"] = default_model.into();
    expected_snapshot["agent"]["reasoning"] = "high".into();
    expected_snapshot["agent"]["speed"] = "".into();
    let queued = s.store.run(&run_id).await.unwrap();
    assert_eq!(queued["status"], RunStatus::Queued);
    assert_eq!(queued["snapshot"], expected_snapshot);
    assert_eq!(queued["snapshot"]["agent"]["provider"], "opencode");
    assert_eq!(queued["chatExecution"]["messageId"], turn_id);
    assert_eq!(queued["sessionId"], cancelled["sessionId"]);
    assert_eq!(queued["workspace"], cancelled["workspace"]);

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    eprintln!(
        "CAIRN_RESUME_TEST_PHASE test=opencode_catalog_default phase=selected_turn_completed"
    );
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["snapshot"], expected_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(completed[field], cancelled[field], "{field}");
    }
    assert_eq!(completed["id"], run_id);
    assert_eq!(completed["chatExecution"]["messageId"], turn_id);
    assert_eq!(
        fixture.checkpoint(&run_id).await["generation"],
        checkpoint_before["generation"]
    );
    let selected_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(resumed_plan_boundary)
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["provider"] == "opencode"
                && plan["chat"]["execution"]["messageId"] == turn_id
        })
        .cloned()
        .expect("the catalog-default OpenCode selection must reach a new worker plan");
    assert_eq!(selected_plan["chat"]["model"], expected_plan_model);
    assert_eq!(selected_plan["sandbox"], "yolo");
    assert_eq!(selected_plan["chat"]["sessionId"], cancelled["sessionId"]);
    assert_eq!(selected_plan["chat"]["execution"]["messageId"], turn_id);
    assert_eq!(
        selected_plan["chat"]["execution"]["attachments"],
        cancelled["chatExecution"]["attachments"]
    );
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn opencode_hidden_auto_resume_rejects_omission_and_selection_but_keeps_pinned_route() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut fixture =
        Fixture::with_opencode_bin(2, common::fixture("opencode-hidden-default.mjs")).await;
    fixture.stop(false).await;
    let data_dir = fixture.service.config.data_dir.clone();
    let controller = Arc::new(Controller {
        data: data_dir,
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(true),
    });
    let log_request_seen = Arc::new(AtomicBool::new(false));
    let plan_received = Arc::new(AtomicBool::new(false));
    let runner = Router::new()
        .fallback(any({
            let log_request_seen = log_request_seen.clone();
            let plan_received = plan_received.clone();
            move |state: State<Arc<Controller>>, request: Request| {
                let log_request_seen = log_request_seen.clone();
                let plan_received = plan_received.clone();
                async move {
                    let path = request.uri().path();
                    if path.starts_with("/runs/") && path.ends_with("/logs") {
                        log_request_seen.store(true, Ordering::Relaxed);
                    }
                    let is_plan_post = request.method() == axum::http::Method::POST
                        && path
                            .strip_prefix("/runs/")
                            .is_some_and(|attempt| !attempt.is_empty() && !attempt.contains('/'));
                    let response = serve_controller(state, request).await;
                    if is_plan_post && response.status().is_success() {
                        plan_received.store(true, Ordering::Relaxed);
                    }
                    response
                }
            }
        }))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let s = fixture.service.clone();
    let server = common::serve(listener, runner);
    s.accounts
        .connect_api_key(
            &s,
            Provider::Opencode,
            "OpenCode Zen",
            "opencode",
            "synthetic-hidden-default-resume-key",
            None,
        )
        .await
        .unwrap();

    let catalog = s.opencode.models(&s).await.unwrap();
    let hidden_default = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["model"] == "auto/retired-model")
        .unwrap();
    assert_eq!(hidden_default["isDefault"], true);
    assert_eq!(hidden_default["hidden"], true);
    assert_eq!(
        hidden_default["routes"][0]["model"],
        "opencode/retired-model"
    );
    assert_eq!(hidden_default["routes"][0]["hidden"], true);

    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["id"] = id().into();
    agent["name"] = "OpenCode hidden default resume fixture".into();
    agent["provider"] = "opencode".into();
    agent["model"] = "opencode/ling-3.1-flash-free".into();
    agent["reasoning"] = "high".into();
    agent["speed"] = "".into();
    agent["access"]["sandbox"] = "yolo".into();
    let agent = s.store.put_agent_fixture(agent).await.unwrap();
    let chat = s
        .chat_create(json!({ "agentId": text(&agent, "id") }))
        .await
        .unwrap();
    let chat_id = text(&chat, "id").to_owned();
    let mut sent = message("fixture:opencode-hang");
    sent["provider"] = "opencode".into();
    sent["model"] = "opencode/ling-3.1-flash-free".into();
    sent["reasoning"] = "high".into();
    let turn_id = text(&sent, "id").to_owned();
    s.chat_send(&chat_id, sent).await.unwrap();
    fixture.start().await;
    let run_id = fixture.chat_run(&chat_id).await;
    let running = await_opencode_resume_setup(
        &fixture,
        &run_id,
        "opencode_hidden_auto",
        &plan_received,
        &log_request_seen,
    )
    .await;
    assert_eq!(
        running["snapshot"]["agent"]["model"],
        "opencode/ling-3.1-flash-free"
    );

    let relay = common::relay_fixture::context(&s).await;
    let relay_token = text(&relay, "value").to_owned();
    let client = reqwest::Client::new();
    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", &relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    eprintln!("CAIRN_RESUME_TEST_PHASE test=opencode_hidden_auto phase=stopped_resumable");
    let cancelled = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    fixture.until_worker_quiescent().await;
    let checkpoint_before = fixture.checkpoint(&run_id).await;
    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // The run began on a pinned route; model the saved Auto parent after all of its
    // current provider routes have since become deprecated, without changing the
    // preserved turn, session, workspace or checkpoint.
    let mut hidden_snapshot = cancelled["snapshot"].clone();
    hidden_snapshot["agent"]["model"] = "auto/retired-model".into();
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                snapshot: Some(serde_json::from_value(hidden_snapshot).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let before_rejection = s.store.run(&run_id).await.unwrap();
    let attempts_before = s
        .store
        .attempts_fixture()
        .await
        .unwrap()
        .into_iter()
        .filter(|attempt| attempt["runId"] == run_id)
        .count();
    let plans_before = controller
        .plans
        .lock()
        .await
        .iter()
        .filter(|plan| plan["runId"] == run_id)
        .count();
    assert_eq!(before_rejection["status"], RunStatus::Failed);
    assert_eq!(before_rejection["resumeAvailable"], true);
    assert_eq!(
        before_rejection["snapshot"]["agent"]["model"],
        "auto/retired-model"
    );

    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    for (label, path, body) in [
        (
            "omitted run resume",
            format!("/api/runs/{run_id}/resume"),
            json!({ "turnId": turn_id }),
        ),
        (
            "omitted chat resume",
            format!("/api/chats/{chat_id}/resume"),
            json!({ "runId": run_id, "turnId": turn_id }),
        ),
    ] {
        let (status, response) = api_post(&app, &relay_token, &path, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {response}");
        assert_eq!(
            s.store.run(&run_id).await.unwrap(),
            before_rejection,
            "{label}"
        );
        assert_eq!(
            fixture.checkpoint(&run_id).await,
            checkpoint_before,
            "{label}"
        );
        assert_eq!(
            s.store
                .attempts_fixture()
                .await
                .unwrap()
                .into_iter()
                .filter(|attempt| attempt["runId"] == run_id)
                .count(),
            attempts_before,
            "{label} must not create an attempt",
        );
        assert_eq!(
            controller
                .plans
                .lock()
                .await
                .iter()
                .filter(|plan| plan["runId"] == run_id)
                .count(),
            plans_before,
            "{label} must not reach the worker",
        );
    }

    for (label, model) in [
        ("hidden default", ""),
        ("explicit hidden Auto", "auto/retired-model"),
    ] {
        let (status, response) = api_post(
            &app,
            &relay_token,
            &format!("/api/chats/{chat_id}/resume"),
            json!({
                "runId": run_id,
                "turnId": turn_id,
                "selection": { "model": model, "reasoning": "", "speed": "" },
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {response}");
        assert_eq!(
            s.store.run(&run_id).await.unwrap(),
            before_rejection,
            "{label}"
        );
        assert_eq!(
            fixture.checkpoint(&run_id).await,
            checkpoint_before,
            "{label}"
        );
        assert_eq!(
            s.store
                .attempts_fixture()
                .await
                .unwrap()
                .into_iter()
                .filter(|attempt| attempt["runId"] == run_id)
                .count(),
            attempts_before,
            "{label} must not create an attempt",
        );
        assert_eq!(
            controller
                .plans
                .lock()
                .await
                .iter()
                .filter(|plan| plan["runId"] == run_id)
                .count(),
            plans_before,
            "{label} must not reach the worker",
        );
        assert_eq!(before_rejection["chatExecution"]["messageId"], turn_id);
        assert_eq!(before_rejection["sessionId"], cancelled["sessionId"]);
        assert_eq!(before_rejection["workspace"], cancelled["workspace"]);
    }

    let (status, response) = api_post(
        &app,
        &relay_token,
        &format!("/api/chats/{chat_id}/resume"),
        json!({
            "runId": run_id,
            "turnId": turn_id,
            "selection": {
                "model": "opencode/retired-model",
                "reasoning": "",
                "speed": "",
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let queued = s.store.run(&run_id).await.unwrap();
    let mut expected_snapshot = before_rejection["snapshot"].clone();
    expected_snapshot["agent"]["model"] = "opencode/retired-model".into();
    expected_snapshot["agent"]["reasoning"] = "".into();
    expected_snapshot["agent"]["speed"] = "".into();
    assert_eq!(queued["status"], RunStatus::Queued);
    assert_eq!(queued["snapshot"], expected_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(queued[field], cancelled[field], "{field}");
    }

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    eprintln!("CAIRN_RESUME_TEST_PHASE test=opencode_hidden_auto phase=pinned_turn_completed");
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(completed["snapshot"], expected_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(completed[field], cancelled[field], "{field}");
    }
    assert_eq!(
        fixture.checkpoint(&run_id).await["generation"],
        checkpoint_before["generation"]
    );
    let pinned_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["provider"] == "opencode"
                && plan["chat"]["model"] == "opencode/retired-model"
                && plan["chat"]["execution"]["messageId"] == turn_id
        })
        .cloned()
        .expect("a deprecated provider-qualified route stays pinned in the worker plan");
    assert_eq!(pinned_plan["sandbox"], "yolo");
    assert_eq!(pinned_plan["chat"]["sessionId"], cancelled["sessionId"]);
    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn chat_resume_selection_keeps_saved_identity_and_reaches_the_worker_plan() {
    use std::sync::atomic::AtomicBool;

    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let data_dir = fixture.service.config.data_dir.clone();
    let controller = Arc::new(Controller {
        data: data_dir,
        plans: tokio::sync::Mutex::new(Vec::new()),
        failures: 0,
        slots: 4,
        hold: AtomicBool::new(true),
    });
    let app = Router::new()
        .fallback(any(serve_controller))
        .with_state(controller.clone());
    let (listener, address) = common::bind().await;
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let s = fixture.service.clone();
    let server = common::serve(listener, app);
    let home = s.config.home.join(".codex");
    tokio::fs::create_dir_all(&home).await.unwrap();
    tokio::fs::write(home.join("auth.json"), "{}")
        .await
        .unwrap();
    cairn_installation::nodes::refresh_local(&s).await.unwrap();
    s.store
        .set(
            "codex-models:",
            json!({
                "checkedAt": now(),
                "models": [
                    {
                        "model": "gpt-6-sol",
                        "isDefault": true,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                    {
                        "model": "gpt-6-luna",
                        "isDefault": false,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "xhigh" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                    {
                        "model": "my-alias",
                        "isDefault": false,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                ],
            }),
            None,
        )
        .await
        .unwrap();

    let chat = s.chat_create(json!({})).await.unwrap();
    let chat_id = text(&chat, "id");
    let attachment_id = id();
    let upload = axum::http::Request::builder()
        .method("PUT")
        .uri("/?name=design.png")
        .body(Body::from(PNG))
        .unwrap();
    s.attachment_http(chat_id, &attachment_id, upload)
        .await
        .unwrap();
    let mut sent = message("fixture:chat-hang");
    sent["provider"] = "codex".into();
    sent["model"] = "gpt-6-sol".into();
    sent["reasoning"] = "high".into();
    sent["speed"] = "priority".into();
    sent["attachmentIds"] = json!([attachment_id]);
    s.chat_send(chat_id, sent).await.unwrap();
    fixture.start().await;
    let run_id = fixture.chat_run(chat_id).await;
    let active = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            controller
                .plans
                .lock()
                .await
                .iter()
                .any(|plan| plan["runId"] == run_id)
                .then_some(())
        },
    )
    .await;

    let session = common::relay_fixture::context(&s).await;
    let client = reqwest::Client::new();
    let resume = |body: Value| {
        client
            .post(format!("{}/api/chats/{chat_id}/resume", fixture.url))
            .header("x-test-relay-token", text(&session, "value"))
            .json(&body)
    };
    let turn_id = text(&active["chatExecution"], "messageId").to_owned();
    let override_selection = json!({
        "model": "gpt-6-luna",
        "reasoning": "xhigh",
        "speed": "",
    });

    let active_refusal = resume(json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": override_selection,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(active_refusal.status(), StatusCode::CONFLICT);
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["snapshot"],
        active["snapshot"]
    );

    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let before = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    fixture.until_worker_quiescent().await;
    let checkpoint_before = fixture.checkpoint(&run_id).await;
    assert!(checkpoint_before["prepared"].is_object());
    let checkpoint_generation_before = checkpoint_before.get("generation").cloned();

    let stale_run = resume(json!({
        "runId": id(),
        "turnId": turn_id,
        "selection": override_selection,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(stale_run.status(), StatusCode::CONFLICT);
    let stale_turn = resume(json!({
        "runId": run_id,
        "turnId": "stale-turn",
        "selection": override_selection,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(stale_turn.status(), StatusCode::CONFLICT);
    for body in [
        json!({ "selection": override_selection }),
        json!({ "runId": run_id, "selection": override_selection }),
        json!({ "turnId": turn_id, "selection": override_selection }),
    ] {
        let missing_guard = resume(body).send().await.unwrap();
        assert_eq!(missing_guard.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["snapshot"],
        before["snapshot"]
    );
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["status"],
        RunStatus::Cancelled
    );
    let unknown_model = resume(json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": {
            "model": "gpt-6-not-in-catalog",
            "reasoning": "xhigh",
            "speed": "",
        },
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(unknown_model.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["snapshot"],
        before["snapshot"]
    );
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["status"],
        RunStatus::Cancelled
    );
    let invalid_default_speed = resume(json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": {
            "model": "gpt-6-luna",
            "reasoning": "xhigh",
            "speed": "default",
        },
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(invalid_default_speed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["snapshot"],
        before["snapshot"]
    );
    assert_eq!(
        s.store.run(&run_id).await.unwrap()["status"],
        RunStatus::Cancelled
    );

    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                cancel_requested_at: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let failed_quota_run = s.store.run(&run_id).await.unwrap();
    assert_eq!(failed_quota_run["status"], RunStatus::Failed);
    assert_eq!(failed_quota_run["cancelRequestedAt"], Value::Null);
    assert!(
        failed_quota_run["error"]
            .as_str()
            .unwrap()
            .contains("Usage limit")
    );
    let mut alternative = serde_json::to_value(
        s.accounts
            .create(&s, Provider::Claude, "Switch target")
            .await
            .unwrap(),
    )
    .unwrap();
    let alternative_id = text(&alternative, "id").to_owned();
    alternative["state"] = "ready".into();
    s.store.save_account_fixture(alternative).await.unwrap();
    let claude_home = accounts::claude::account_home(&s.config, &alternative_id);
    std::fs::create_dir_all(&claude_home).unwrap();
    std::fs::write(
        claude_home.join(".credentials.json"),
        json!({
            "claudeAiOauth": {
                "accessToken": "fixture-access",
                "expiresAt": now() + 3_600_000,
                "scopes": ["user:inference"],
            },
        })
        .to_string(),
    )
    .unwrap();

    let app = common::relay_fixture::router(s.clone()).await.unwrap();
    let relay_token = text(&session, "value");
    let switch_id = id();
    let switch = json!({
        "runId": run_id,
        "turnId": turn_id,
        "requestId": switch_id,
        "provider": "claude",
    });
    let (switch_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        switch.clone(),
    )
    .await;
    assert_eq!(switch_status, StatusCode::OK);
    let staged = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(staged.request_id, switch_id);
    assert!(staged.pending);

    let (switch_retry_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        switch.clone(),
    )
    .await;
    assert_eq!(switch_retry_status, StatusCode::OK);
    let before_legacy_resume = s.store.run(&run_id).await.unwrap();
    let checkpoint_before_legacy_resume = fixture.checkpoint(&run_id).await;
    let (legacy_resume_status, legacy_resume_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/resume"),
        json!({ "runId": run_id, "turnId": turn_id }),
    )
    .await;
    assert_eq!(
        legacy_resume_status,
        StatusCode::CONFLICT,
        "{legacy_resume_response}"
    );
    assert_eq!(s.store.run(&run_id).await.unwrap(), before_legacy_resume);
    assert_eq!(
        fixture.checkpoint(&run_id).await,
        checkpoint_before_legacy_resume
    );
    let preserved_switch = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&preserved_switch).unwrap(),
        serde_json::to_value(&staged).unwrap()
    );
    assert_eq!(preserved_switch.request_id, switch_id);
    assert_eq!(preserved_switch.provider, "claude");
    assert!(preserved_switch.pending);

    let resume_body = json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": override_selection,
    });
    let (resume_status, resume_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/resume"),
        resume_body,
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK, "{resume_response}");
    let queued_resume = s.store.run(&run_id).await.unwrap();
    assert_eq!(queued_resume["status"], RunStatus::Queued);
    assert_eq!(queued_resume["snapshot"]["agent"]["provider"], "codex");
    assert_eq!(queued_resume["snapshot"]["agent"]["speed"], "priority");
    assert_eq!(queued_resume["chatExecution"]["messageId"], turn_id);
    let cancelled_switch = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled_switch.request_id, switch_id);
    assert!(!cancelled_switch.pending);

    let late_switch = json!({
        "runId": run_id,
        "turnId": turn_id,
        "requestId": id(),
        "provider": "claude",
    });
    let (late_switch_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        late_switch,
    )
    .await;
    assert_eq!(late_switch_status, StatusCode::CONFLICT);

    let (completed_switch_retry_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        switch,
    )
    .await;
    assert_eq!(completed_switch_retry_status, StatusCode::OK);
    assert!(
        !s.store
            .read({
                let chat_id = chat_id.to_owned();
                move |db| Ok(db.engine_switch(&chat_id)?.unwrap().pending)
            })
            .await
            .unwrap()
    );

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    let resume = |body: Value| {
        client
            .post(format!("{}/api/chats/{chat_id}/resume", fixture.url))
            .header("x-test-relay-token", text(&session, "value"))
            .json(&body)
    };
    let completed = fixture.until_finished(&run_id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    let mut expected_snapshot = before["snapshot"].clone();
    expected_snapshot["agent"]["model"] = "gpt-6-luna".into();
    expected_snapshot["agent"]["reasoning"] = "xhigh".into();
    expected_snapshot["agent"]["speed"] = "priority".into();
    assert_eq!(completed["snapshot"], expected_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(completed[field], before[field], "{field}");
    }
    assert_eq!(completed["chatExecution"]["messageId"], turn_id);
    let checkpoint_after = fixture.checkpoint(&run_id).await;
    assert_eq!(
        checkpoint_after.get("generation").cloned(),
        checkpoint_generation_before
    );
    let selected_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["model"] == "gpt-6-luna"
                && plan["chat"]["reasoning"] == "xhigh"
        })
        .cloned()
        .expect("the resumed worker plan must contain the selected model and reasoning");
    assert_eq!(selected_plan["chat"]["execution"]["messageId"], turn_id);
    assert_eq!(selected_plan["chat"]["serviceTier"], "priority");
    assert_eq!(selected_plan["chat"]["sessionId"], before["sessionId"]);
    assert_eq!(
        selected_plan["chat"]["execution"]["attachments"],
        before["chatExecution"]["attachments"]
    );

    let unpaused = client
        .post(format!("{}/api/chats/{chat_id}/pause", fixture.url))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({ "paused": false }))
        .send()
        .await
        .unwrap();
    unpaused.error_for_status().unwrap();
    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let mut later_message = message("fixture:chat-hang");
    later_message["provider"] = "codex".into();
    later_message["model"] = "gpt-6-luna".into();
    later_message["reasoning"] = "xhigh".into();
    later_message["speed"] = "priority".into();
    let later_turn_id = text(&later_message, "id").to_owned();
    s.chat_send(chat_id, later_message).await.unwrap();
    let later_turn = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Running
                && run["chatExecution"]["messageId"] == later_turn_id
        })
        .await;
    assert_eq!(later_turn["id"], run_id);
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            controller
                .plans
                .lock()
                .await
                .iter()
                .any(|plan| {
                    plan["runId"] == run_id
                        && plan["chat"]["execution"]["messageId"] == later_turn_id
                })
                .then_some(())
        },
    )
    .await;
    let stopped = client
        .post(format!("{}/api/chats/{chat_id}/stop", fixture.url))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let later_turn_stopped = fixture
        .until(&run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    fixture.until_worker_quiescent().await;
    assert_eq!(
        later_turn_stopped["chatExecution"]["messageId"],
        later_turn_id
    );
    let stale_existing_turn = resume(json!({
        "runId": run_id,
        "turnId": turn_id,
        "selection": override_selection,
    }))
    .send()
    .await
    .unwrap();
    assert_eq!(stale_existing_turn.status(), StatusCode::CONFLICT);
    let after_stale_existing_turn = s.store.run(&run_id).await.unwrap();
    assert_eq!(after_stale_existing_turn["status"], RunStatus::Cancelled);
    assert_eq!(after_stale_existing_turn["snapshot"], expected_snapshot);
    assert_eq!(
        after_stale_existing_turn["chatExecution"]["messageId"],
        later_turn_id
    );

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let standard_plan_boundary = controller.plans.lock().await.len();
    let (standard_resume, standard_snapshot) = (
        resume(json!({
            "runId": run_id,
            "turnId": later_turn_id,
            "selection": {
                "model": "gpt-6-luna",
                "reasoning": "xhigh",
                "speed": "standard",
            },
        }))
        .send()
        .await
        .unwrap(),
        {
            let mut snapshot = expected_snapshot.clone();
            snapshot["agent"]["speed"] = "standard".into();
            snapshot
        },
    );
    assert!(standard_resume.status().is_success());
    let standard_completed = fixture.until_finished(&run_id).await;
    assert_eq!(standard_completed["status"], RunStatus::Succeeded);
    assert_eq!(standard_completed["snapshot"], standard_snapshot);
    for field in ["id", "taskId", "workspace", "sessionId"] {
        assert_eq!(
            standard_completed[field], later_turn_stopped[field],
            "{field}"
        );
    }
    assert_eq!(
        standard_completed["chatExecution"]["messageId"],
        later_turn_id
    );
    let standard_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(standard_plan_boundary)
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["execution"]["messageId"] == later_turn_id
                && plan["chat"]["model"] == "gpt-6-luna"
        })
        .cloned()
        .expect("the explicit Standard request must reach the resumed worker plan");
    assert_eq!(standard_plan["chat"]["serviceTier"], "default");

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);

    fixture.stop(false).await;
    s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let normal_switch_id = id();
    let normal_switch = json!({
        "runId": run_id,
        "turnId": later_turn_id,
        "requestId": normal_switch_id,
        "provider": "claude",
    });
    let (normal_stage_status, normal_stage_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        normal_switch.clone(),
    )
    .await;
    assert_eq!(
        normal_stage_status,
        StatusCode::OK,
        "{normal_stage_response}"
    );
    let (normal_retry_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        normal_switch.clone(),
    )
    .await;
    assert_eq!(normal_retry_status, StatusCode::OK);
    let normal_staged = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(normal_staged.request_id, normal_switch_id);
    assert!(normal_staged.pending);

    fixture.start().await;
    let switched = fixture
        .until(&run_id, |run| {
            run["snapshot"]["agent"]["provider"] == "claude"
        })
        .await;
    assert_eq!(switched["id"], run_id);
    assert_eq!(switched["chatExecution"]["messageId"], later_turn_id);
    let completed_switch = fixture.until_finished(&run_id).await;
    assert_eq!(completed_switch["snapshot"]["agent"]["provider"], "claude");
    let normal_receipt = s
        .store
        .read({
            let chat_id = chat_id.to_owned();
            move |db| db.engine_switch(&chat_id)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(normal_receipt.request_id, normal_switch_id);
    assert_eq!(normal_receipt.provider, "claude");
    assert!(!normal_receipt.pending);
    let claude_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .find(|plan| {
            plan["runId"] == run_id
                && plan["chat"]["execution"]["messageId"] == later_turn_id
                && plan["chat"]["provider"] == "claude"
        })
        .cloned()
        .expect("the normal pending switch must reach the Claude worker plan");
    assert_eq!(claude_plan["chat"]["provider"], "claude");
    let plans_before_receipt_retry = controller.plans.lock().await.len();
    let (normal_receipt_retry_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{chat_id}/coding-agent"),
        normal_switch,
    )
    .await;
    assert_eq!(normal_receipt_retry_status, StatusCode::OK);
    assert_eq!(
        controller.plans.lock().await.len(),
        plans_before_receipt_retry
    );

    let compatibility_chat = s.chat_create(json!({})).await.unwrap();
    let compatibility_chat_id = text(&compatibility_chat, "id");
    let mut compatibility_message = message("fixture:chat-hang");
    compatibility_message["provider"] = "codex".into();
    compatibility_message["model"] = "gpt-6-sol".into();
    compatibility_message["reasoning"] = "high".into();
    s.chat_send(compatibility_chat_id, compatibility_message)
        .await
        .unwrap();
    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let compatibility_run_id = fixture.chat_run(compatibility_chat_id).await;
    fixture
        .until(&compatibility_run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    let stopped = client
        .post(format!(
            "{}/api/chats/{compatibility_chat_id}/stop",
            fixture.url
        ))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let compatibility_before = fixture
        .until(&compatibility_run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let active_runs_before_resume = fixture.until_worker_quiescent().await;
    let compatible = client
        .post(format!(
            "{}/api/chats/{compatibility_chat_id}/resume",
            fixture.url
        ))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let compatible_status = compatible.status();
    assert!(
        compatible_status.is_success(),
        "resume returned HTTP {}; active_conflict_before_resume={}",
        compatible_status.as_u16(),
        active_runs_before_resume > 0
    );
    let compatibility_completed = fixture.until_finished(&compatibility_run_id).await;
    assert_eq!(
        compatibility_completed["snapshot"],
        compatibility_before["snapshot"]
    );
    let compatibility_plans = controller
        .plans
        .lock()
        .await
        .iter()
        .filter(|plan| plan["runId"] == compatibility_run_id)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(compatibility_plans.len(), 2);
    assert!(
        compatibility_plans
            .iter()
            .all(|plan| plan["chat"]["model"] == "gpt-6-sol")
    );

    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let default_model_chat = s.chat_create(json!({})).await.unwrap();
    let default_model_chat_id = text(&default_model_chat, "id").to_owned();
    let mut default_model_message = message("fixture:chat-hang");
    default_model_message["provider"] = "codex".into();
    default_model_message["model"] = "".into();
    default_model_message["reasoning"] = "high".into();
    default_model_message["speed"] = "priority".into();
    let default_model_turn_id = text(&default_model_message, "id").to_owned();
    s.chat_send(&default_model_chat_id, default_model_message)
        .await
        .unwrap();
    let default_model_run_id = fixture.chat_run(&default_model_chat_id).await;
    let default_model_running = fixture
        .until(&default_model_run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    assert_eq!(default_model_running["snapshot"]["agent"]["model"], "");
    let stopped = client
        .post(format!(
            "{}/api/chats/{default_model_chat_id}/stop",
            fixture.url
        ))
        .header("x-test-relay-token", text(&session, "value"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let default_model_before = fixture
        .until(&default_model_run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    fixture.until_worker_quiescent().await;
    assert_eq!(
        default_model_before["sessionId"],
        default_model_running["sessionId"]
    );
    fixture.stop(false).await;
    s.store
        .change_run(
            &default_model_run_id,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Failed),
                finished_at: Some(Some(now())),
                error: Some(Some("Usage limit has been reached".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let default_plan_count_before_resume = controller.plans.lock().await.len();
    let (default_resume_status, default_resume_response) = api_post(
        &app,
        text(&session, "value"),
        &format!("/api/chats/{default_model_chat_id}/resume"),
        json!({
            "runId": default_model_run_id,
            "turnId": default_model_turn_id,
            "selection": {
                "model": "",
                "reasoning": "high",
                "speed": "standard",
            },
        }),
    )
    .await;
    assert_eq!(
        default_resume_status,
        StatusCode::OK,
        "{default_resume_response}"
    );
    let default_resume_queued = s.store.run(&default_model_run_id).await.unwrap();
    assert_eq!(default_resume_queued["status"], RunStatus::Queued);
    assert_eq!(
        default_resume_queued["snapshot"]["agent"]["model"],
        "gpt-6-sol"
    );
    assert_eq!(
        default_resume_queued["snapshot"]["agent"]["reasoning"],
        "high"
    );
    assert_eq!(
        default_resume_queued["snapshot"]["agent"]["speed"],
        "standard"
    );
    assert_eq!(
        default_resume_queued["chatExecution"]["messageId"],
        default_model_turn_id
    );
    assert_eq!(
        default_resume_queued["sessionId"],
        default_model_before["sessionId"]
    );

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    let default_model_completed = fixture.until_finished(&default_model_run_id).await;
    assert_eq!(default_model_completed["status"], RunStatus::Succeeded);
    assert_eq!(
        default_model_completed["snapshot"]["agent"]["model"],
        "gpt-6-sol"
    );
    assert_eq!(
        default_model_completed["chatExecution"]["messageId"],
        default_model_turn_id
    );
    assert_eq!(
        default_model_completed["sessionId"],
        default_model_before["sessionId"]
    );
    let default_model_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(default_plan_count_before_resume)
        .find(|plan| {
            plan["runId"] == default_model_run_id
                && plan["chat"]["execution"]["messageId"] == default_model_turn_id
        })
        .cloned()
        .expect("the resolved default model must reach the resumed worker plan");
    assert_eq!(default_model_plan["chat"]["provider"], "codex");
    assert_eq!(default_model_plan["chat"]["model"], "gpt-6-sol");
    assert_eq!(default_model_plan["chat"]["reasoning"], "high");
    assert_eq!(default_model_plan["chat"]["serviceTier"], "default");

    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let preserved_model_chat = s.chat_create(json!({})).await.unwrap();
    let preserved_model_chat_id = text(&preserved_model_chat, "id").to_owned();
    let mut preserved_model_message = message("fixture:chat-hang");
    preserved_model_message["provider"] = "codex".into();
    preserved_model_message["model"] = "".into();
    preserved_model_message["reasoning"] = "".into();
    preserved_model_message["speed"] = "priority".into();
    let preserved_model_turn_id = text(&preserved_model_message, "id").to_owned();
    s.chat_send(&preserved_model_chat_id, preserved_model_message)
        .await
        .unwrap();
    let preserved_model_run_id = fixture.chat_run(&preserved_model_chat_id).await;
    let preserved_model_running = fixture
        .until(&preserved_model_run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    assert_eq!(preserved_model_running["snapshot"]["agent"]["model"], "");
    let stopped = client
        .post(format!(
            "{}/api/chats/{preserved_model_chat_id}/stop",
            fixture.url
        ))
        .header("x-test-relay-token", relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let preserved_model_before = fixture
        .until(&preserved_model_run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    fixture.until_worker_quiescent().await;
    let preserved_checkpoint_before = fixture.checkpoint(&preserved_model_run_id).await;
    let preserved_generation_before = preserved_checkpoint_before.get("generation").cloned();
    let preserved_plan_count_before_resume = controller.plans.lock().await.len();
    let (preserve_status, preserve_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{preserved_model_chat_id}/resume"),
        json!({
            "runId": preserved_model_run_id,
            "turnId": preserved_model_turn_id,
            "selection": {
                "model": "",
                "preserveModel": true,
                "reasoning": "high",
                "speed": "standard",
            },
        }),
    )
    .await;
    assert_eq!(preserve_status, StatusCode::OK, "{preserve_response}");
    let preserved_queued = s.store.run(&preserved_model_run_id).await.unwrap();
    assert_eq!(preserved_queued["status"], RunStatus::Queued);
    assert_eq!(preserved_queued["snapshot"]["agent"]["model"], "");
    assert_eq!(preserved_queued["snapshot"]["agent"]["reasoning"], "high");
    assert_eq!(preserved_queued["snapshot"]["agent"]["speed"], "standard");
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(
            preserved_queued[field], preserved_model_before[field],
            "{field}"
        );
    }
    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let preserved_completed = fixture.until_finished(&preserved_model_run_id).await;
    assert_eq!(preserved_completed["status"], RunStatus::Succeeded);
    assert_eq!(preserved_completed["snapshot"]["agent"]["model"], "");
    assert_eq!(
        preserved_completed["snapshot"]["agent"]["reasoning"],
        "high"
    );
    assert_eq!(
        preserved_completed["snapshot"]["agent"]["speed"],
        "standard"
    );
    assert_eq!(
        fixture
            .checkpoint(&preserved_model_run_id)
            .await
            .get("generation")
            .cloned(),
        preserved_generation_before
    );
    let preserved_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(preserved_plan_count_before_resume)
        .find(|plan| {
            plan["runId"] == preserved_model_run_id
                && plan["chat"]["execution"]["messageId"] == preserved_model_turn_id
        })
        .cloned()
        .expect(
            "the preserved blank model and explicit Standard must reach the resumed worker plan",
        );
    assert_eq!(preserved_plan["chat"]["model"], "");
    assert_eq!(preserved_plan["chat"]["reasoning"], "high");
    assert_eq!(preserved_plan["chat"]["serviceTier"], "default");
    assert_eq!(
        preserved_plan["chat"]["sessionId"],
        preserved_model_before["sessionId"]
    );
    assert_eq!(
        preserved_plan["chat"]["execution"]["attachments"],
        preserved_model_before["chatExecution"]["attachments"]
    );

    controller
        .hold
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let alias_chat = s.chat_create(json!({})).await.unwrap();
    let alias_chat_id = text(&alias_chat, "id").to_owned();
    let mut alias_message = message("fixture:chat-hang");
    alias_message["provider"] = "codex".into();
    alias_message["model"] = "my-alias".into();
    alias_message["reasoning"] = "high".into();
    alias_message["speed"] = "priority".into();
    let alias_turn_id = text(&alias_message, "id").to_owned();
    let alias_initial_plan_boundary = controller.plans.lock().await.len();
    s.chat_send(&alias_chat_id, alias_message).await.unwrap();
    let alias_run_id = fixture.chat_run(&alias_chat_id).await;
    let alias_running = fixture
        .until(&alias_run_id, |run| {
            run["status"] == RunStatus::Running && run["sessionId"].is_string()
        })
        .await;
    assert_eq!(alias_running["snapshot"]["agent"]["model"], "my-alias");
    assert_eq!(alias_running["snapshot"]["agent"]["reasoning"], "high");
    assert_eq!(alias_running["snapshot"]["agent"]["speed"], "priority");
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            controller
                .plans
                .lock()
                .await
                .iter()
                .any(|plan| {
                    plan["runId"] == alias_run_id
                        && plan["chat"]["execution"]["messageId"] == alias_turn_id
                })
                .then_some(())
        },
    )
    .await;
    let alias_initial_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(alias_initial_plan_boundary)
        .find(|plan| {
            plan["runId"] == alias_run_id && plan["chat"]["execution"]["messageId"] == alias_turn_id
        })
        .cloned()
        .expect("the current catalog must route the saved alias before it is removed");
    assert_eq!(alias_initial_plan["chat"]["model"], "my-alias");
    assert_eq!(alias_initial_plan["chat"]["reasoning"], "high");
    assert_eq!(alias_initial_plan["chat"]["serviceTier"], "priority");
    let stopped = client
        .post(format!("{}/api/chats/{alias_chat_id}/stop", fixture.url))
        .header("x-test-relay-token", relay_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    stopped.error_for_status().unwrap();
    let alias_before = fixture
        .until(&alias_run_id, |run| {
            run["status"] == RunStatus::Cancelled && run["resumeAvailable"] == true
        })
        .await;
    fixture.until_worker_quiescent().await;
    let alias_checkpoint_before = fixture.checkpoint(&alias_run_id).await;
    let alias_generation_before = alias_checkpoint_before.get("generation").cloned();

    fixture.stop(false).await;
    s.store
        .set(
            "codex-models:",
            json!({
                "checkedAt": now(),
                "models": [
                    {
                        "model": "gpt-6-sol",
                        "isDefault": true,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "high" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                    {
                        "model": "gpt-6-luna",
                        "isDefault": false,
                        "supportedReasoningEfforts": [{ "reasoningEffort": "xhigh" }],
                        "serviceTiers": [{ "id": "priority" }],
                    },
                ],
            }),
            None,
        )
        .await
        .unwrap();

    let changed_alias = json!({
        "runId": alias_run_id,
        "turnId": alias_turn_id,
        "selection": {
            "model": "new-alias",
            "reasoning": "high",
            "speed": "standard",
        },
    });
    let (changed_alias_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{alias_chat_id}/resume"),
        changed_alias,
    )
    .await;
    assert_eq!(changed_alias_status, StatusCode::BAD_REQUEST);
    let changed_alias_reasoning = json!({
        "runId": alias_run_id,
        "turnId": alias_turn_id,
        "selection": {
            "model": "my-alias",
            "reasoning": "xhigh",
            "speed": "standard",
        },
    });
    let (changed_reasoning_status, _) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{alias_chat_id}/resume"),
        changed_alias_reasoning,
    )
    .await;
    assert_eq!(changed_reasoning_status, StatusCode::BAD_REQUEST);
    assert_eq!(s.store.run(&alias_run_id).await.unwrap(), alias_before);
    assert_eq!(
        fixture.checkpoint(&alias_run_id).await,
        alias_checkpoint_before
    );

    let alias_plan_boundary = controller.plans.lock().await.len();
    let (alias_resume_status, alias_resume_response) = api_post(
        &app,
        relay_token,
        &format!("/api/chats/{alias_chat_id}/resume"),
        json!({
            "runId": alias_run_id,
            "turnId": alias_turn_id,
            "selection": {
                "model": "my-alias",
                "reasoning": "high",
                "speed": "standard",
            },
        }),
    )
    .await;
    assert_eq!(
        alias_resume_status,
        StatusCode::OK,
        "{alias_resume_response}"
    );
    let alias_queued = s.store.run(&alias_run_id).await.unwrap();
    assert_eq!(alias_queued["status"], RunStatus::Queued);
    assert_eq!(alias_queued["snapshot"]["agent"]["model"], "my-alias");
    assert_eq!(alias_queued["snapshot"]["agent"]["reasoning"], "high");
    assert_eq!(alias_queued["snapshot"]["agent"]["speed"], "standard");
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(alias_queued[field], alias_before[field], "{field}");
    }
    assert_eq!(alias_queued["chatExecution"]["messageId"], alias_turn_id);

    controller
        .hold
        .store(false, std::sync::atomic::Ordering::Relaxed);
    fixture.start().await;
    let alias_completed = fixture.until_finished(&alias_run_id).await;
    assert_eq!(alias_completed["status"], RunStatus::Succeeded);
    assert_eq!(alias_completed["snapshot"]["agent"]["model"], "my-alias");
    assert_eq!(alias_completed["snapshot"]["agent"]["reasoning"], "high");
    assert_eq!(alias_completed["snapshot"]["agent"]["speed"], "standard");
    assert_eq!(alias_completed["chatExecution"]["messageId"], alias_turn_id);
    for field in ["id", "taskId", "workspace", "sessionId", "chatExecution"] {
        assert_eq!(alias_completed[field], alias_before[field], "{field}");
    }
    assert_eq!(
        fixture
            .checkpoint(&alias_run_id)
            .await
            .get("generation")
            .cloned(),
        alias_generation_before
    );
    let alias_plan = controller
        .plans
        .lock()
        .await
        .iter()
        .skip(alias_plan_boundary)
        .find(|plan| {
            plan["runId"] == alias_run_id
                && plan["chat"]["execution"]["messageId"] == alias_turn_id
                && plan["chat"]["model"] == "my-alias"
        })
        .cloned()
        .expect("the retained saved Codex alias must reach the resumed worker plan");
    assert_eq!(alias_plan["chat"]["reasoning"], "high");
    assert_eq!(alias_plan["chat"]["serviceTier"], "default");
    assert_eq!(alias_plan["chat"]["sessionId"], alias_before["sessionId"]);

    fixture.stop(false).await;
    server.abort();
}

#[tokio::test]
async fn unlimited_runs_cancel_and_resume_without_creating_a_deadline() {
    let mut fixture = Fixture::new().await;
    let s = fixture.service.clone();
    let run = fixture.enqueue("fixture:restart").await;
    let id = text(&run, "id");
    assert_eq!(run["snapshot"]["agent"]["timeoutMinutes"], 0);
    fixture.until_restart_ready(id).await;
    assert_eq!(s.store.run(id).await.unwrap()["status"], RunStatus::Running);
    assert_eq!(
        fixture.checkpoint(id).await.get("remainingMs"),
        Some(&Value::Null)
    );
    let session = common::relay_fixture::context(&s).await;
    let client = reqwest::Client::new();
    let command = |action: &str| {
        client
            .post(format!("{}/api/runs/{id}/{action}", fixture.url))
            .header("x-test-relay-token", text(&session, "value"))
            .json(&json!({}))
    };
    command("cancel")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture
        .until(id, |r| r["status"] == RunStatus::Cancelled)
        .await;
    // The worker records the final status event right after settling the run.
    let events = eventually(
        Duration::from_secs(5),
        Duration::from_millis(30),
        async || {
            let id = id.to_owned();
            let events = s
                .store
                .read(move |db| db.events_fixture(&id, 0, 1000))
                .await
                .unwrap();
            events
                .iter()
                .any(|event| event["type"] == "status" && event["text"] == "cancelled")
                .then_some(events)
        },
    )
    .await;
    assert!(
        events.iter().all(|event| event["type"] != "turn.failed"),
        "A stop Cairn requested does not fail the turn: {events:?}"
    );
    // "Run cancelled" says it all: the status repeats no reason.
    let cancelled = events
        .iter()
        .find(|event| event["type"] == "status" && event["text"] == "cancelled")
        .unwrap();
    assert!(cancelled["payload"].is_null(), "{cancelled}");
    eventually(
        Duration::from_secs(5),
        Duration::from_millis(30),
        async || {
            let response = command("resume").send().await.unwrap();
            if response.status() == StatusCode::CONFLICT {
                return None;
            }
            response.error_for_status().unwrap();
            Some(())
        },
    )
    .await;
    fixture
        .until(id, |r| r["status"] == RunStatus::Succeeded)
        .await;
    assert_eq!(
        fixture.checkpoint(id).await.get("remainingMs"),
        Some(&Value::Null)
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn quiet_chat_protocol_keeps_a_finite_checkpoint_deadline() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    fixture.clock = Some(FixtureClock::new(fixture.root.path()));
    let s = fixture.service.clone();
    let run = fixture.enqueue("fixture:hang").await;
    let id = text(&run, "id");
    let mut snapshot = run["snapshot"].clone();
    snapshot["agent"]["timeoutMinutes"] = 1.into();
    s.store
        .change_run(
            id,
            cairn_installation::runs::RunChanges {
                snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Resume the final second of an existing one-minute budget.
    common::set_checkpoint(
        &s.store,
        id,
        json!({ "launched": false, "remainingMs": 1000 }),
    )
    .await;
    fixture.start().await;
    fixture
        .until(id, |r| r["sessionId"] == "fixture-chat")
        .await;
    // Ensure the quiet turn has started before advancing its resumed budget.
    eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            let path = fixture
                .run_directory(id)
                .join("codex/fixture-conversation.json");
            let data = tokio::fs::read(path).await.ok()?;
            let thread: Value = serde_json::from_slice(&data).ok()?;
            thread["turns"]
                .as_array()?
                .iter()
                .any(|turn| turn["status"] == "inProgress")
                .then_some(())
        },
    )
    .await;
    // Real time and the fixture's monotonic timers continue while its wall
    // clock is frozen; the resumed budget must still be intact.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let still_running = s.store.run(id).await.unwrap();
    assert_eq!(
        still_running["status"],
        RunStatus::Running,
        "{still_running}"
    );
    assert_eq!(fixture.checkpoint(id).await["remainingMs"], 1000);
    fixture.clock.as_mut().unwrap().advance(1250);
    let failed = tokio::time::timeout(
        Duration::from_secs(20),
        fixture.until(id, |r| r["status"] == RunStatus::Failed),
    )
    .await
    .expect("a quiet chat must stop when its finite checkpoint budget expires");
    assert!(text(&failed, "summary").contains("time limit"), "{failed}");
    fixture.stop(false).await;
}

#[tokio::test]
async fn queued_work_uses_current_node_grants_without_rejecting_unrelated_policy() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = &fixture.service;
    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["access"]["nodes"] = Value::Null;
    s.store.put_agent_fixture(agent.clone()).await.unwrap();
    let run = fixture
        .enqueue("Inspect the fixture after a node policy change")
        .await;
    agent["access"]["nodes"] = json!([LOCAL_NODE_ID]);
    s.store.put_agent_fixture(agent).await.unwrap();
    fixture.start().await;
    let completed = fixture.until_finished(text(&run, "id")).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");
    assert_eq!(
        completed["snapshot"]["agent"]["access"]["nodes"],
        json!([LOCAL_NODE_ID])
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn queued_work_continues_with_equivalent_or_wider_access_but_not_reduced_access() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let run = fixture
        .enqueue("Inspect the fixture after an equivalent access change")
        .await;
    let id = text(&run, "id");
    let mut agent = s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    agent["name"] = "Renamed agent".into();
    s.store.put_agent_fixture(agent.clone()).await.unwrap();
    fixture.start().await;
    let completed = fixture.until_finished(id).await;
    assert_eq!(completed["status"], RunStatus::Succeeded, "{completed}");

    fixture.stop(false).await;
    let reduced_run = fixture
        .enqueue("Inspect the fixture after a reduction")
        .await;
    agent["access"]["sandbox"] = "read-only".into();
    s.store.put_agent_fixture(agent).await.unwrap();
    fixture.start().await;
    let failed = fixture.until_finished(text(&reduced_run, "id")).await;
    assert_eq!(failed["status"], RunStatus::Failed, "{failed}");
    assert!(
        text(&failed, "error").contains("Agent access was reduced"),
        "{failed}"
    );
    fixture.stop(false).await;
}

#[tokio::test]
async fn queued_work_waits_for_node_capacity_without_failing() {
    let mut fixture = Fixture::new().await;
    fixture.stop(false).await;
    let s = fixture.service.clone();
    let node = id();
    let put_node = |name: &str| {
        json!({
            "id": node,
            "name": name,
            "accepting": true,
            "executionReady": true,
            "runtimeId": "development",

            "lastSeen": now() + 60_000,
            "capabilities": {"kvm": true },
            "limits": {"cpu": 1, "memoryMiB": 8192, "diskMiB": 65536},
            "pressure": "memory"
        })
    };
    s.store.put_node_fixture(put_node("Laptop")).await.unwrap();
    let mut agent = s
        .agent_snapshot(cairn_installation::config::MAIN_AGENT_ID)
        .await
        .unwrap();
    agent["access"]["nodes"] = json!([node]);
    s.store.put_agent_fixture(agent).await.unwrap();
    let run = fixture.enqueue("Waits for shared RAM headroom").await;
    let run_id = text(&run, "id");
    let resources = json!({"cpu": 2, "memoryMiB": 4096, "diskMiB": 32768});
    s.store
        .update_run_boundary_fixture(run_id, json!({"requestedResources": resources}))
        .await
        .unwrap();
    fixture.start().await;
    let waiting = fixture
        .until(run_id, |r| {
            text(r, "accountWaitReason").starts_with("Waiting for capacity.")
        })
        .await;
    let reason = text(&waiting, "accountWaitReason");
    assert!(reason.contains("shared headroom"), "{reason}");
    assert!(
        reason.contains("Laptop has 4 free slots; check memory"),
        "{reason}"
    );
    assert_eq!(waiting["status"], "queued");
    // New figures replace the reason without repeating the status event.
    s.store.put_node_fixture(put_node("Desk")).await.unwrap();
    let updated = fixture
        .until(run_id, |r| {
            text(r, "accountWaitReason").contains("Desk has")
        })
        .await;
    assert_eq!(updated["status"], "queued");
    let id = run_id.to_owned();
    let events = s
        .store
        .read(move |db| db.events_fixture(&id, 0, 500))
        .await
        .unwrap();
    let waits = events
        .iter()
        .filter(|e| text(e, "text").starts_with("Waiting for capacity."))
        .count();
    assert_eq!(waits, 1, "{events:?}");
    let attempts = s.store.attempts_fixture().await.unwrap();
    assert!(attempts.is_empty(), "{attempts:?}");
    fixture.stop(false).await;
}
