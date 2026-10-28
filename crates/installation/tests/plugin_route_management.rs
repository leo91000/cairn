use crate::common::AgentFixture;

mod common;
#[path = "common/management.rs"]
mod management;

use aws_lc_rs::hmac;
use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::now,
    plugins::{connections, registry, resources},
    service::Service,
    validation::text,
};
use management::{active_run, call, enabled_main, public_call, result, service};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    s: Arc<Service>,
    main: Value,
    configuration: Value,
    instance: Value,
    resource: Value,
    app: axum::Router,
}

fn route_input(main: &Value) -> Value {
    json!({
        "name": "Private route", "enabled": true, "match": {"channel": "C_ALLOWED"},
        "agentId": main["id"], "accessMode": "requests", "userIds": ["U_ALICE"],
    })
}

fn route_args(f: &Fixture, action: &str, route: &Value, data: &Value) -> Value {
    json!({
        "pluginId": "managed-routes", "instanceId": f.instance["id"],
        "collection": "workspaces", "resourceId": f.resource["id"],
        "routeId": route["id"], "action": action, "data": data,
    })
}

async fn saved_route(f: &Fixture) -> Value {
    cairn_installation::plugins::routes::save(
        &f.s,
        "managed-routes",
        text(&f.instance, "id"),
        "workspaces",
        text(&f.resource, "id"),
        "fixture-owner",
        &route_input(&f.main),
        None,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn route_tools_refuse_stale_edits_wrong_targets_and_self_grants() {
    let f = fixture().await;
    let route = saved_route(&f).await;
    let denied = public_call(
        &f.s,
        &["read"],
        "manage_plugin_route",
        route_args(&f, "list", &route, &json!({})),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    let other = resources::save(
        &f.s,
        "managed-routes",
        text(&f.instance, "id"),
        "workspaces",
        &json!({"name": "Other", "settings": {}}),
        None,
    )
    .await
    .unwrap();
    let draft = cairn_installation::plugins::setup::save(
        &f.s,
        "managed-routes",
        "fixture-owner",
        &json!({"name": "Inert app"}),
        None,
    )
    .await
    .unwrap();
    for public in [true, false] {
        let invoke = async |args: Value| {
            if public {
                public_call(&f.s, &["manage"], "manage_plugin_route", args).await
            } else {
                call(&f.s, &f.configuration, "manage_plugin_route", args).await
            }
        };
        for action in ["save", "delete", "people"] {
            let mut data = if action == "save" {
                route_input(&f.main)
            } else {
                json!({"action": "remove"})
            };
            data["revision"] = json!("stale-edit-token");
            let mut args = route_args(&f, action, &route, &data);
            if action == "people" {
                args["userId"] = json!("U_ALICE");
            }
            let response = invoke(args).await;
            assert_eq!(response["result"]["isError"], true, "{response}");
            assert!(response.to_string().contains("changed"), "{response}");
        }
        let mut data = route_input(&f.main);
        data["revision"] = route["revision"].clone();
        data["agentIds"] = json!([f.main["id"]]);
        let response = invoke(route_args(&f, "save", &route, &data)).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        let mut args = route_args(
            &f,
            "delete",
            &route,
            &json!({"revision": route["revision"]}),
        );
        args["resourceId"] = other["id"].clone();
        let response = invoke(args).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        let mut args = route_args(&f, "list", &route, &json!({}));
        args["instanceId"] = draft["id"].clone();
        let response = invoke(args).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
    }
    let current = cairn_installation::plugins::routes::list(
        &f.s,
        "managed-routes",
        text(&f.instance, "id"),
        "workspaces",
        text(&f.resource, "id"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(current["items"][0]["revision"], route["revision"]);
    assert_eq!(current["items"][0]["userIds"], json!(["U_ALICE"]));
    assert_eq!(
        f.s.agent_snapshot(text(&f.main, "id")).await.unwrap()["plugins"],
        json!([])
    );
}

#[tokio::test]
async fn route_management_rechecks_permissions_and_target_after_authority_io() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for (action, change) in [
        ("eligibility", "revoke"),
        ("list", "revoke"),
        ("requests", "revoke"),
        ("save", "revoke"),
        ("people", "revoke"),
        ("delete", "revoke"),
        ("save", "remove-resource"),
        ("save", "replace-owner"),
    ] {
        let f = fixture().await;
        let route = saved_route(&f).await;
        let mut data = if action == "save" {
            route_input(&f.main)
        } else {
            json!({})
        };
        data["revision"] = route["revision"].clone();
        if action == "save" {
            data["name"] = json!("Changed without authority");
        }
        if action == "people" {
            data["action"] = json!("remove");
        }
        if action == "eligibility" {
            data = json!({"accessMode": "requests"});
        }
        let mut args = route_args(&f, action, &route, &data);
        if action == "people" {
            args["userId"] = json!("U_ALICE");
        }
        if change == "remove-resource" {
            args.as_object_mut().unwrap().remove("routeId");
        }
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let replaced = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let (arrival, resume, owner_changed) = (entered.clone(), release.clone(), replaced.clone());
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let (arrival, resume, calls, owner_changed) = (arrival.clone(), resume.clone(), calls.clone(), owner_changed.clone());
                async move {
                    // Pause the late authority exchange, after initial owner
                    // resolution and preflight, using the actual HTTP boundary.
                    if calls.fetch_add(1, Ordering::SeqCst) == 3 {
                        arrival.notify_one();
                        resume.notified().await;
                    }
                    axum::Json(json!({"owner": {"account_id": "fixture-owner", "access_id": if owner_changed.load(Ordering::SeqCst) {"replacement-owner-grant"} else {"fixture-owner"}}, "members": []}))
                }
            }),
        );
        let (origin, server) = common::serve_locally(authority).await;
        let path = f.s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(&path, serde_json::to_vec(&identity).unwrap()).unwrap();
        let (s, configuration) = (f.s.clone(), f.configuration.clone());
        let mut operation =
            tokio::spawn(
                async move { call(&s, &configuration, "manage_plugin_route", args).await },
            );
        tokio::select! {
            () = entered.notified() => {},
            result = &mut operation => panic!("{action}/{change} returned before the late authority exchange: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("{action}/{change} never reached the authority exchange"),
        }
        match change {
            "remove-resource" => {
                resources::remove(
                    &f.s,
                    "managed-routes",
                    text(&f.instance, "id"),
                    "workspaces",
                    text(&f.resource, "id"),
                )
                .await
                .unwrap();
            }
            "replace-owner" => {
                replaced.store(true, Ordering::SeqCst);
            }
            _ => {
                f.s.agent_fixture(
                    json!({"name": "Main agent", "access": {"installation": false}}),
                    Some(text(&f.main, "id")),
                )
                .await
                .unwrap();
            }
        }
        release.notify_one();
        let response = operation.await.unwrap();
        assert_eq!(
            response["result"]["isError"], true,
            "{action}/{change}: {response}"
        );
        if change == "revoke" {
            let current = cairn_installation::plugins::routes::list(
                &f.s,
                "managed-routes",
                text(&f.instance, "id"),
                "workspaces",
                text(&f.resource, "id"),
                None,
            )
            .await
            .unwrap();
            assert_eq!(current["items"][0]["revision"], route["revision"]);
            assert_eq!(current["items"][0]["userIds"], json!(["U_ALICE"]));
        }
        server.abort();
    }
}

async fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let package = registry::install(&s, &json!({
        "manifest": {
            "id": "managed-routes", "name": "Managed routes", "version": "1", "apiVersion": 2,
            "capabilities": ["resources", "conversations.submit", "webhooks", "inbound"],
            "settings": [{"key": "signature", "title": "Signature", "type": "secret"}],
            "webhooks": [{"name": "events", "signature": {"secret": "signature", "header": "x-signature", "prefix": "v0=", "template": "v0:{{timestamp}}:{{body}}", "timestamp": {"header": "x-timestamp", "maxAgeSeconds": 300}}}],
            "resources": [{"name": "workspaces", "title": "Workspaces", "singular": "Workspace", "delegation": true}],
        },
        "source": "return {webhook = function(event) return {status = 200, body = cairn.json.encode(cairn.inbound.deliver(cairn.json.decode(event.body)))} end}",
    })).await.unwrap();
    registry::approve(&s, "managed-routes", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "managed-routes",
        &json!({"name": "App", "enabled": true, "secrets": {"signature": "managed-route-fixture"}}),
        None,
    )
    .await
    .unwrap();
    let resource = resources::save(
        &s,
        "managed-routes",
        text(&instance, "id"),
        "workspaces",
        &json!({"name": "Workspace", "settings": {}}),
        None,
    )
    .await
    .unwrap();
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    Fixture {
        _root: root,
        s,
        main,
        configuration,
        instance,
        resource,
        app,
    }
}

#[tokio::test]
async fn route_lifecycle_matches_owner_http_public_mcp_and_installation_tool() {
    let Fixture {
        _root,
        s,
        main,
        configuration,
        instance,
        resource,
        app,
        ..
    } = fixture().await;
    let path = format!(
        "/api/plugins/managed-routes/connections/{}/resources/workspaces/{}/routes",
        text(&instance, "id"),
        text(&resource, "id"),
    );
    for source in ["http", "public", "installation"] {
        let invoke = async |action: &str, route: Option<&str>, user: Option<&str>, data: Value| {
            if source == "http" {
                let path = route.map_or_else(|| path.clone(), |id| format!("{path}/{id}"));
                let path = match action {
                    "eligibility" => format!(
                        "{}/route-eligibility?accessMode=requests",
                        path.strip_suffix("/routes").unwrap()
                    ),
                    "requests" => format!("{path}/requests"),
                    "people" => format!("{path}/people/{}", user.unwrap()),
                    _ => path,
                };
                let method = match (action, route) {
                    ("list" | "requests" | "eligibility", _) => "GET",
                    ("people", _) => "POST",
                    ("save", None) => "POST",
                    ("save", Some(_)) => "PUT",
                    ("delete", _) => "DELETE",
                    _ => panic!("Unknown fixture action"),
                };
                let mut request = common::request(method, &path)
                    .header("content-type", "application/json")
                    .body(if method == "GET" {
                        Body::empty()
                    } else {
                        Body::from(data.to_string())
                    })
                    .unwrap();
                request
                    .extensions_mut()
                    .insert(InstallationIdentity::trusted(
                        InstallationRole::Owner,
                        "fixture-owner",
                    ));
                let response = common::send(&app, request).await;
                assert_eq!(response.status(), StatusCode::OK, "{source}: {action}");
                common::read_json(response).await
            } else {
                let mut args = json!({
                    "pluginId": "managed-routes", "instanceId": instance["id"],
                    "collection": "workspaces", "resourceId": resource["id"],
                    "action": action, "data": data,
                });
                if let Some(id) = route {
                    args["routeId"] = json!(id);
                }
                if let Some(user) = user {
                    args["userId"] = json!(user);
                }
                let response = if source == "public" {
                    public_call(&s, &["manage"], "manage_plugin_route", args).await
                } else {
                    call(&s, &configuration, "manage_plugin_route", args).await
                };
                result(&response).clone()
            }
        };
        let mut data = json!({
            "name": format!("Route {source}"), "enabled": true,
            "match": {"channel": "C_ALLOWED"}, "agentId": main["id"],
            "accessMode": "requests", "userIds": ["U_ALICE"],
        });
        let eligible = invoke("eligibility", None, None, json!({"accessMode": "requests"})).await;
        assert!(
            eligible["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["agentId"] == main["id"] && item["eligible"] == true)
        );
        let created = invoke("save", None, None, data.clone()).await;
        let route = text(&created, "id");
        assert_eq!(created["resourceId"], resource["id"]);
        assert_eq!(created["agentId"], main["id"]);
        assert_eq!(created["userIds"], json!(["U_ALICE"]));
        let listed = invoke("list", None, None, json!({})).await;
        assert_eq!(listed["items"][0]["id"], route);
        assert_eq!(listed["items"][0]["revision"], created["revision"]);
        data["revision"] = created["revision"].clone();
        data["name"] = json!("Edited route");
        let updated = invoke("save", Some(route), None, data).await;
        assert_eq!(updated["id"], route);
        assert_eq!(updated["name"], "Edited route");
        assert_ne!(updated["revision"], created["revision"]);
        for person in ["U_BOB", "U_MALLORY"] {
            let body = json!({
                "id": format!("{source}-{person}"), "resource": resource["id"],
                "author": {"id": person, "name": person},
                "context": {"channel": "C_ALLOWED", "thread": person},
                "body": {"text": "private unadmitted message"},
            })
            .to_string();
            let timestamp = (now() / 1000).to_string();
            let key = hmac::Key::new(hmac::HMAC_SHA256, b"managed-route-fixture");
            let signature =
                hex::encode(hmac::sign(&key, format!("v0:{timestamp}:{body}").as_bytes()).as_ref());
            let request = common::request(
                "POST",
                &format!("/api/plugin-webhooks/{}/events", text(&instance, "id")),
            )
            .header("content-type", "application/json")
            .header("x-timestamp", timestamp)
            .header("x-signature", format!("v0={signature}"))
            .body(Body::from(body))
            .unwrap();
            let response = common::send(&app, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(common::read_json(response).await["status"], "requested");
        }
        let pending = invoke("requests", Some(route), None, json!({})).await;
        assert_eq!(pending["items"].as_array().unwrap().len(), 2);
        assert!(!pending.to_string().contains("private unadmitted message"));
        let approved = invoke(
            "people",
            Some(route),
            Some("U_BOB"),
            json!({"action": "approve", "revision": updated["revision"]}),
        )
        .await;
        assert_eq!(approved["userIds"], json!(["U_ALICE", "U_BOB"]));
        let denied = invoke(
            "people",
            Some(route),
            Some("U_MALLORY"),
            json!({"action": "deny", "revision": approved["revision"]}),
        )
        .await;
        assert_eq!(denied["userIds"], approved["userIds"]);
        let removed = invoke(
            "people",
            Some(route),
            Some("U_BOB"),
            json!({"action": "remove", "revision": denied["revision"]}),
        )
        .await;
        assert_eq!(removed["userIds"], json!(["U_ALICE"]));
        let requests = invoke("requests", Some(route), None, json!({})).await;
        assert_eq!(requests["items"].as_array().unwrap().len(), 1);
        assert_eq!(requests["items"][0]["userId"], "U_MALLORY");
        assert_eq!(requests["items"][0]["status"], "denied");
        let deleted = invoke(
            "delete",
            Some(route),
            None,
            json!({"revision": removed["revision"]}),
        )
        .await;
        assert_eq!(deleted, json!({"deleted": true}));
        assert_eq!(
            invoke("list", None, None, json!({})).await["items"],
            json!([])
        );
    }
}
