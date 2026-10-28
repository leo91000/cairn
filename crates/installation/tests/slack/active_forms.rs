use super::*;

#[tokio::test]
async fn a_saved_http_app_keeps_its_generated_event_configuration() {
    active_http_form(false).await;
}

#[tokio::test]
async fn a_saved_socket_app_can_preview_its_http_event_configuration() {
    active_http_form(true).await;
}

async fn active_http_form(initial_socket_mode: bool) {
    let root = TempDir::new().unwrap();
    let service = management::service(&root).await;
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let provider = Router::new()
        .route("/api/{method}", post(api).get(api))
        .with_state(Arc::new(Slack::default()));
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let (instance, _) = configure_transport(&app, &service, initial_socket_mode).await;
    let main = management::enabled_main(&service).await;
    let (_, ungranted) = management::active_run(&service, &main).await;
    let denied = management::call(
        &service,
        &ungranted,
        "manage_plugin",
        json!({
            "pluginId": "slack",
            "instanceId": instance["id"],
            "action": "form",
            "data": {"settings": instance["settings"]},
        }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    assert!(
        text(&denied["result"]["content"][0], "text")
            .contains("Installation management does not grant access to this plugin account")
    );
    let granted = service
        .agent_fixture(
            json!({
                "name": "Saved Slack form helper",
                "access": {"installation": true},
                "plugins": [instance["id"]],
            }),
            None,
        )
        .await
        .unwrap();
    let (_, configuration) = management::active_run(&service, &granted).await;

    if initial_socket_mode {
        let form = save(
            &app,
            "/api/plugins/slack/form",
            json!({
                "instance": instance["id"],
                "settings": instance["settings"],
            }),
        )
        .await;
        let manifest = app_manifest(&form);
        assert_eq!(manifest["settings"]["socket_mode_enabled"], true);
        assert_eq!(
            manifest["settings"]["event_subscriptions"]["bot_events"],
            json!(["app_mention"])
        );
    }

    let mut settings = instance["settings"].clone();
    settings["transport"] = json!("http");
    let mut expected = None;
    for source in ["http", "public", "installation"] {
        let form = if source == "http" {
            save(
                &app,
                "/api/plugins/slack/form",
                json!({
                    "instance": instance["id"],
                    "settings": settings,
                }),
            )
            .await
        } else {
            let args = json!({
                "pluginId": "slack",
                "instanceId": instance["id"],
                "action": "form",
                "data": {"settings": settings},
            });
            let response = if source == "public" {
                management::public_call(&service, &["manage"], "manage_plugin", args).await
            } else {
                management::call(&service, &configuration, "manage_plugin", args).await
            };
            management::result(&response).clone()
        };
        let manifest = app_manifest(&form);
        let subscriptions = &manifest["settings"]["event_subscriptions"];
        let event_url = text(subscriptions, "request_url");
        assert_eq!(manifest["settings"]["socket_mode_enabled"], false);
        assert_eq!(subscriptions["bot_events"], json!(["app_mention"]));
        assert!(event_url.contains("/api/public/installations/"));
        assert!(event_url.ends_with(&format!("/plugins/{}/events", text(&instance, "id"))));
        assert_eq!(manifest["settings"]["interactivity"]["is_enabled"], true);
        assert_eq!(
            manifest["settings"]["interactivity"]["request_url"],
            event_url
        );
        if let Some(expected) = &expected {
            assert_eq!(&manifest, expected, "Manifest differs through {source}");
        } else {
            expected = Some(manifest);
        }
    }
    server.abort();
}
