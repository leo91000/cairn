//! Admission fixtures use the real package and native authority; no work is completed synthetically.
use super::*;

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
