mod common;

use axum::{http::StatusCode, response::IntoResponse};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    plugins::{connections, registry, resources},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
use tempfile::TempDir;

fn owner_request(method: &str, path: &str, body: Value) -> axum::extract::Request {
    let mut request = common::request(method, path)
        .header("content-type", "application/json")
        .body(axum::Json(body).into_response().into_body())
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    request
}

async fn fixture() -> (TempDir, axum::Router, String, Value) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = "http://runner:4311".to_owned();
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let scopes: Vec<_> = (0..8).map(|n| format!("scope{n}")).collect();
    let bindings = scopes
        .iter()
        .map(|key| (key.clone(), json!(key)))
        .collect::<serde_json::Map<_, _>>();
    let installed = registry::install(&s, &json!({
        "manifest": {
            "id": "management-pages", "name": "Management pages", "version": "1", "apiVersion": 2,
            "capabilities": ["resources", "http", "conversations.submit"], "hosts": ["api.example.test"],
            "agentAccess": {},
            "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace", "delegation": true}],
            "operations": [{"name": "read", "collection": "workspace", "profile": "read", "scopes": scopes,
                "requests": [{"method": "GET", "host": "api.example.test", "path": "/messages", "bindings": bindings}]}],
        }, "source": "return {}",
    })).await.unwrap();
    registry::approve(&s, "management-pages", "1", text(&installed, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "management-pages",
        &json!({"name": "App", "enabled": true}),
        None,
    )
    .await
    .unwrap();
    let instance = text(&instance, "id");
    let resource = resources::save(
        &s,
        "management-pages",
        instance,
        "workspace",
        &json!({"name": "Engineering"}),
        None,
    )
    .await
    .unwrap();
    let path = format!(
        "/api/plugins/management-pages/connections/{instance}/resources/workspace/{}",
        text(&resource, "id")
    );
    let agent = s.agent(json!({"name": "Dedicated", "access": {"projects": [], "installation": false, "github": false}}), None).await.unwrap();
    let app = cairn_installation::http::router(Arc::clone(&s))
        .await
        .unwrap();
    (root, app, path, agent)
}

async fn read_pages(app: &axum::Router, path: &str, expected: &BTreeSet<String>) -> Vec<Value> {
    let mut cursor: Option<String> = None;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    let mut pages = 0;
    let mut largest = 0;
    loop {
        let url = cursor.as_ref().map_or_else(
            || path.to_owned(),
            |cursor| format!("{path}?cursor={cursor}"),
        );
        let response = common::send(app, owner_request("GET", &url, Value::Null)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            bytes.len() <= cairn_protocol::MAX_BODY,
            "A valid management page exceeds the relay limit: {} bytes",
            bytes.len()
        );
        let page: Value = serde_json::from_slice(&bytes).unwrap();
        pages += 1;
        largest = largest.max(bytes.len());
        let next = page["items"].as_array().unwrap();
        assert!(
            next.len() <= 32,
            "A management page was not bounded: {} records",
            next.len()
        );
        for value in next {
            assert!(
                seen.insert(text(value, "id").to_owned()),
                "An item was repeated across pages"
            );
            items.push(value.clone());
        }
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
        assert!(items.len() < expected.len(), "The list did not terminate");
    }
    assert_eq!(&seen, expected);
    eprintln!(
        "Listed {} records in {pages} pages; largest response {largest} bytes.",
        items.len()
    );
    items
}

#[tokio::test]
async fn large_allowed_routes_remain_listable_and_removable_within_the_relay_limit() {
    let (_root, app, resource, agent) = fixture().await;
    let path = format!("{resource}/routes");
    let users: Vec<_> = (0..100).map(|n| format!("{n:064}")).collect();
    let people: Vec<_> = users
        .iter()
        .map(|id| json!({"id": id,"name": "😀".repeat(100)}))
        .collect();
    let mut created = BTreeSet::new();
    for n in 0..256 {
        let input = json!({"name": format!("Channel {n}"), "enabled": true, "match": {"channel": format!("C{n}")},
            "agentId": agent["id"], "userIds": users, "people": people});
        assert!(input.to_string().len() < 150_000);
        let response = common::send(&app, owner_request("POST", &path, input)).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        let value = common::read_json(response).await;
        created.insert(text(&value, "id").to_owned());
    }
    let values = read_pages(&app, &path, &created).await;
    let last = values.last().unwrap();
    let response = common::send(
        &app,
        owner_request(
            "DELETE",
            &format!("{path}/{}", text(last, "id")),
            json!({"revision": last["revision"]}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
}

#[tokio::test]
async fn large_allowed_grants_remain_listable_and_revocable_within_the_relay_limit() {
    let (_root, app, resource, agent) = fixture().await;
    let path = format!("{resource}/grants");
    let scopes = (0..8)
        .map(|n| {
            (
                format!("scope{n}"),
                json!(
                    (0..100)
                        .map(|i| format!("{n}-{i:03}-{}", "x".repeat(172)))
                        .collect::<Vec<_>>()
                ),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let input = json!({"agentId": agent["id"], "profile": "read", "scopes": scopes});
    assert!(input.to_string().len() < 150_000);
    let mut created = BTreeSet::new();
    for _ in 0..64 {
        let response = common::send(&app, owner_request("POST", &path, input.clone())).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        let value = common::read_json(response).await;
        created.insert(text(&value, "id").to_owned());
    }
    let values = read_pages(&app, &path, &created).await;
    let last = values.last().unwrap();
    let response = common::send(
        &app,
        owner_request(
            "DELETE",
            &format!("{path}/{}", text(last, "id")),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
}

#[tokio::test]
async fn large_allowed_setup_drafts_remain_resumable_and_discardable_within_the_relay_limit() {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    // TOML literal strings remain compact on upload but JSON defaults expand.
    let mut manifest = String::from(
        "id = 'setup-pages'\nname = 'Setup pages'\nversion = '1'\napiVersion = 2\ncapabilities = []\n",
    );
    for n in 0..20 {
        manifest.push_str(&format!("\n[[settings]]\nkey = 'field{n}'\ntitle = 'Field {n}'\ntype = 'string'\nmaxLength = 10000\n"));
        if n < 6 {
            manifest.push_str(&format!("default = '{}'\n", "\"".repeat(10_000)));
        }
    }
    assert!(manifest.len() < 65_536);
    // Owners may copy a package into the installation (PLUGINS.md). Preserve its
    // TOML here: the upload endpoint converts it to a larger canonical JSON file.
    let package = s.config.data_dir.join("plugins/setup-pages/1");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("manifest.toml"), manifest).unwrap();
    std::fs::write(package.join("main.luau"), "return {}").unwrap();
    let app = cairn_installation::http::router(s).await.unwrap();
    let response = common::send(&app, owner_request("GET", "/api/plugins", Value::Null)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let catalog = common::read_json(response).await;
    let installed = &catalog["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["id"] == "setup-pages")
        .unwrap()["versions"][0];
    let response = common::send(
        &app,
        owner_request(
            "POST",
            "/api/plugins/packages/setup-pages/approve",
            json!({"version": "1", "digest": installed["digest"]}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let path = "/api/plugins/setup-pages/setup-drafts";
    let settings = (6..20)
        .map(|n| (format!("field{n}"), json!("x".repeat(10_000))))
        .collect::<serde_json::Map<_, _>>();
    let mut created = BTreeSet::new();
    for n in 0..32 {
        let input = json!({"name": format!("Setup {n}"), "settings": settings});
        assert!(input.to_string().len() < 150_000);
        let response = common::send(&app, owner_request("POST", path, input)).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        let draft = common::read_json(response).await;
        created.insert(text(&draft, "id").to_owned());
    }
    let drafts = read_pages(&app, path, &created).await;
    let last = drafts.last().unwrap();
    let target = format!("{path}/{}", text(last, "id"));
    let response = common::send(&app, owner_request("GET", &target, Value::Null)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let saved = common::read_json(response).await;
    for n in 0..20 {
        assert_eq!(
            saved["settings"][format!("field{n}")],
            if n < 6 {
                json!("\"".repeat(10_000))
            } else {
                json!("x".repeat(10_000))
            }
        );
    }
    // Both clients send the fully expanded settings when resaving a draft. A
    // valid persisted draft must remain editable even when defaults grew on read.
    let input = json!({
        "revision": saved["revision"],
        "name": "Renamed setup",
        "settings": saved["settings"],
    });
    assert!(serde_json::to_vec(&input).unwrap().len() > 150_000);
    let response = common::send(&app, owner_request("PATCH", &target, input)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let updated = common::read_json(response).await;
    assert_eq!(updated["name"], "Renamed setup");
    assert_eq!(updated["settings"], saved["settings"]);
    let response = common::send(
        &app,
        owner_request("DELETE", &target, json!({"revision": updated["revision"]})),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    assert_eq!(
        common::send(&app, owner_request("GET", &target, Value::Null))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn owner_configuration_rejects_settings_that_cannot_reach_the_worker() {
    let root = TempDir::new().unwrap();
    let s = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let fields = (0..30)
        .map(|n| {
            json!({
                "key": format!("field{n}"), "title": format!("Field {n}"),
                "type": "string", "maxLength": 10_000,
            })
        })
        .collect::<Vec<_>>();
    let values = |value: &str| {
        Value::Object(
            (0..30)
                .map(|n| (format!("field{n}"), json!(value.repeat(10_000))))
                .collect(),
        )
    };
    let ordinary = values("x");
    let oversized = values("😀");
    assert!(ordinary.to_string().len() > 150_000);
    assert!(oversized.to_string().len() > cairn_plugin_api::MAX_FRAME_BYTES);
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    for instance_form in [false, true] {
        let plugin = if instance_form {
            "bounded-form"
        } else {
            "bounded-settings"
        };
        let manifest = json!({
            "id": plugin, "name": "Bounded settings", "version": "1", "apiVersion": 2,
            "capabilities": [], "settings": fields, "instanceView": true, "instanceForm": instance_form,
        });
        let installed = registry::install(&s, &json!({
            "manifest": manifest,
            "source": "return {form = function(event) return {} end, view = function(event) assert(#event.instance.settings.field0 == 10000); return {{type='text', text='Configured'}} end}",
        })).await.unwrap();
        registry::approve(&s, plugin, "1", text(&installed, "digest"))
            .await
            .unwrap();
        let path = format!("/api/plugins/{plugin}/connections");
        let response = common::send(
            &app,
            owner_request(
                "POST",
                &path,
                json!({"name": "Too large", "enabled": true, "settings": oversized}),
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "Oversized settings must be refused before persistence or worker invocation"
        );
        let response = common::send(
            &app,
            owner_request(
                "POST",
                &path,
                json!({"name": "Fits", "enabled": true, "settings": ordinary}),
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        let instance = common::read_json(response).await;
        assert_eq!(instance["settings"], ordinary);
        let id = text(&instance, "id");
        let view = cairn_installation::plugins::panels::view(&s, plugin, id)
            .await
            .unwrap();
        assert_eq!(view["view"][0]["text"], "Configured");
        let response = common::send(
            &app,
            owner_request(
                "PUT",
                &format!("{path}/{id}"),
                json!({"name": "Too large", "enabled": true, "settings": oversized}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            connections::get(&s, plugin, id).await.unwrap().settings,
            ordinary
        );
        if instance_form {
            let response = common::send(
                &app,
                owner_request(
                    "POST",
                    &format!("/api/plugins/{plugin}/form"),
                    json!({"instance": id, "settings": oversized}),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        }
        let path = format!("/api/plugins/{plugin}/setup-drafts");
        let response = common::send(
            &app,
            owner_request(
                "POST",
                &path,
                json!({"name": "Too large", "settings": oversized}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let response = common::send(
            &app,
            owner_request("POST", &path, json!({"name": "Fits", "settings": ordinary})),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        let draft = common::read_json(response).await;
        let path = format!("{path}/{}", text(&draft, "id"));
        let response = common::send(
            &app,
            owner_request(
                "PATCH",
                &path,
                json!({"name": "Too large", "settings": oversized, "revision": draft["revision"]}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let response = common::send(&app, owner_request("GET", &path, Value::Null)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(common::read_json(response).await, draft);
        if instance_form {
            let response = common::send(
                &app,
                owner_request(
                    "POST",
                    &format!("{path}/form"),
                    json!({"settings": oversized, "revision": draft["revision"]}),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        }
    }
}
