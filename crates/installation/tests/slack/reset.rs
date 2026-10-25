//! Real package controls, with the parent's actual manager and provider process.
use super::*;

pub(super) async fn control(
    app: &Router,
    instance: &str,
    payload: &Value,
    secret: &str,
) -> (StatusCode, String) {
    let body = serde_urlencoded::to_string([("payload", payload.to_string())]).unwrap();
    let timestamp = (now() / 1000).to_string();
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let signature =
        hex::encode(hmac::sign(&key, format!("v0:{timestamp}:{body}").as_bytes()).as_ref());
    let request = common::request("POST", &format!("/api/plugin-webhooks/{instance}/events"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("x-slack-request-timestamp", timestamp)
        .header("x-slack-signature", format!("v0={signature}"))
        .body(Body::from(body))
        .unwrap();
    let response = common::send(app, request).await;
    (response.status(), common::read_text(response).await)
}

pub(super) async fn round_trip(
    app: &Router,
    service: &Service,
    slack: &Arc<Slack>,
    instance: &str,
    envelope: &Value,
    post: &HashMap<String, String>,
) {
    let blocks: Value = serde_json::from_str(
        post.get("blocks")
            .expect("Slack answer has no reset control"),
    )
    .unwrap();
    assert_eq!(
        blocks[0]["text"]["text"], post["text"],
        "the control hid the actual answer"
    );
    let button = &blocks[1]["elements"][0];
    assert_eq!(button["action_id"], "cairn_reset");
    let old_chat = button["value"].as_str().unwrap().to_owned();
    let owner_key = format!("plugin-conversation:{old_chat}");
    common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || {
            service
                .store
                .list(work::DELIVERIES)
                .await
                .unwrap()
                .is_empty()
                .then_some(())
        },
    )
    .await;
    // Reset is a control, not new work: native saturation must not block it.
    let current = service
        .store
        .get(connections::KIND, instance)
        .await
        .unwrap()
        .unwrap();
    let occupied = capacity::occupy(service, &current).await;
    let ts = text(&envelope["event"], "ts");
    let action_ts = format!("{}.000100", now() / 1000);
    let message_ts = slack.post_ids.lock().await[text(&blocks[1], "block_id")].clone();
    let payload = json!({
        "type": "block_actions", "api_app_id": "ATEST", "team": {"id": "TTEST"},
        "user": {"id": "UALICE"}, "channel": {"id": "CTEST"},
        "container": {"type": "message", "channel_id": "CTEST", "message_ts": message_ts},
        "message": {"user": "UBOT", "app_id": "ATEST", "ts": message_ts, "thread_ts": ts},
        "actions": [{"type": "button", "action_id": "cairn_reset", "block_id": blocks[1]["block_id"], "value": old_chat, "action_ts": action_ts}],
        "response_url": "https://hooks.slack.com/actions/SYNTHETIC_DO_NOT_STORE",
        "state": {"values": {"unrelated": "PRIVATE_SYNTHETIC_STATE"}},
    });
    assert_eq!(
        control(app, instance, &payload, "wrong-synthetic-key")
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for (field, value, status) in [
        ("/api_app_id", json!("AOTHER"), StatusCode::FORBIDDEN),
        ("/team/id", json!("TOTHER"), StatusCode::FORBIDDEN),
        ("/channel/id", json!("COTHER"), StatusCode::BAD_REQUEST),
        ("/message/user", json!("UOTHER"), StatusCode::FORBIDDEN),
        (
            "/actions/0/value",
            json!("invalid-conversation"),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut invalid = payload.clone();
        *invalid.pointer_mut(field).unwrap() = value;
        assert_eq!(
            control(app, instance, &invalid, SIGNING_SECRET).await.0,
            status,
            "{field}"
        );
    }
    let mut denied = payload.clone();
    denied["user"]["id"] = json!("UMALLORY");
    assert_eq!(
        control(app, instance, &denied, SIGNING_SECRET).await,
        (StatusCode::OK, "denied".into())
    );
    for code in ["internal_error", "service_unavailable", "ratelimited"] {
        slack
            .metadata_errors
            .lock()
            .await
            .push_back(code.to_owned());
        assert_eq!(
            control(app, instance, &payload, SIGNING_SECRET).await.0,
            StatusCode::SERVICE_UNAVAILABLE,
            "{code}"
        );
        assert!(service.store.list(work::EVENTS).await.unwrap().is_empty());
        let key = owner_key.clone();
        assert!(
            service
                .store
                .read(move |db| Ok(db.kv(&key)?.unwrap()["retiredAt"].is_null()))
                .await
                .unwrap()
        );
        if code == "ratelimited" {
            tokio::time::sleep(Duration::from_millis(2100)).await;
        }
    }
    let started = Instant::now();
    assert_eq!(
        control(app, instance, &payload, SIGNING_SECRET).await,
        (StatusCode::OK, "accepted".into())
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "interaction acknowledgement exceeded Slack's deadline"
    );
    common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || {
            let key = owner_key.clone();
            service
                .store
                .read(move |db| {
                    Ok(db
                        .kv(&key)?
                        .is_some_and(|owner| !owner["retiredAt"].is_null()))
                })
                .await
                .unwrap()
                .then_some(())
        },
    )
    .await;
    assert_eq!(
        control(app, instance, &payload, SIGNING_SECRET).await,
        (StatusCode::OK, "duplicate".into())
    );

    capacity::release(service, occupied).await;

    let mut next = envelope.clone();
    next["event"]["ts"] = json!(format!("{}.000009", now() / 1000));
    next["event"]["thread_ts"] = json!(ts);
    next["event"]["text"] = json!("<@UBOT> A new conversation fixture:short-final");
    assert_eq!(
        ingress(app, instance, next, SIGNING_SECRET).await,
        (StatusCode::OK, "accepted".into())
    );
    let new_chat = common::eventually(
        Duration::from_secs(20),
        Duration::from_millis(20),
        async || {
            let chats = service.store.list("chats").await.unwrap();
            (chats.len() == 2
                && slack.posts.lock().await.len() == 2
                && service
                    .store
                    .list(work::DELIVERIES)
                    .await
                    .unwrap()
                    .is_empty())
            .then(|| {
                chats
                    .into_iter()
                    .find(|chat| chat["id"] != old_chat)
                    .unwrap()
            })
        },
    )
    .await;
    let new_id = text(&new_chat, "id").to_owned();
    assert!(
        service
            .store
            .get("chats", &old_chat)
            .await
            .unwrap()
            .is_some(),
        "reset erased history"
    );
    let mut old_button = payload.clone();
    old_button["actions"][0]["action_ts"] = json!(format!("{}.000200", now() / 1000));
    assert_eq!(
        control(app, instance, &old_button, SIGNING_SECRET).await,
        (StatusCode::OK, "accepted".into())
    );
    common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || {
            service
                .store
                .list("plugin-events")
                .await
                .unwrap()
                .iter()
                .any(|event| event["attempts"].as_u64().unwrap_or(0) > 0)
                .then_some(())
        },
    )
    .await;
    let pending =
        serde_json::to_string(&service.store.list("plugin-events").await.unwrap()).unwrap();
    assert!(
        !pending.contains("SYNTHETIC_DO_NOT_STORE") && !pending.contains("PRIVATE_SYNTHETIC_STATE"),
        "unrelated sensitive payload was retained"
    );
    let new_owner_key = format!("plugin-conversation:{new_id}");
    assert!(
        service
            .store
            .read(move |db| Ok(db.kv(&new_owner_key)?.unwrap()["retiredAt"].is_null()))
            .await
            .unwrap(),
        "old control retired its successor"
    );
    let runs = service
        .store
        .read(|db| db.runs(None, None, 20, 0, true))
        .await
        .unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run["status"] == "succeeded"
        && run["snapshot"]["agent"]["access"]["installation"] == false));
    let posts = slack.posts.lock().await;
    assert!(
        posts
            .iter()
            .all(|post| post["channel"] == "CTEST" && post["thread_ts"] == ts)
    );
}
