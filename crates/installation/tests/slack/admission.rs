//! Admission fixtures use the real package and native authority; no work is completed synthetically.
use super::*;

#[tokio::test]
async fn replacing_the_slack_app_refuses_a_workspace_verified_for_the_previous_app() {
    app_binding(AppBinding::ActiveReplacement).await;
}

#[tokio::test]
async fn replacing_the_app_before_activation_cannot_promote_another_apps_workspace_authority() {
    app_binding(AppBinding::PreparedReplacement).await;
}

#[tokio::test]
async fn a_legacy_workspace_must_be_reverified_before_its_token_can_be_used() {
    app_binding(AppBinding::Legacy).await;
}

#[derive(Clone, Copy)]
enum AppBinding {
    ActiveReplacement,
    PreparedReplacement,
    Legacy,
}

async fn app_binding(scenario: AppBinding) {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let slack = Arc::new(Slack::default());
    let provider = Router::new()
        .route("/api/{method}", post(api).get(api))
        .with_state(slack.clone());
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let replacement_secret = "synthetic-replacement-signature";
    let (instance, resource) = if matches!(scenario, AppBinding::PreparedReplacement) {
        let draft = save(&app, "/api/plugins/slack/setup-drafts", json!({
            "name": "Prepared app", "settings": {"appId": "ATEST", "transport": "http", "sharingAcknowledged": true, "maxRequestAgeMinutes": 15},
            "secrets": {"signingSecret": SIGNING_SECRET},
        })).await;
        let path = format!("/api/plugins/slack/setup-drafts/{}", text(&draft, "id"));
        let resource = save(&app, &format!("{path}/resources/workspace"), json!({
            "setupRevision": draft["revision"], "name": "Prepared workspace", "settings": {"channelId": "CTEST"},
            "secrets": {"botToken": BOT_TOKEN},
        })).await;
        let mut settings = draft["settings"].clone();
        settings["appId"] = json!("AOTHER");
        let changed = common::send(
            &app,
            owner(
                "PATCH",
                &path,
                &json!({
                    "revision": draft["revision"], "name": draft["name"], "settings": settings,
                    "secrets": {"signingSecret": replacement_secret},
                }),
            ),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);
        let changed = common::read_json(changed).await;
        let active = save(
            &app,
            &format!("{path}/activate"),
            json!({"revision": changed["revision"], "enabled": true}),
        )
        .await;
        (active, resource)
    } else {
        configure(&app, &service).await
    };
    let agent = service.agent(json!({
        "name": "App binding", "access": {"projects": [], "skills": [], "mcps": [], "installation": false},
    }), None).await.unwrap();
    save(&app, &format!("/api/plugins/slack/connections/{}/resources/workspace/{}/routes", text(&instance,"id"), text(&resource,"id")), json!({
        "name": "Allowed", "enabled": true, "match": {"channel": "CTEST"}, "agentId": agent["id"],
        "accessMode": "allowlist", "userIds": ["UALICE"], "maxMessagesPerHour": 10, "maxPerPersonPerHour": 5,
    })).await;
    let path = format!("/api/plugins/slack/connections/{}", text(&instance, "id"));
    if matches!(scenario, AppBinding::ActiveReplacement) {
        let mut settings = instance["settings"].clone();
        settings["appId"] = json!("AOTHER");
        let changed = common::send(
            &app,
            owner(
                "PUT",
                &path,
                &json!({
                    "name": instance["name"], "enabled": true, "settings": settings,
                    "secrets": {"signingSecret": replacement_secret},
                }),
            ),
        )
        .await;
        assert_eq!(
            changed.status(),
            StatusCode::OK,
            "{}",
            common::read_text(changed).await
        );
    } else if matches!(scenario, AppBinding::Legacy) {
        // The prior package persisted this exact resource without an appId.
        // Keep its encrypted credential/revision; only reproduce that old format.
        let id = text(&resource, "id").to_owned();
        service
            .store
            .transaction(move |db| {
                let mut old = db.get("plugin-resources", &id)?.unwrap();
                old["settings"].as_object_mut().unwrap().remove("appId");
                db.put("plugin-resources", &old)?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let before = slack.metadata_attempts.load(Ordering::SeqCst);
    let envelope = json!({
        "type": "event_callback", "api_app_id": if matches!(scenario, AppBinding::Legacy) {"ATEST"} else {"AOTHER"}, "team_id": "TTEST",
        "event": {"type": "app_mention", "user": "UALICE", "channel": "CTEST", "ts": format!("{}.000100", now()/1000),
            "text": "<@UNEWBOT> ask <@UBOT> to inspect this"},
    });
    let secret = if matches!(scenario, AppBinding::Legacy) {
        SIGNING_SECRET
    } else {
        replacement_secret
    };
    let refused = ingress(&app, text(&instance, "id"), envelope, secret).await;
    assert_eq!(refused.0, StatusCode::FORBIDDEN, "{refused:?}");
    assert_eq!(
        slack.metadata_attempts.load(Ordering::SeqCst),
        before,
        "The former app's token was used before refusal"
    );
    assert!(service.store.list(work::EVENTS).await.unwrap().is_empty());
    assert!(slack.posts.lock().await.is_empty());
    if matches!(scenario, AppBinding::Legacy) {
        let resource_path = format!("{path}/resources/workspace/{}", text(&resource, "id"));
        let current = common::send(&app, owner("GET", &resource_path, &json!({}))).await;
        assert_eq!(current.status(), StatusCode::OK);
        let current = common::read_json(current).await;
        let preview = save(
            &app,
            &format!("{path}/resources/workspace/form"),
            json!({
                "resource": current["id"], "revision": current["revision"], "settings": current["settings"],
            }),
        )
        .await;
        assert_eq!(preview["values"]["appId"], "ATEST");
        let mut settings = current["settings"].clone();
        for (key, value) in preview["values"].as_object().unwrap() {
            settings[key] = value.clone();
        }
        let checked = common::send(
            &app,
            owner(
                "PUT",
                &resource_path,
                &json!({
                    "revision": current["revision"], "name": current["name"], "enabled": true,
                    "settings": settings,
                }),
            ),
        )
        .await;
        assert_eq!(
            checked.status(),
            StatusCode::OK,
            "{}",
            common::read_text(checked).await
        );
    } else {
        let mut settings = instance["settings"].clone();
        settings["appId"] = json!("ATEST");
        let restored = common::send(
            &app,
            owner(
                "PUT",
                &path,
                &json!({
                    "name": instance["name"], "enabled": true, "settings": settings,
                    "secrets": {"signingSecret": SIGNING_SECRET},
                }),
            ),
        )
        .await;
        assert_eq!(restored.status(), StatusCode::OK);
    }
    let accepted = ingress(&app, text(&instance,"id"), json!({
        "type": "event_callback", "api_app_id": "ATEST", "team_id": "TTEST",
        "event": {"type": "app_mention", "user": "UALICE", "channel": "CTEST", "ts": format!("{}.000101", now()/1000),
            "text": "<@UBOT> The original app still matches"},
    }), SIGNING_SECRET).await;
    assert_eq!(accepted, (StatusCode::OK, "accepted".into()));
    assert_eq!(service.store.list(work::EVENTS).await.unwrap().len(), 1);
    service.shutdown.cancel();
    server.abort();
}

pub(super) async fn retain_transient_reply(service: &Service, slack: &Slack, before: usize) {
    for index in 0..6 {
        let delivery = common::eventually(
            Duration::from_secs(15),
            Duration::from_millis(20),
            async || {
                let rows = service.store.list(work::DELIVERIES).await.unwrap();
                (slack.metadata_attempts.load(Ordering::SeqCst) > before + index)
                    .then(|| {
                        rows.into_iter().find(|row| {
                            row["nextAttemptAt"]
                                .as_i64()
                                .is_some_and(|at| at > now() + 100)
                        })
                    })
                    .flatten()
            },
        )
        .await;
        assert_eq!(
            delivery["attempts"], 0,
            "A definite transient metadata read consumed the delivery-error budget"
        );
        assert!(
            slack.posts.lock().await.is_empty(),
            "A reply bypassed unavailable channel metadata"
        );
        service
            .store
            .write(move |db| {
                let mut current = db
                    .get(work::DELIVERIES, text(&delivery, "id"))?
                    .expect("The accepted reply disappeared");
                assert_eq!(current["attempts"], 0);
                // Only advance scheduling; no run, answer or receipt is fabricated.
                current["nextAttemptAt"] = json!(now());
                db.put(work::DELIVERIES, &current)
            })
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_reply_metadata_does_not_exhaust_accepted_delivery() {
    exchange(DeliveryScenario::ReplyTransientMetadata).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_metadata_reads_do_not_exhaust_a_completed_accepted_reply() {
    exchange(DeliveryScenario::ReplyInterruptedMetadata).await;
}

#[tokio::test]
async fn read_transport_recovery_does_not_hide_native_authority_or_write_failures() {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    let package = registry::builtin("slack").unwrap();
    let mut source = package.source;
    source.main = r#"
        local api = require('./lib/api')
        return { test = function(event)
            return api.request(event.method, 'conversations.info', nil, nil, nil)
        end }
    "#
    .to_owned();
    for (method, code, retryable) in [
        ("GET", "unavailable", true),
        ("GET", "forbidden", false),
        ("GET", "invalid", false),
        ("GET", "not_found", false),
        ("POST", "unavailable", false),
    ] {
        let mut session = service
            .plugins
            .host
            .start(
                &PathBuf::from(env!("CARGO_BIN_EXE_cairn")),
                &package.manifest,
                &source,
            )
            .await
            .unwrap();
        let response = session
            .dispatch("test", json!({ "method": method }), |name, _| async move {
                assert_eq!(name, "http.request");
                Err(cairn_plugin_host::Denied(code))
            })
            .await;
        session.stop().await;
        if retryable {
            assert_eq!(response.unwrap()["status"], 503);
        } else {
            assert!(
                response.is_err(),
                "{method} {code} was treated as a transient read"
            );
        }
    }
    service.shutdown.cancel();
}

#[tokio::test]
async fn transient_json_metadata_keeps_http_mentions_retryable() {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let slack = Arc::new(Slack::default());
    let provider = Router::new()
        .route("/api/{method}", post(api).get(api))
        .with_state(slack.clone());
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let (instance, resource) = configure(&app, &service).await;
    let agent = service
        .agent(
            json!({
                "name": "Read retry",
                "access": { "projects": [], "skills": [], "mcps": [], "installation": false },
            }),
            None,
        )
        .await
        .unwrap();
    let routes = format!(
        "/api/plugins/slack/connections/{}/resources/workspace/{}/routes",
        text(&instance, "id"),
        text(&resource, "id")
    );
    save(
        &app,
        &routes,
        json!({
            "name": "Authorized requests",
            "enabled": true,
            "match": { "channel": "CTEST" },
            "agentId": agent["id"],
            "accessMode": "allowlist",
            "userIds": ["UALICE"],
            "maxMessagesPerHour": 10,
            "maxPerPersonPerHour": 5,
        }),
    )
    .await;
    for (index, code) in [
        "internal_error",
        "service_unavailable",
        "fatal_error",
        "org_login_required",
        "team_added_to_org",
        "ratelimited",
    ]
    .into_iter()
    .enumerate()
    {
        let envelope = json!({
            "type": "event_callback",
            "api_app_id": "ATEST",
            "team_id": "TTEST",
            "event": {
                "type": "app_mention",
                "user": "UALICE",
                "channel": "CTEST",
                "ts": format!("{}.{index:06}", now() / 1000),
                "text": "<@UBOT> Retry this read safely",
            },
        });
        slack
            .metadata_errors
            .lock()
            .await
            .push_back(code.to_owned());
        assert_eq!(
            ingress(
                &app,
                text(&instance, "id"),
                envelope.clone(),
                SIGNING_SECRET
            )
            .await
            .0,
            StatusCode::SERVICE_UNAVAILABLE,
            "{code}"
        );
        assert_eq!(service.store.list(work::EVENTS).await.unwrap().len(), index);
        if code == "ratelimited" {
            tokio::time::sleep(Duration::from_millis(2100)).await;
        }
        assert_eq!(
            ingress(&app, text(&instance, "id"), envelope, SIGNING_SECRET).await,
            (StatusCode::OK, "accepted".into())
        );
    }
    for (index, code) in ["channel_not_found", "missing_scope", "invalid_auth"]
        .into_iter()
        .enumerate()
    {
        slack
            .metadata_errors
            .lock()
            .await
            .push_back(code.to_owned());
        let envelope = json!({
            "type": "event_callback",
            "api_app_id": "ATEST",
            "team_id": "TTEST",
            "event": {
                "type": "app_mention",
                "user": "UALICE",
                "channel": "CTEST",
                "ts": format!("{}.{:06}", now() / 1000, index + 100),
                "text": "<@UBOT> Denied metadata",
            },
        });
        assert_eq!(
            ingress(&app, text(&instance, "id"), envelope, SIGNING_SECRET)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{code}"
        );
        assert_eq!(service.store.list(work::EVENTS).await.unwrap().len(), 6);
    }
    service.shutdown.cancel();
    server.abort();
}

pub(super) async fn stale_second_route(app: &Router, service: &Arc<Service>, base: &str) {
    let resource = save(
        app,
        base,
        json!({
            "name": "Second source",
            "settings": { "channelId": "CSECOND" },
            "secrets": { "botToken": BOT_TOKEN },
        }),
    )
    .await;
    let agent = service
        .agent(
            json!({
                "name": "Agent requiring reapproval",
                "access": { "projects": [], "skills": [], "mcps": [], "installation": false },
            }),
            None,
        )
        .await
        .unwrap();
    save(
        app,
        &format!("{base}/{}/routes", text(&resource, "id")),
        json!({
            "name": "Stale source",
            "enabled": true,
            "match": { "channel": "CSECOND" },
            "agentId": agent["id"],
            "accessMode": "allowlist",
            "userIds": [ "UALICE" ],
            "maxMessagesPerHour": 10,
            "maxPerPersonPerHour": 5,
        }),
    )
    .await;
    let mut changed = agent.clone();
    changed["access"]["installation"] = json!(true);
    service
        .agent(changed, Some(text(&agent, "id")))
        .await
        .unwrap();
}

#[tokio::test]
async fn request_age_uses_receipt_time_even_when_an_older_signature_is_still_valid() {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let slack = Arc::new(Slack::default());
    let provider = Router::new()
        .route("/api/{method}", post(api).get(api))
        .with_state(slack);
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let (instance, resource) = configure(&app, &service).await;
    let agent = service
        .agent(
            json!({
                "name": "Age check",
                "access": { "projects": [], "skills": [], "mcps": [], "installation": false },
            }),
            None,
        )
        .await
        .unwrap();
    save(
        &app,
        &format!(
            "/api/plugins/slack/connections/{}/resources/workspace/{}/routes",
            text(&instance, "id"),
            text(&resource, "id")
        ),
        json!({
            "name": "Age route",
            "enabled": true,
            "match": { "channel": "CTEST" },
            "agentId": agent["id"],
            "accessMode": "allowlist",
            "userIds": [ "UALICE" ],
            "maxMessagesPerHour": 10,
            "maxPerPersonPerHour": 5,
        }),
    )
    .await;
    let received = now() / 1000;
    let envelope = json!({
        "type": "event_callback",
        "api_app_id": "ATEST",
        "team_id": "TTEST",
        "event": {
            "type": "app_mention",
            "user": "UALICE",
            "channel": "CTEST",
            "ts": format!("{}.000001", received - 16 * 60),
            "text": "<@UBOT> Delayed outside the automatic window",
        },
    });
    // Signature is only two minutes old and remains cryptographically fresh.
    // The source request was14min old when signed, but16min old when received.
    let result = ingress_at(
        &app,
        text(&instance, "id"),
        envelope,
        SIGNING_SECRET,
        received - 120,
    )
    .await;
    assert_eq!(
        result,
        (
            StatusCode::OK,
            "Expired request; mention the app again to start fresh".into()
        )
    );
    assert!(
        service.store.list(work::EVENTS).await.unwrap().is_empty(),
        "Expired work was imported"
    );
    service.shutdown.cancel();
    server.abort();
}
