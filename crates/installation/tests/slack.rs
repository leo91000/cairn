#[path = "slack/checkpoints.rs"]
mod checkpoints;
mod common;
#[path = "slack/controller.rs"]
mod controller;

use aws_lc_rs::hmac;
use axum::{
    Json, Router,
    body::Body,
    extract::{Form, FromRequest, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::now,
    plugins::{capabilities, connections, registry, work},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{Mutex, Notify},
};

const SIGNING_SECRET: &str = "synthetic-slack-signature";
const BOT_TOKEN: &str = "xoxb-synthetic-slack-test";

#[derive(Default)]
struct Slack {
    posts: Mutex<Vec<HashMap<String, String>>>,
    private: AtomicBool,
    rate_limit_first: AtomicBool,
    post_attempts: AtomicUsize,
    rate_limit_second: AtomicBool,
    hold_second_reply: AtomicBool,
    uncertain_first: AtomicBool,
    cooldown_race: AtomicBool,
    channel_attempts: AtomicUsize,
    metadata_attempts: AtomicUsize,
    channel_entered: Notify,
    release_channel: Notify,
    hold_reply_check: AtomicBool,
    post_times: Mutex<Vec<Instant>>,
}

async fn api(
    State(slack): State<Arc<Slack>>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    assert_eq!(
        headers.get("authorization").unwrap(),
        format!("Bearer {BOT_TOKEN}").as_str()
    );
    let path = request.uri().path().to_owned();
    let Form(values) = Form::<HashMap<String, String>>::from_request(request, &())
        .await
        .unwrap();
    let value = match path.as_str() {
        "/api/auth.test" => {
            json!({"ok": true, "team_id": "TTEST", "user_id": "UBOT", "bot_id": "BBOT"})
        }
        "/api/bots.info" => {
            assert_eq!(values["bot"], "BBOT");
            json!({"ok": true, "bot": {"id": "BBOT", "app_id": "ATEST"}})
        }
        "/api/conversations.info" => {
            slack.metadata_attempts.fetch_add(1, Ordering::SeqCst);
            assert!(["CTEST", "CSECOND"].contains(&values["channel"].as_str()));
            if slack.hold_reply_check.swap(false, Ordering::SeqCst) {
                slack.channel_entered.notify_one();
                slack.release_channel.notified().await;
            }
            if slack.cooldown_race.load(Ordering::SeqCst) {
                let attempt = slack.channel_attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    slack.channel_entered.notify_one();
                    slack.release_channel.notified().await;
                } else if attempt == 1 {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "900")],
                        Json(json!({"ok": false, "error": "ratelimited"})),
                    )
                        .into_response();
                }
            }
            json!({"ok": true, "channel": {"id": values["channel"], "name": "cairn", "is_private": slack.private.load(Ordering::SeqCst), "is_archived": false, "is_member": true, "is_shared": false}})
        }
        "/api/chat.postMessage" => {
            let attempt = slack.post_attempts.fetch_add(1, Ordering::SeqCst);
            slack.post_times.lock().await.push(Instant::now());
            if attempt == 1 && slack.rate_limit_second.swap(false, Ordering::SeqCst) {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(
                        "retry-after",
                        if slack.hold_second_reply.load(Ordering::SeqCst) {
                            "900"
                        } else {
                            "1"
                        },
                    )],
                    Json(json!({"ok": false, "error": "ratelimited"})),
                )
                    .into_response();
            }
            if slack.rate_limit_first.swap(false, Ordering::SeqCst) {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "900")],
                    Json(json!({"ok": false, "error": "ratelimited"})),
                )
                    .into_response();
            }
            let channel = values["channel"].clone();
            slack.posts.lock().await.push(values);
            if slack.uncertain_first.swap(false, Ordering::SeqCst) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": "internal_error"})),
                )
                    .into_response();
            }
            json!({"ok": true, "channel": channel, "ts": "1700000001.000001"})
        }
        _ => panic!("unexpected Slack method {path}"),
    };
    (StatusCode::OK, Json(value)).into_response()
}

fn owner(method: &str, path: &str, value: &Value) -> axum::extract::Request {
    let mut request = common::request(method, path)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    request
}

async fn save(app: &Router, path: &str, input: Value) -> Value {
    let response = common::send(app, owner("POST", path, &input)).await;
    let status = response.status();
    let body = common::read_json(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn ingress(app: &Router, instance: &str, value: Value, secret: &str) -> (StatusCode, String) {
    let body = value.to_string();
    let timestamp = (now() / 1000).to_string();
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let signature =
        hex::encode(hmac::sign(&key, format!("v0:{timestamp}:{body}").as_bytes()).as_ref());
    let request = common::request("POST", &format!("/api/plugin-webhooks/{instance}/events"))
        .header("content-type", "application/json")
        .header("x-slack-request-timestamp", timestamp)
        .header("x-slack-signature", format!("v0={signature}"))
        .body(Body::from(body))
        .unwrap();
    let response = common::send(app, request).await;
    (response.status(), common::read_text(response).await)
}

fn app_manifest(form: &Value) -> Value {
    form["view"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["type"] == "code")
        .find_map(|node| serde_json::from_str(text(node, "text")).ok())
        .expect("the actual Luau form did not generate a Slack app manifest")
}

/// Run the owner setup handlers, not a preactivated connection fixture. Applying
/// this generated manifest in Slack's dashboard remains a separate live step.
async fn configure(app: &Router, service: &Service) -> (Value, Value) {
    let collection = "/api/plugins/slack/setup-drafts";
    let draft = save(
        app,
        collection,
        json!({"name": "Test Slack app", "settings": {}}),
    )
    .await;
    let root = format!("{collection}/{}", text(&draft, "id"));
    let form_path = format!("{root}/form");
    let form = save(app, &form_path, json!({"revision": draft["revision"]})).await;
    assert!(app_manifest(&form)["settings"]["event_subscriptions"].is_null());
    let response = common::send(app, owner("PATCH", &root, &json!({
        "revision": draft["revision"], "name": "Test Slack app",
        "settings": {"appId": "ATEST", "sharingAcknowledged": true, "maxRequestAgeMinutes": 15},
        "secrets": {"signingSecret": SIGNING_SECRET},
    }))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let draft = common::read_json(response).await;
    let id = text(&draft, "id");
    let form = save(app, &form_path, json!({"revision": draft["revision"]})).await;
    let manifest = app_manifest(&form);
    assert!(
        text(&manifest["settings"]["event_subscriptions"], "request_url")
            .ends_with(&format!("/plugins/{id}/events"))
    );
    assert_eq!(
        manifest["settings"]["event_subscriptions"]["bot_events"],
        json!(["app_mention"])
    );
    assert_eq!(
        ingress(
            app,
            id,
            json!({"type": "url_verification", "challenge": "inert-draft-challenge"}),
            SIGNING_SECRET
        )
        .await,
        (StatusCode::OK, "inert-draft-challenge".into())
    );
    assert_eq!(
        ingress(app, id, json!({"type": "event_callback"}), SIGNING_SECRET)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert!(
        service
            .store
            .list("plugin-events")
            .await
            .unwrap()
            .is_empty()
    );
    let resource = save(
        app,
        &format!("{root}/resources/workspace"),
        json!({
            "setupRevision": draft["revision"], "name": "Test workspace",
            "settings": {"channelId": "CTEST"}, "secrets": {"botToken": BOT_TOKEN},
        }),
    )
    .await;
    let instance = save(
        app,
        &format!("{root}/activate"),
        json!({"revision": draft["revision"], "enabled": true}),
    )
    .await;
    assert_eq!(instance["id"], draft["id"]);
    assert!(!instance.to_string().contains(SIGNING_SECRET));
    (instance, resource)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_mention_runs_as_its_author_and_posts_the_real_fixture_answer_in_its_thread() {
    exchange(DeliveryScenario::Normal).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_delivery_state_does_not_exhaust_plugin_storage_after_old_replies() {
    exchange(DeliveryScenario::FullStorage).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_cooldown_is_shared_across_channels_and_early_scheduler_retries() {
    exchange(DeliveryScenario::RateLimited).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_converted_after_admission_cannot_receive_the_answer() {
    exchange(DeliveryScenario::PrivateBeforeReply).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uncertain_committed_post_is_not_repeated_after_manager_restart() {
    exchange(DeliveryScenario::UncertainRestart).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_storage_preserves_definite_rate_limits_across_channels() {
    exchange(DeliveryScenario::FullStorageRateLimited).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreserved_full_storage_refuses_provider_io_and_admission() {
    exchange(DeliveryScenario::UnreservedStorage).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_unicode_answers_arrive_completely_in_order_in_the_same_slack_thread() {
    exchange(DeliveryScenario::LongReply).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limiting_a_later_chunk_does_not_repeat_earlier_parts_of_the_answer() {
    exchange(DeliveryScenario::LongReplyRateLimited).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restarting_between_chunks_continues_the_answer_without_repeating_the_first_part() {
    exchange(DeliveryScenario::LongReplyRestart).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_long_answer_survives_the_next_authorized_slack_turn() {
    exchange(DeliveryScenario::LongFailedReplyAdvanced).await;
}

#[derive(Clone, Copy)]
enum DeliveryScenario {
    Normal,
    LongReply,
    LongReplyRateLimited,
    LongReplyRestart,
    LongFailedReplyAdvanced,
    FullStorage,
    FullStorageRateLimited,
    UnreservedStorage,
    RateLimited,
    PrivateBeforeReply,
    UncertainRestart,
    ChannelCadence,
}

async fn start_manager(file: &std::path::Path, slack_origin: &str) -> tokio::process::Child {
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_cairn"))
        .parent()
        .unwrap()
        .join("examples/worker_fixture");
    let mut manager = Command::new(executable)
        .arg("serve")
        .env("CAIRN_CONFIG", file)
        .env("CAIRN_FIXTURE_PLUGIN_HTTP_ORIGIN", slack_origin)
        .env_remove("CAIRN_TOOLKIT_DIR")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(manager.stdout.take().unwrap()).lines();
    let listening = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(listening.starts_with("Listening on"), "{listening}");
    manager
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_threads_share_channel_cadence_including_a_fresh_worker() {
    exchange(DeliveryScenario::ChannelCadence).await;
}

async fn stop_manager(manager: &mut tokio::process::Child) {
    unsafe {
        libc::kill(manager.id().unwrap() as i32, libc::SIGTERM);
    }
    if tokio::time::timeout(Duration::from_secs(10), manager.wait())
        .await
        .is_err()
    {
        manager.kill().await.unwrap();
        let _ = manager.wait().await;
        panic!("the test manager did not shut down gracefully");
    }
}

async fn fill_storage(service: &Service, instance_id: &str) {
    let connection = instance_id.to_owned();
    service
        .store
        .transaction(move |db| {
            let existing = db
                .list("plugin-storage")?
                .iter()
                .filter(|row| row["connectionId"] == connection)
                .count();
            for index in existing..1000 {
                let key = format!("reply:completed-{index}");
                let id = format!(
                    "storage:{connection}:{}",
                    cairn_installation::auth::hex_digest(&key)
                );
                db.put(
                    "plugin-storage",
                    &json!({
                        "id": id, "connectionId": connection, "key": key,
                        "value": {"status": "sent", "ts": "1700000000.000001"}, "updatedAt": now(),
                    }),
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        service.store.list("plugin-storage").await.unwrap().len(),
        1000
    );
}

async fn exchange(scenario: DeliveryScenario) {
    let prefill_receipts = matches!(
        scenario,
        DeliveryScenario::FullStorage | DeliveryScenario::FullStorageRateLimited
    );
    let rate_limited = matches!(
        scenario,
        DeliveryScenario::RateLimited | DeliveryScenario::FullStorageRateLimited
    );
    let long_reply = matches!(
        scenario,
        DeliveryScenario::LongReply
            | DeliveryScenario::LongReplyRateLimited
            | DeliveryScenario::LongReplyRestart
            | DeliveryScenario::LongFailedReplyAdvanced
    );
    let root = TempDir::new().unwrap();
    let mut config = common::config(root.path());
    config.codex_bin = common::fixture("codex.mjs");
    std::fs::create_dir_all(&config.home).unwrap();
    // A synthetic legacy login in this temporary home: never host credentials.
    // No managed marker: it would require an account broker for this fake login.
    std::fs::create_dir_all(config.home.join(".codex")).unwrap();
    std::fs::write(config.home.join(".codex/auth.json"), r#"{"tokens":{}}"#).unwrap();
    let controller = controller::Controller::new(config.clone());
    let (controller_origin, controller_server) = common::serve_locally(controller.router()).await;
    config.runner_url = controller_origin;
    let service = Service::new(config.clone()).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    assert!(
        registry::builtin("slack").is_some(),
        "the actual bundled package must load"
    );
    let slack = Arc::new(Slack::default());
    slack.rate_limit_second.store(
        matches!(
            scenario,
            DeliveryScenario::LongReplyRateLimited | DeliveryScenario::LongFailedReplyAdvanced
        ),
        Ordering::SeqCst,
    );
    slack.rate_limit_first.store(rate_limited, Ordering::SeqCst);
    slack.hold_second_reply.store(
        matches!(scenario, DeliveryScenario::LongFailedReplyAdvanced),
        Ordering::SeqCst,
    );
    slack.uncertain_first.store(
        matches!(scenario, DeliveryScenario::UncertainRestart),
        Ordering::SeqCst,
    );
    let slack_app = Router::new()
        .route("/api/{method}", post(api).get(api))
        .with_state(slack.clone());
    let (slack_origin, slack_server) = common::serve_locally(slack_app).await;
    service.plugins.route_http_to(Some(slack_origin.clone()));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let (instance, resource) = configure(&app, &service).await;
    let instance_id = text(&instance, "id");
    if matches!(scenario, DeliveryScenario::UnreservedStorage) {
        fill_storage(&service, instance_id).await;
        let attempts = slack.metadata_attempts.load(Ordering::SeqCst);
        let envelope = json!({"type": "event_callback", "api_app_id": "ATEST", "team_id": "TTEST",
            "event": {"type": "app_mention", "user": "UALICE", "channel": "CTEST",
                "ts": format!("{}.000001", now() / 1000), "text": "<@UBOT> Unreserved store"}});
        let result = ingress(&app, instance_id, envelope, SIGNING_SECRET).await;
        assert_ne!(
            result.0,
            StatusCode::OK,
            "quota state must be reserved before admission"
        );
        assert_eq!(
            slack.metadata_attempts.load(Ordering::SeqCst),
            attempts,
            "provider I/O preceded the required quota reservation"
        );
        assert!(
            service
                .store
                .list("plugin-events")
                .await
                .unwrap()
                .is_empty()
        );
        service.shutdown.cancel();
        slack_server.abort();
        controller_server.abort();
        return;
    }

    let base = format!("/api/plugins/slack/connections/{instance_id}/resources/workspace");
    assert_eq!(resource["settings"]["externalId"], "TTEST");
    assert_eq!(resource["settings"]["botId"], "UBOT");
    assert!(!resource.to_string().contains(BOT_TOKEN));
    let agent = service.agent(json!({"name": "Slack helper", "access": {"projects": [], "skills": [], "mcps": [], "installation": false}}), None).await.unwrap();
    let route = save(&app, &format!("{base}/{}/routes", text(&resource,"id")), json!({
        "name": "Authorized channel", "enabled": true, "match": {"channel": "CTEST"}, "agentId": agent["id"],
        "accessMode": "allowlist", "userIds": ["UALICE"], "maxMessagesPerHour": 10, "maxPerPersonPerHour": 5
    })).await;
    let ts = format!("{}.000001", now() / 1000);
    let prompt = if long_reply {
        format!(
            "{}Explain the Slack tracer {}",
            if matches!(scenario, DeliveryScenario::LongFailedReplyAdvanced) {
                "fixture:failed-final "
            } else {
                ""
            },
            "é🙂&<> ".repeat(1500)
        )
    } else {
        "Explain the Slack tracer".to_owned()
    };
    // The fake agent emits this exact final answer, after its separate commentary.
    let expected_answer = format!(
        "I reviewed the workspace. **The approach looks good.**\n\n{}\n\nReady for the next step.",
        prompt.trim()
    );
    let envelope = json!({"type": "event_callback", "api_app_id": "ATEST", "team_id": "TTEST", "event_id": "EvTEST",
        "event": {"type": "app_mention", "user": "UALICE", "channel": "CTEST", "ts": ts, "text": format!("<@UBOT> {prompt}")}});
    assert_eq!(
        ingress(
            &app,
            instance_id,
            json!({"type": "url_verification", "challenge": "fixture-challenge"}),
            SIGNING_SECRET
        )
        .await,
        (StatusCode::OK, "fixture-challenge".into())
    );
    assert_eq!(
        ingress(&app, instance_id, envelope.clone(), "wrong-secret")
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let mut denied = envelope.clone();
    denied["event"]["user"] = json!("UMALLORY");
    denied["event"]["ts"] = json!(format!("{}.000002", now() / 1000));
    assert_eq!(
        ingress(&app, instance_id, denied, SIGNING_SECRET).await,
        (StatusCode::OK, "denied".into())
    );
    if prefill_receipts {
        // Exercise the actual ingress preparation before filling the remaining
        // general slots, rather than inventing reserved cooldown records.
        fill_storage(&service, instance_id).await;
    }
    if !prefill_receipts {
        // Slack keeps a channel's ID when it becomes private after setup.
        slack.private.store(true, Ordering::SeqCst);
        let mut converted = envelope.clone();
        converted["event"]["ts"] = json!(format!("{}.000003", now() / 1000));
        converted["event"]["text"] = json!("<@UBOT> Private content must not be imported");
        assert_eq!(
            ingress(&app, instance_id, converted, SIGNING_SECRET)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert!(
            service
                .store
                .list("plugin-events")
                .await
                .unwrap()
                .is_empty()
        );
        slack.private.store(false, Ordering::SeqCst);
    }
    assert_eq!(
        ingress(&app, instance_id, envelope.clone(), SIGNING_SECRET).await,
        (StatusCode::OK, "accepted".into())
    );
    assert_eq!(
        ingress(&app, instance_id, envelope.clone(), SIGNING_SECRET).await,
        (StatusCode::OK, "duplicate".into())
    );

    if rate_limited {
        let another_resource = save(&app, &base, json!({"name": "Same workspace, another channel", "settings": {"channelId": "CSECOND"}, "secrets": {"botToken": BOT_TOKEN}})).await;
        save(&app, &format!("{base}/{}/routes", text(&another_resource, "id")), json!({
            "name": "Second authorized channel", "enabled": true, "match": {"channel": "CSECOND"}, "agentId": agent["id"],
            "accessMode": "allowlist", "userIds": ["UALICE"], "maxMessagesPerHour": 10, "maxPerPersonPerHour": 5,
        })).await;
        let mut second = envelope.clone();
        second["event"]["channel"] = json!("CSECOND");
        second["event"]["ts"] = json!(format!("{}.000004", now() / 1000));
        second["event"]["thread_ts"] = json!(format!("{}.000003", now() / 1000));
        second["event"]["text"] = json!("<@UBOT> A second independent request");
        assert_eq!(
            ingress(&app, instance_id, second, SIGNING_SECRET).await,
            (StatusCode::OK, "accepted".into())
        );
    }

    if matches!(scenario, DeliveryScenario::ChannelCadence) {
        for index in 2..=3 {
            let mut next = envelope.clone();
            next["event"]["ts"] = json!(format!("{}.{index:06}", now() / 1000));
            next["event"]["text"] = json!(format!("<@UBOT> Independent thread {index}"));
            assert_eq!(
                ingress(&app, instance_id, next, SIGNING_SECRET).await,
                (StatusCode::OK, "accepted".into())
            );
        }
        // Hold only the first reply's metadata lookup until all three real
        // provider turns have completed. No completed answer is inserted in DB.
        slack.hold_reply_check.store(true, Ordering::SeqCst);
    }

    // The separate real manager launches runner-client from its own executable,
    // and the fixture controller runs the native chat adapter + actual Node process.
    std::fs::write(
        config.data_dir.join("storage-s3.json"),
        json!({"bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1"}).to_string(),
    )
    .unwrap();
    config.worker_enabled = true;
    let file = root.path().join("config.json");
    std::fs::write(&file, serde_json::to_vec(&config).unwrap()).unwrap();
    if matches!(scenario, DeliveryScenario::PrivateBeforeReply) {
        // Admission was public and authorized. Visibility changes before the
        // real manager starts the actual turn, so only the export must fail.
        slack.private.store(true, Ordering::SeqCst);
    }
    let mut manager = start_manager(&file, &slack_origin).await;
    if matches!(scenario, DeliveryScenario::LongReplyRestart) {
        common::eventually(
            Duration::from_secs(20),
            Duration::from_millis(10),
            async || {
                let checkpoints = service
                    .store
                    .list("plugin-delivery-checkpoints")
                    .await
                    .unwrap();
                (checkpoints.iter().any(|row| {
                    row["state"]["status"] == "retryable" && row["state"]["nextChunk"] == 2
                }))
                .then_some(())
            },
        )
        .await;
        stop_manager(&mut manager).await;
        assert_eq!(slack.posts.lock().await.len(), 1);
        manager = start_manager(&file, &slack_origin).await;
    }
    if matches!(scenario, DeliveryScenario::LongFailedReplyAdvanced) {
        common::eventually(
            Duration::from_secs(20),
            Duration::from_millis(20),
            async || {
                let checkpoints = service
                    .store
                    .list("plugin-delivery-checkpoints")
                    .await
                    .unwrap();
                (slack.post_attempts.load(Ordering::SeqCst) == 2
                    && checkpoints.iter().any(|row| {
                        row["state"]["status"] == "retryable" && row["state"]["nextChunk"] == 2
                    }))
                .then_some(())
            },
        )
        .await;
        let mut next = envelope.clone();
        next["event"]["ts"] = json!(format!("{}.000007", now() / 1000));
        next["event"]["thread_ts"] = json!(ts);
        next["event"]["text"] = json!("<@UBOT> SECOND_AFTER_FAILED fixture:short-final");
        assert_eq!(
            ingress(&app, instance_id, next, SIGNING_SECRET).await,
            (StatusCode::OK, "accepted".into())
        );
        common::eventually(
            Duration::from_secs(20),
            Duration::from_millis(20),
            async || {
                let abandoned = service
                    .store
                    .read(|db| db.keys("plugin-turn-abandoned:"))
                    .await
                    .unwrap();
                (!abandoned.is_empty() && controller.plans.lock().await.len() == 2).then_some(())
            },
        )
        .await;
        let old_id = service
            .store
            .list("plugin-delivery-checkpoints")
            .await
            .unwrap()
            .into_iter()
            .find(|row| row["state"]["nextChunk"] == 2)
            .unwrap()["deliveryId"]
            .clone();
        let old = service
            .store
            .list(work::DELIVERIES)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row["id"] == old_id)
            .unwrap();
        let root_connection = connections::get(&service, "slack", instance_id)
            .await
            .unwrap();
        let pending_reply = work::reply(&service, &root_connection, &old)
            .await
            .unwrap()
            .unwrap();
        if pending_reply.text != expected_answer {
            stop_manager(&mut manager).await;
            panic!(
                "The failed turn's final reply changed after the next Slack request advanced: {}",
                pending_reply.text
            );
        }
        // Advance only the simulated REST cooldown and delivery clock. Both agent
        // turns and their completion events are produced by the real adapter/process.
        service
            .store
            .transaction(|db| {
                for mut row in db.list("plugin-storage")? {
                    if text(&row, "key").ends_with(":chat.postMessage") {
                        row["value"] = json!(0);
                        db.put("plugin-storage", &row)?;
                    }
                }
                for mut row in db.list("plugin-deliveries")? {
                    row["nextAttemptAt"] = json!(now());
                    db.put("plugin-deliveries", &row)?;
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    if matches!(scenario, DeliveryScenario::Normal) {
        let key = format!("plugin-status:{instance_id}");
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status_key = key.clone();
                let state = service
                    .store
                    .read(move |db| db.kv(&status_key))
                    .await
                    .unwrap();
                if state.is_some_and(|value| value["state"] == "connected") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if ready.is_err() {
            stop_manager(&mut manager).await;
        }
        assert!(ready.is_ok(), "working HTTP Slack app remained Connecting");
    }
    if matches!(scenario, DeliveryScenario::ChannelCadence) {
        tokio::time::timeout(Duration::from_secs(20), slack.channel_entered.notified())
            .await
            .unwrap();
        common::eventually(
            Duration::from_secs(20),
            Duration::from_millis(20),
            async || {
                let runs = service
                    .store
                    .read(|db| db.runs(None, None, 20, 0, true))
                    .await
                    .unwrap();
                (runs.len() == 3 && runs.iter().all(|run| run["status"] == "succeeded"))
                    .then_some(())
            },
        )
        .await;
        slack.release_channel.notify_one();
        common::eventually(
            Duration::from_secs(15),
            Duration::from_millis(10),
            async || (slack.posts.lock().await.len() >= 2).then_some(()),
        )
        .await;
        let times = slack.post_times.lock().await.clone();
        let gap = times[1].duration_since(times[0]);
        if gap < Duration::from_secs(1) {
            stop_manager(&mut manager).await;
            panic!("two ready Slack threads posted only {gap:?} apart");
        }
        common::eventually(
            Duration::from_secs(5),
            Duration::from_millis(10),
            async || {
                (service
                    .store
                    .list("plugin-activity")
                    .await
                    .unwrap()
                    .iter()
                    .filter(|row| row["status"] == "replied")
                    .count()
                    == 2)
                    .then_some(())
            },
        )
        .await;
        stop_manager(&mut manager).await;
        assert_eq!(
            slack.posts.lock().await.len(),
            2,
            "third reply ignored channel pacing"
        );
        let mut pending = service.store.list(work::DELIVERIES).await.unwrap();
        assert_eq!(pending.len(), 1);
        pending[0]["nextAttemptAt"] = json!(now());
        service
            .store
            .put(work::DELIVERIES, pending.remove(0))
            .await
            .unwrap();
        let mut fresh = start_manager(&file, &slack_origin).await;
        common::eventually(
            Duration::from_secs(10),
            Duration::from_millis(20),
            async || {
                (service
                    .store
                    .list("plugin-activity")
                    .await
                    .unwrap()
                    .iter()
                    .filter(|row| row["status"] == "replied")
                    .count()
                    == 3)
                    .then_some(())
            },
        )
        .await;
        stop_manager(&mut fresh).await;
        let times = slack.post_times.lock().await;
        assert_eq!(times.len(), 3);
        assert!(
            times[2].duration_since(times[1]) >= Duration::from_secs(1),
            "fresh worker lost channel pacing"
        );
        let posts = slack.posts.lock().await;
        let threads: std::collections::HashSet<_> = posts
            .iter()
            .map(|post| post["thread_ts"].as_str())
            .collect();
        assert_eq!(threads.len(), 3);
        service.shutdown.cancel();
        slack_server.abort();
        controller_server.abort();
        return;
    }
    if matches!(
        scenario,
        DeliveryScenario::PrivateBeforeReply | DeliveryScenario::UncertainRestart
    ) {
        let pending = common::eventually(
            Duration::from_secs(30),
            Duration::from_millis(30),
            async || {
                service
                    .store
                    .list("plugin-deliveries")
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|row| row["attempts"].as_u64().unwrap_or(0) >= 1)
            },
        )
        .await;
        stop_manager(&mut manager).await;
        let runs = service
            .store
            .read(|db| db.runs(None, None, 20, 0, true))
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["status"], "succeeded");
        if matches!(scenario, DeliveryScenario::PrivateBeforeReply) {
            assert_eq!(slack.post_attempts.load(Ordering::SeqCst), 0);
            assert!(slack.posts.lock().await.is_empty());
        } else {
            assert_eq!(slack.post_attempts.load(Ordering::SeqCst), 1);
            assert_eq!(
                slack.posts.lock().await.len(),
                1,
                "the provider committed before returning an error"
            );
            let checkpoints = service
                .store
                .list("plugin-delivery-checkpoints")
                .await
                .unwrap();
            assert_eq!(checkpoints.len(), 1);
            assert_eq!(checkpoints[0]["state"]["status"], "sending");
            let previous_attempts = pending["attempts"].as_u64().unwrap();
            let mut due = pending;
            due["nextAttemptAt"] = json!(now());
            service.store.put("plugin-deliveries", due).await.unwrap();
            // New process + fresh Luau worker read the durable write intent.
            let mut restarted = start_manager(&file, &slack_origin).await;
            common::eventually(
                Duration::from_secs(10),
                Duration::from_millis(30),
                async || {
                    service
                        .store
                        .list("plugin-deliveries")
                        .await
                        .unwrap()
                        .iter()
                        .any(|row| row["attempts"].as_u64().unwrap_or(0) > previous_attempts)
                        .then_some(())
                },
            )
            .await;
            stop_manager(&mut restarted).await;
            assert_eq!(
                slack.post_attempts.load(Ordering::SeqCst),
                1,
                "uncertain post repeated after restart"
            );
            assert_eq!(slack.posts.lock().await.len(), 1);
        }
        service.shutdown.cancel();
        slack_server.abort();
        controller_server.abort();
        return;
    }
    if rate_limited {
        // Both real turns complete. The first outbound attempt receives 900 s;
        // the other delivery must share that method/workspace cooldown.
        let deliveries = common::eventually(
            Duration::from_secs(30),
            Duration::from_millis(30),
            async || {
                let rows = service.store.list("plugin-deliveries").await.unwrap();
                let checkpoints = service
                    .store
                    .list("plugin-delivery-checkpoints")
                    .await
                    .unwrap();
                (rows.len() == 2
                    && checkpoints.len() == 2
                    && rows
                        .iter()
                        .all(|row| row["nextAttemptAt"].as_i64().unwrap_or(0) > now() + 1000)
                    && checkpoints
                        .iter()
                        .all(|row| row["state"]["status"] == "retryable"))
                .then_some(rows)
            },
        )
        .await;
        assert_eq!(slack.post_attempts.load(Ordering::SeqCst), 1);
        let cooldown = service
            .store
            .list("plugin-storage")
            .await
            .unwrap()
            .into_iter()
            .find(|row| {
                row["key"]
                    .as_str()
                    .is_some_and(|key| key.ends_with(":chat.postMessage"))
            })
            .unwrap();
        assert!(cooldown["value"].as_i64().unwrap() >= now() / 1000 + 895);
        assert!(
            deliveries
                .iter()
                .all(|row| row["nextAttemptAt"].as_i64().unwrap() <= now() + 301000)
        );
        // Exercise an early due scheduler pass without sleeping for 900 seconds.
        // Only scheduling timestamps change; no run completion or answer is fabricated.
        let forced_at = now();
        service
            .store
            .transaction(move |db| {
                for original in deliveries {
                    let mut row = db.get("plugin-deliveries", text(&original, "id"))?.unwrap();
                    row["nextAttemptAt"] = json!(forced_at);
                    db.put("plugin-deliveries", &row)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        common::eventually(
            Duration::from_secs(10),
            Duration::from_millis(30),
            async || {
                let rows = service.store.list("plugin-deliveries").await.unwrap();
                rows.iter()
                    .all(|row| row["nextAttemptAt"].as_i64().unwrap() > forced_at + 200000)
                    .then_some(())
            },
        )
        .await;
        assert_eq!(
            slack.post_attempts.load(Ordering::SeqCst),
            1,
            "a delayed or different delivery called Slack early"
        );
        assert!(slack.posts.lock().await.is_empty());
        stop_manager(&mut manager).await;
        service.shutdown.cancel();
        slack_server.abort();
        controller_server.abort();
        return;
    }

    let result = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let post = slack.posts.lock().await.first().cloned();
            let settled = !long_reply
                || service
                    .store
                    .list("plugin-deliveries")
                    .await
                    .unwrap()
                    .is_empty();
            if let Some(post) = post
                && settled
            {
                break post;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    if result.is_err() {
        eprintln!(
            "runs: {:?}; events: {:?}; statuses: {:?}",
            service
                .store
                .read(|db| db.runs(None, None, 20, 0, true))
                .await,
            service.store.list("plugin-events").await,
            service.store.list("plugin-status").await
        );
    }
    if result.is_ok() {
        common::eventually(
            Duration::from_secs(10),
            Duration::from_millis(25),
            async || {
                let activities = service.store.list("plugin-activity").await.unwrap();
                activities
                    .iter()
                    .any(|activity| activity["status"] == "replied")
                    .then_some(())
            },
        )
        .await;
    }
    stop_manager(&mut manager).await;
    let post = result.expect("the actual manager/provider/worker never delivered Slack's answer");
    assert_eq!(post["channel"], "CTEST");
    assert_eq!(post["thread_ts"], ts);
    assert!(
        post["text"].contains("I reviewed the workspace."),
        "{post:?}"
    );
    assert!(post["text"].contains("Explain the Slack tracer"));
    let posts = slack.posts.lock().await;
    if long_reply {
        assert!(posts.len() > 1, "the answer was not chunked");
        assert!(posts.iter().all(|part| part["channel"] == "CTEST"
            && part["thread_ts"] == ts
            && part["text"].len() <= 3000));
        let joined: String = posts
            .iter()
            .filter(|part| !part["text"].contains("SECOND_AFTER_FAILED"))
            .map(|part| part["text"].as_str())
            .collect();
        let expected = expected_answer
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        assert_eq!(
            joined, expected,
            "the first answer lost, repeated or mixed chunks"
        );
        let escaped = prompt
            .trim()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        assert_eq!(
            joined.matches(&escaped).count(),
            1,
            "answer content missing or repeated"
        );
        assert!(!joined.contains('<') && !joined.contains('>'));
        assert!(
            joined.ends_with("Ready for the next step."),
            "the final part is missing"
        );
        let attempts = slack.post_attempts.load(Ordering::SeqCst);
        assert_eq!(
            attempts,
            posts.len()
                + usize::from(matches!(
                    scenario,
                    DeliveryScenario::LongReplyRateLimited
                        | DeliveryScenario::LongFailedReplyAdvanced
                ))
        );
    } else {
        assert_eq!(posts.len(), 1);
    }
    drop(posts);
    assert!(
        service
            .store
            .list("plugin-delivery-checkpoints")
            .await
            .unwrap()
            .is_empty(),
        "settled delivery state leaked"
    );
    let runs = service
        .store
        .read(|db| db.runs(None, None, 20, 0, true))
        .await
        .unwrap();
    assert_eq!(runs.len(), 1, "a denied author or duplicate started work");
    assert_eq!(runs[0]["status"], "succeeded");
    assert_eq!(runs[0]["snapshot"]["agent"]["id"], agent["id"]);
    assert_eq!(
        runs[0]["snapshot"]["agent"]["access"]["installation"],
        false
    );
    let plans = controller.plans.lock().await;
    assert_eq!(
        plans.len(),
        if matches!(scenario, DeliveryScenario::LongFailedReplyAdvanced) {
            2
        } else {
            1
        }
    );
    assert!(
        plans[0]["chat"]["execution"]["text"]
            .as_str()
            .unwrap()
            .contains("Explain the Slack tracer")
    );
    let principals = service
        .store
        .read(|db| db.keys("plugin-message-requester:"))
        .await
        .unwrap();
    assert!(!principals.is_empty(), "no native requester binding");
    let principal = &principals[0].1;
    assert_eq!(principal["subject"], "UALICE");
    assert_eq!(principal["connectionId"], instance_id);
    assert_eq!(principal["binding"]["id"], route["id"]);
    assert_eq!(principal["binding"]["resource"], resource["id"]);
    service.shutdown.cancel();
    slack_server.abort();
    controller_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_concurrent_ingress_cannot_erase_a_newer_workspace_cooldown() {
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
    let (instance, _resource) = configure(&app, &service).await;
    let id = text(&instance, "id").to_owned();
    let key = "cooldown:TTEST:conversations.info";
    service
        .store
        .put(
            "plugin-storage",
            json!({
                "id": format!("storage:{id}:{}", cairn_installation::auth::hex_digest(key)),
                "connectionId": id, "key": key, "value": now() / 1000 - 1, "updatedAt": now(),
            }),
        )
        .await
        .unwrap();
    let envelope = json!({"type": "event_callback", "api_app_id": "ATEST", "team_id": "TTEST",
        "event": {"type": "app_mention", "user": "UALICE", "channel": "CTEST", "ts": format!("{}.000001", now() / 1000), "text": "<@UBOT> A delayed metadata check"}});
    slack.cooldown_race.store(true, Ordering::SeqCst);
    let first_app = app.clone();
    let first_id = id.clone();
    let first_envelope = envelope.clone();
    let first = tokio::spawn(async move {
        ingress(&first_app, &first_id, first_envelope, SIGNING_SECRET).await
    });
    tokio::time::timeout(Duration::from_secs(2), slack.channel_entered.notified())
        .await
        .unwrap();
    let mut second = envelope.clone();
    second["event"]["ts"] = json!(format!("{}.000002", now() / 1000));
    assert_eq!(
        ingress(&app, &id, second, SIGNING_SECRET).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    slack.release_channel.notify_one();
    assert_eq!(first.await.unwrap(), (StatusCode::OK, "denied".into()));
    let mut third = envelope;
    third["event"]["ts"] = json!(format!("{}.000003", now() / 1000));
    assert_eq!(
        ingress(&app, &id, third, SIGNING_SECRET).await.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "a success erased a newer cooldown and called the provider again"
    );
    assert_eq!(slack.channel_attempts.load(Ordering::SeqCst), 2);
    assert!(
        service
            .store
            .list("plugin-events")
            .await
            .unwrap()
            .is_empty()
    );
    service.shutdown.cancel();
    server.abort();
}
