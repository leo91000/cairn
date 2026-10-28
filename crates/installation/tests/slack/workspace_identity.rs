//! A saved workspace keeps its verified external namespace and its access rules.
use crate::common::AgentFixture;

use super::*;

const OTHER_TOKEN: &str = "xoxb-synthetic-other-workspace";
const ROTATED_TOKEN: &str = "xoxb-synthetic-rotated-original-workspace";

async fn workspace_api(
    State(slack): State<Arc<Slack>>,
    mut headers: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    let token = headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap();
    let team = match token {
        OTHER_TOKEN => "TOTHER",
        BOT_TOKEN | ROTATED_TOKEN => "TTEST",
        _ => panic!("Unexpected synthetic workspace credential"),
    };
    if request.uri().path() == "/api/auth.test" {
        return Json(json!({"ok": true, "team_id": team, "user_id": "UBOT", "bot_id": "BBOT"}))
            .into_response();
    }
    // Both synthetic workspace tokens belong to ATEST. Other provider methods
    // retain the existing fixture's app/channel assertions after token checking.
    headers.insert(
        "authorization",
        format!("Bearer {BOT_TOKEN}").parse().unwrap(),
    );
    api(State(slack), headers, request).await
}

#[tokio::test]
async fn a_saved_workspace_cannot_transfer_its_resource_and_routes_to_another_team() {
    workspace_identity(false).await;
}

#[tokio::test]
async fn a_prepared_workspace_keeps_its_verified_team_before_activation() {
    workspace_identity(true).await;
}

async fn workspace_identity(inert: bool) {
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let slack = Arc::new(Slack::default());
    let provider = Router::new()
        .route("/api/{method}", post(workspace_api).get(workspace_api))
        .with_state(slack);
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let (instance, resource, resources) = if inert {
        let draft = save(
            &app,
            "/api/plugins/slack/setup-drafts",
            json!({
                "name": "Prepared app",
                "settings": {
                    "appId": "ATEST", "transport": "http",
                    "sharingAcknowledged": true, "maxRequestAgeMinutes": 15,
                },
                "secrets": {"signingSecret": SIGNING_SECRET},
            }),
        )
        .await;
        let resources = format!(
            "/api/plugins/slack/setup-drafts/{}/resources/workspace",
            text(&draft, "id")
        );
        let resource = save(
            &app,
            &resources,
            json!({
                "setupRevision": draft["revision"], "name": "Prepared workspace",
                "settings": {"channelId": "CTEST"}, "secrets": {"botToken": BOT_TOKEN},
            }),
        )
        .await;
        (draft, resource, resources)
    } else {
        let (instance, resource) = configure(&app, &service).await;
        let resources = format!(
            "/api/plugins/slack/connections/{}/resources/workspace",
            text(&instance, "id")
        );
        (instance, resource, resources)
    };
    let path = format!("{resources}/{}", text(&resource, "id"));
    let current = common::send(&app, owner("GET", &path, &json!({}))).await;
    assert_eq!(current.status(), StatusCode::OK);
    let current = common::read_json(current).await;
    let routes = format!("{path}/routes");
    let before_routes = if !inert {
        let agent = service
            .agent_fixture(
                json!({
                    "name": "Original workspace",
                    "access": {"projects": [], "skills": [], "mcps": [], "installation": false},
                }),
                None,
            )
            .await
            .unwrap();
        save(
            &app,
            &routes,
            json!({
                "name": "Original people", "enabled": true, "match": {},
                "agentId": agent["id"], "accessMode": "allowlist", "userIds": ["UALICE"],
                "maxMessagesPerHour": 10, "maxPerPersonPerHour": 5,
            }),
        )
        .await;
        let saved =
            common::read_json(common::send(&app, owner("GET", &routes, &json!({}))).await).await;
        assert_eq!(saved["items"].as_array().unwrap().len(), 1);
        Some(saved)
    } else {
        None
    };
    let secret_key = cairn_installation::plugins::resources::secrets_key(text(&resource, "id"));
    let before_secret = service.vault.get(&secret_key).await.unwrap();
    assert!(before_secret.is_some());
    for forged_id in [None, Some("TOTHER")] {
        let mut settings = json!({"channelId": "CSECOND"});
        if let Some(forged_id) = forged_id {
            settings["externalId"] = json!(forged_id);
        }
        let mut input = json!({
            "revision": current["revision"], "name": current["name"], "enabled": true,
            "settings": settings, "secrets": {"botToken": OTHER_TOKEN},
        });
        if inert {
            input["setupRevision"] = instance["revision"].clone();
        }
        let replacement = common::send(&app, owner("PUT", &path, &input)).await;
        assert_eq!(
            replacement.status(),
            StatusCode::BAD_REQUEST,
            "A different verified team reused the original resource ID"
        );
        let mut preview = json!({
            "resource": current["id"], "revision": current["revision"],
            "settings": input["settings"], "secrets": input["secrets"],
        });
        if inert {
            preview["setupRevision"] = instance["revision"].clone();
        }
        let preview = save(&app, &format!("{resources}/form"), preview).await;
        assert!(
            preview["errors"]["botToken"]
                .as_str()
                .is_some_and(|message| message.contains("another workspace"))
        );
        let unchanged =
            common::read_json(common::send(&app, owner("GET", &path, &json!({}))).await).await;
        assert_eq!(unchanged, current);
        if let Some(before_routes) = &before_routes {
            let after_routes =
                common::read_json(common::send(&app, owner("GET", &routes, &json!({}))).await)
                    .await;
            assert_eq!(&after_routes, before_routes);
        }
        assert!(service.vault.get(&secret_key).await.unwrap() == before_secret);
    }
    // Rotation within the original namespace remains valid. A second verified
    // workspace is supported through its own new resource, with no old routes.
    let mut rotation = json!({
        "revision": current["revision"], "name": current["name"], "enabled": true,
        "settings": {"channelId": "CTEST"}, "secrets": {"botToken": ROTATED_TOKEN},
    });
    if inert {
        rotation["setupRevision"] = instance["revision"].clone();
    }
    let rotated = common::send(&app, owner("PUT", &path, &rotation)).await;
    assert_eq!(rotated.status(), StatusCode::OK);
    assert_eq!(
        common::read_json(rotated).await["settings"]["externalId"],
        "TTEST"
    );
    assert!(
        service.vault.get(&secret_key).await.unwrap().unwrap()["fields"]["botToken"]
            == ROTATED_TOKEN
    );
    let mut addition = json!({
        "name": "Second workspace", "settings": {"channelId": "CSECOND"},
        "secrets": {"botToken": OTHER_TOKEN},
    });
    if inert {
        addition["setupRevision"] = instance["revision"].clone();
    }
    let added = save(&app, &resources, addition).await;
    assert_ne!(added["id"], current["id"]);
    assert_eq!(added["settings"]["externalId"], "TOTHER");
    if !inert {
        let route_path = format!("{resources}/{}/routes", text(&added, "id"));
        let fresh =
            common::read_json(common::send(&app, owner("GET", &route_path, &json!({}))).await)
                .await;
        assert!(fresh["items"].as_array().unwrap().is_empty());
    }
    service.shutdown.cancel();
    server.abort();
}
