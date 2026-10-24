mod common;
#[path = "common/management.rs"]
mod management;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    plugins::{connections, registry, setup},
    validation::text,
};
use management::{active_run, call, enabled_main, public_call, result, service};
use serde_json::{Value, json};
use tempfile::TempDir;

#[tokio::test]
async fn resource_lifecycle_matches_owner_http_public_mcp_and_installation_tool() {
    resource_lifecycle(false).await;
}

#[tokio::test]
async fn inert_resources_match_both_tools_without_acquiring_live_authority() {
    resource_lifecycle(true).await;
}

async fn resource_lifecycle(inert: bool) {
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let main = enabled_main(&s).await;
    let (_, configuration) = active_run(&s, &main).await;
    let package = registry::install(&s, &json!({
        "manifest": {
            "id": "managed-resources", "name": "Managed resources", "version": "1", "apiVersion": 2,
            "capabilities": ["resources"],
            "resources": [{
                "name": "workspaces", "title": "Workspaces", "singular": "Workspace",
                "settings": [{"key": "team", "title": "Team", "type": "string", "required": true}],
            }],
        },
        "source": "return {}",
    })).await.unwrap();
    registry::approve(&s, "managed-resources", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = if inert {
        setup::save(
            &s,
            "managed-resources",
            "fixture-owner",
            &json!({"name": "Inert app"}),
            None,
        )
        .await
        .unwrap()
    } else {
        connections::save(
            &s,
            "managed-resources",
            &json!({"name": "App", "enabled": false}),
            None,
        )
        .await
        .unwrap()
    };
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let path = format!(
        "/api/plugins/managed-resources/{}/{}/resources/workspaces",
        if inert { "setup-drafts" } else { "connections" },
        text(&instance, "id"),
    );
    for source in ["http", "public", "installation"] {
        let invoke = async |action: &str, resource: Option<&str>, mut data: Value| {
            if inert {
                data["setupRevision"] = instance["revision"].clone();
            }
            if source == "http" {
                let path = resource.map_or_else(|| path.clone(), |id| format!("{path}/{id}"));
                let method = match (action, resource) {
                    ("list" | "get", _) => "GET",
                    ("save", None) => "POST",
                    ("save", Some(_)) => "PUT",
                    ("delete", _) => "DELETE",
                    _ => panic!("Unknown fixture action"),
                };
                let body = if method == "GET" {
                    Body::empty()
                } else {
                    Body::from(data.to_string())
                };
                let mut request = common::request(method, &path)
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap();
                request
                    .extensions_mut()
                    .insert(InstallationIdentity::trusted(
                        InstallationRole::Owner,
                        "fixture-owner",
                    ));
                let response = common::send(&app, request).await;
                assert_eq!(response.status(), StatusCode::OK);
                common::read_json(response).await
            } else {
                let mut args = json!({
                    "pluginId": "managed-resources",
                    "collection": "workspaces", "action": action, "data": data,
                });
                args[if inert { "draftId" } else { "instanceId" }] = instance["id"].clone();
                if let Some(id) = resource {
                    args["resourceId"] = json!(id);
                }
                let response = if source == "public" {
                    public_call(&s, &["manage"], "manage_plugin_resource", args).await
                } else {
                    call(&s, &configuration, "manage_plugin_resource", args).await
                };
                result(&response).clone()
            }
        };
        let data =
            json!({"name": format!("Workspace {source}"), "settings": {"team": "T_ALLOWED"}});
        let created = invoke("save", None, data.clone()).await;
        assert_eq!(created["name"], data["name"]);
        assert_eq!(created["settings"], json!({"team": "T_ALLOWED"}));
        assert_eq!(created["connectionId"], instance["id"]);
        if inert {
            let active_path = format!(
                "/api/plugins/managed-resources/connections/{}/resources/workspaces/{}",
                text(&instance, "id"),
                text(&created, "id")
            );
            let mut request = common::request("GET", &active_path)
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(InstallationIdentity::trusted(
                    InstallationRole::Owner,
                    "fixture-owner",
                ));
            assert_eq!(
                common::send(&app, request).await.status(),
                StatusCode::NOT_FOUND
            );
        }
        let id = text(&created, "id");
        let read = invoke("get", Some(id), json!({})).await;
        assert_eq!(read["settings"], json!({"team": "T_ALLOWED"}));
        assert_eq!(read["revision"], created["revision"]);
        let listed = invoke("list", None, json!({})).await;
        assert!(
            listed["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == id)
        );
        let updated = invoke(
            "save",
            Some(id),
            json!({
                "name": "Renamed workspace", "settings": {"team": "T_RENAMED"},
                "revision": created["revision"],
            }),
        )
        .await;
        assert_eq!(updated["id"], id);
        assert_eq!(updated["name"], "Renamed workspace");
        assert_ne!(updated["revision"], created["revision"]);
        let read = invoke("get", Some(id), json!({})).await;
        assert_eq!(read["settings"], json!({"team": "T_RENAMED"}));
        if source != "http" {
            let mut args = json!({
                "pluginId": "managed-resources",
                "collection": "workspaces", "action": "save", "resourceId": id,
                "data": {"name": "Stale", "settings": {"team": "T_STALE"}, "revision": created["revision"]},
            });
            args[if inert { "draftId" } else { "instanceId" }] = instance["id"].clone();
            if inert {
                args["data"]["setupRevision"] = instance["revision"].clone();
            }
            let stale = if source == "public" {
                public_call(&s, &["manage"], "manage_plugin_resource", args).await
            } else {
                call(&s, &configuration, "manage_plugin_resource", args).await
            };
            assert_eq!(stale["result"]["isError"], true, "{stale}");
            let read = invoke("get", Some(id), json!({})).await;
            assert_eq!(read["settings"], json!({"team": "T_RENAMED"}));
        }
        assert_eq!(
            invoke("delete", Some(id), json!({"revision": updated["revision"]})).await["deleted"],
            true
        );
        let listed = invoke("list", None, json!({})).await;
        assert!(
            !listed["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == id)
        );
    }
    let mut target = json!({"pluginId": "managed-resources", "collection": "workspaces", "action": "save", "data": {"name": "Denied", "settings": {"team": "T_DENIED"}}});
    target[if inert { "draftId" } else { "instanceId" }] = instance["id"].clone();
    let denied = public_call(&s, &["read"], "manage_plugin_resource", target.clone()).await;
    assert_eq!(denied["result"]["isError"], true);
    for internal in [false, true] {
        for mode in ["ambiguous", "missing", "self-grant"] {
            let mut args = target.clone();
            match mode {
                "ambiguous" => {
                    args["instanceId"] = instance["id"].clone();
                    args["draftId"] = instance["id"].clone();
                }
                "missing" => {
                    args.as_object_mut().unwrap().remove("instanceId");
                    args.as_object_mut().unwrap().remove("draftId");
                }
                "self-grant" => {
                    args["data"]["agentIds"] = json!([cairn_installation::config::MAIN_AGENT_ID]);
                    if inert {
                        args["data"]["setupRevision"] = instance["revision"].clone();
                    }
                }
                _ => unreachable!(),
            }
            let denied = if internal {
                call(&s, &configuration, "manage_plugin_resource", args).await
            } else {
                public_call(&s, &["manage"], "manage_plugin_resource", args).await
            };
            assert_eq!(denied["result"]["isError"], true, "{mode}: {denied}");
        }
    }
    let mut list = target.clone();
    list["action"] = json!("list");
    list.as_object_mut().unwrap().remove("data");
    assert_eq!(
        result(&public_call(&s, &["manage"], "manage_plugin_resource", list).await)["items"],
        json!([])
    );
    if inert {
        let changed = setup::save(
            &s,
            "managed-resources",
            "fixture-owner",
            &json!({"name": "Changed draft", "revision": instance["revision"]}),
            Some(text(&instance, "id")),
        )
        .await
        .unwrap();
        target["data"]["setupRevision"] = instance["revision"].clone();
        for internal in [false, true] {
            let denied = if internal {
                call(&s, &configuration, "manage_plugin_resource", target.clone()).await
            } else {
                public_call(&s, &["manage"], "manage_plugin_resource", target.clone()).await
            };
            assert_eq!(denied["result"]["isError"], true, "{denied}");
        }
        target["data"]["setupRevision"] = changed["revision"].clone();
        let fresh = public_call(&s, &["manage"], "manage_plugin_resource", target).await;
        assert_eq!(result(&fresh)["settings"]["team"], "T_DENIED");
    }
}

#[tokio::test]
async fn resource_form_parity_preserves_private_parent_credentials_and_owner_grants() {
    resource_form_case(FormCase::Ordinary).await;
}

#[tokio::test]
async fn revoked_management_cannot_publish_a_resource_after_provider_io() {
    resource_form_case(FormCase::Revoked).await;
}

#[tokio::test]
async fn moving_to_a_public_parent_does_not_require_the_old_parents_credential() {
    resource_form_case(FormCase::MoveToPublic).await;
}

#[tokio::test]
async fn inert_private_parent_forms_remain_owner_only() {
    resource_form_case(FormCase::Inert).await;
}

#[tokio::test]
async fn remote_resource_choices_match_owner_http_and_both_management_tools() {
    resource_form_case(FormCase::RemoteLookup).await;
}

enum FormCase {
    Ordinary,
    Revoked,
    MoveToPublic,
    Inert,
    RemoteLookup,
}

async fn resource_form_case(case: FormCase) {
    use axum::{Json, Router, http::HeaderMap, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let inert = matches!(case, FormCase::Inert);
    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let requests = Arc::new(AtomicUsize::new(0));
    let calls = requests.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (arrival, resume) = (entered.clone(), release.clone());
    let (origin, server) = common::serve_locally(Router::new().route("/verify", get(move |headers: HeaderMap, axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| {
        let (arrival, resume) = (arrival.clone(), resume.clone());
        calls.fetch_add(1, Ordering::SeqCst);
        async move {
            if query.get("channel").is_some_and(|value| value == "hold") {
                arrival.notify_one();
                resume.notified().await;
            }
            Json(json!({"ok": headers.get("authorization").is_some_and(|header| header == "Bearer fixture-private-parent"), "label": "Verified team"}))
        }
    }))).await;
    s.plugins.route_http_to(Some(origin));
    let package = registry::install(&s, &json!({
        "manifest": {
            "id": "managed-forms", "name": "Managed forms", "apiVersion": 2, "version": "1",
            "capabilities": ["resources", "http"], "hosts": ["api.example.test"], "agentAccess": {},
            "resources": [
                {"name": "workspaces", "title": "Workspaces", "singular": "Workspace", "settings": [{"key": "token", "title": "Token", "type": "secret"}]},
                {"name": "channels", "title": "Channels", "singular": "Channel", "parent": "workspaces", "form": true,
                 "settings": [{"key": "channel", "title": "Channel", "type": "select", "dynamic": true, "remote": true, "required": true}, {"key": "verified", "title": "Verified label", "type": "string", "hidden": true, "required": true}]},
            ],
        },
        "source": r#"return {form = function(event)
            if event.values.channel == 'public' then return {values = {verified = 'Public channel'}, options = {channel = {{value = 'public', label = 'Public'}}}} end
            local response = cairn.http.request({url = 'https://api.example.test/verify?channel=' .. (event.values.channel or ''), auth = {configuration = 'parent', secret = 'token', prefix = 'Bearer '}})
            assert(response.json.ok, 'The selected parent credential was not used')
            if event.lookup then
                if event.lookup.query == 'derive' then return {values = {verified = 'Must not apply'}} end
                if event.lookup.cursor then return {options = {channel = cairn.array()}, pages = {channel = {nextCursor = cairn.json.null}}} end
                return {options = {channel = {{value = 'C2', label = event.lookup.query}}}, pages = {channel = {nextCursor = 'next'}}}
            end
            return {values = {verified = response.json.label}, options = {channel = {{value = 'C1', label = 'General'}, {value = 'hold', label = 'Held'}}}}
        end}"#,
    })).await.unwrap();
    registry::approve(&s, "managed-forms", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = if inert {
        setup::save(
            &s,
            "managed-forms",
            "fixture-owner",
            &json!({"name": "Inert app"}),
            None,
        )
        .await
        .unwrap()
    } else {
        connections::save(
            &s,
            "managed-forms",
            &json!({"name": "App", "enabled": true}),
            None,
        )
        .await
        .unwrap()
    };
    let mut parent_args = json!({
        "pluginId": "managed-forms", "collection": "workspaces", "action": "save",
        "data": {"name": "Private workspace", "secrets": {"token": "fixture-private-parent"}},
    });
    parent_args[if inert { "draftId" } else { "instanceId" }] = instance["id"].clone();
    if inert {
        parent_args["data"]["setupRevision"] = instance["revision"].clone();
    }
    let parent = public_call(&s, &["manage"], "manage_plugin_resource", parent_args).await;
    let parent = result(&parent).clone();
    let mut data = json!({"parentId": parent["id"], "settings": {"channel": "C1"}});
    if inert {
        data["setupRevision"] = instance["revision"].clone();
    }
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let path = format!(
        "/api/plugins/managed-forms/{}/{}/resources/channels/form",
        if inert { "setup-drafts" } else { "connections" },
        text(&instance, "id")
    );
    let mut request = common::request("POST", &path)
        .header("content-type", "application/json")
        .body(Body::from(data.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    let response = common::send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let expected = common::read_json(response).await;
    assert_eq!(expected["values"]["verified"], "Verified team");
    let mut args = json!({"pluginId": "managed-forms", "collection": "channels", "action": "form", "data": data});
    args[if inert { "draftId" } else { "instanceId" }] = instance["id"].clone();
    let preview = public_call(&s, &["manage"], "manage_plugin_resource", args.clone()).await;
    assert_eq!(result(&preview), &expected);
    let before = requests.load(Ordering::SeqCst);
    let denied = call(&s, &configuration, "manage_plugin_resource", args.clone()).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        before,
        "Management access must not borrow the parent's credential"
    );
    if inert {
        server.abort();
        return;
    }
    // Keep the large owner-grant fixture future off this scenario's stack.
    Box::pin(connections::save(
        &s,
        "managed-forms",
        &json!({
            "name": "App",
            "enabled": true,
            "agentIds": [cairn_installation::config::MAIN_AGENT_ID],
        }),
        Some(text(&instance, "id")),
    ))
    .await
    .unwrap();
    let preview = call(&s, &configuration, "manage_plugin_resource", args).await;
    assert_eq!(result(&preview), &expected);
    let saved = call(&s, &configuration, "manage_plugin_resource", json!({
        "pluginId": "managed-forms", "instanceId": instance["id"], "collection": "channels", "action": "save",
        "data": {"name": "Channel", "parentId": parent["id"], "settings": {"channel": "C1", "verified": "forged"}},
    })).await;
    assert_eq!(result(&saved)["settings"]["verified"], "Verified team");
    for response in [&preview, &saved] {
        assert!(!response.to_string().contains("fixture-private-parent"));
    }
    if matches!(case, FormCase::RemoteLookup) {
        let mut args = json!({
            "pluginId": "managed-forms", "instanceId": instance["id"], "collection": "channels", "action": "form", "resourceId": result(&saved)["id"],
            "data": {"parentId": parent["id"], "revision": result(&saved)["revision"], "settings": {"channel": "C1"}},
        });
        for cursor in [None, Some("next")] {
            let lookup = match cursor {
                None => json!({"field": "channel", "query": "Remote result"}),
                Some(cursor) => {
                    json!({"field": "channel", "query": "Remote result", "cursor": cursor})
                }
            };
            args["data"]["lookup"] = lookup;
            let mut http = args["data"].clone();
            http["resource"] = result(&saved)["id"].clone();
            let mut request = common::request("POST", &path)
                .header("content-type", "application/json")
                .body(Body::from(http.to_string()))
                .unwrap();
            request
                .extensions_mut()
                .insert(InstallationIdentity::trusted(
                    InstallationRole::Owner,
                    "fixture-owner",
                ));
            let response = common::send(&app, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            let expected = common::read_json(response).await;
            assert_eq!(expected["lookup"]["field"], "channel");
            assert_eq!(expected["values"], json!({}));
            if cursor.is_none() {
                assert_eq!(
                    expected["options"]["channel"],
                    json!([{"value": "C2", "label": "Remote result"}])
                );
                assert_eq!(expected["pages"]["channel"]["nextCursor"], "next");
            } else {
                assert_eq!(expected["options"]["channel"], json!([]));
                assert!(expected["pages"]["channel"]["nextCursor"].is_null());
            }
            assert_eq!(
                result(&public_call(&s, &["manage"], "manage_plugin_resource", args.clone()).await),
                &expected
            );
            assert_eq!(
                result(&call(&s, &configuration, "manage_plugin_resource", args.clone()).await),
                &expected
            );
        }
        args["data"]["lookup"]["query"] = json!("derive");
        for internal in [false, true] {
            let response = if internal {
                call(&s, &configuration, "manage_plugin_resource", args.clone()).await
            } else {
                public_call(&s, &["manage"], "manage_plugin_resource", args.clone()).await
            };
            assert_eq!(
                response["result"]["isError"], true,
                "Lookup cannot derive saved values: {response}"
            );
        }
        args["data"].as_object_mut().unwrap().remove("lookup");
        args["data"]["resource"] = args["resourceId"].clone();
        args.as_object_mut().unwrap().remove("resourceId");
        let before = requests.load(Ordering::SeqCst);
        for internal in [false, true] {
            let response = if internal {
                call(&s, &configuration, "manage_plugin_resource", args.clone()).await
            } else {
                public_call(&s, &["manage"], "manage_plugin_resource", args.clone()).await
            };
            assert_eq!(response["result"]["isError"], true);
        }
        assert_eq!(requests.load(Ordering::SeqCst), before);
        let unchanged = public_call(&s, &["manage"], "manage_plugin_resource", json!({"pluginId": "managed-forms", "instanceId": instance["id"], "collection": "channels", "action": "get", "resourceId": result(&saved)["id"]})).await;
        assert_eq!(result(&unchanged), result(&saved));
    }
    if matches!(case, FormCase::MoveToPublic) {
        let public = public_call(&s, &["manage"], "manage_plugin_resource", json!({
            "pluginId": "managed-forms", "instanceId": instance["id"], "collection": "workspaces", "action": "save",
            "data": {"name": "Public workspace"},
        })).await;
        let public = result(&public);
        assert_eq!(public["secretKeys"], json!([]));
        connections::save(
            &s,
            "managed-forms",
            &json!({"name": "App", "enabled": true, "agentIds": []}),
            Some(text(&instance, "id")),
        )
        .await
        .unwrap();
        let mut args = json!({
            "pluginId": "managed-forms", "instanceId": instance["id"], "collection": "channels", "action": "form", "resourceId": result(&saved)["id"],
            "data": {"parentId": public["id"], "revision": result(&saved)["revision"], "settings": {"channel": "public"}},
        });
        let before = requests.load(Ordering::SeqCst);
        let owner = public_call(&s, &["manage"], "manage_plugin_resource", args.clone()).await;
        assert_eq!(result(&owner)["values"]["verified"], "Public channel");
        let preview = call(&s, &configuration, "manage_plugin_resource", args.clone()).await;
        assert_eq!(result(&preview), result(&owner));
        args["action"] = json!("save");
        args["data"]["name"] = json!("Moved channel");
        let moved = call(&s, &configuration, "manage_plugin_resource", args).await;
        assert_eq!(result(&moved)["parentId"], public["id"]);
        assert_eq!(result(&moved)["settings"]["verified"], "Public channel");
        assert_eq!(requests.load(Ordering::SeqCst), before);
    }
    if matches!(case, FormCase::Revoked) {
        let list = json!({"pluginId": "managed-forms", "instanceId": instance["id"], "collection": "channels", "action": "list"});
        let before = public_call(&s, &["manage"], "manage_plugin_resource", list.clone()).await;
        let pending = tokio::spawn({
            let s = s.clone();
            let configuration = configuration.clone();
            let instance = instance.clone();
            let parent = parent.clone();
            async move {
                call(&s, &configuration, "manage_plugin_resource", json!({
                    "pluginId": "managed-forms", "instanceId": instance["id"], "collection": "channels", "action": "save",
                    "data": {"name": "Must not publish", "parentId": parent["id"], "settings": {"channel": "hold"}},
                })).await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
        release.notify_one();
        let denied = tokio::time::timeout(std::time::Duration::from_secs(10), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(denied["result"]["isError"], true, "{denied}");
        let after = public_call(&s, &["manage"], "manage_plugin_resource", list).await;
        assert_eq!(result(&after), result(&before));
    }
    server.abort();
}

#[tokio::test]
async fn saved_resource_oauth_is_not_implicit_configuration_authority() {
    use axum::{
        Json, Router,
        extract::Form,
        http::HeaderMap,
        routing::{get, post},
    };
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    let root = TempDir::new().unwrap();
    let s = service(&root).await;
    s.plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let main = enabled_main(&s).await;
    let (run, configuration) = active_run(&s, &main).await;
    let requests = Arc::new(AtomicUsize::new(0));
    let calls = requests.clone();
    let expiring = Arc::new(AtomicBool::new(false));
    let short_lived = expiring.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (arrival, resume) = (entered.clone(), release.clone());
    let refreshes = Arc::new(AtomicUsize::new(0));
    let exchanges = refreshes.clone();
    let provider = Router::new()
        .route("/token", post(move |Form(fields): Form<HashMap<String, String>>| {
            let (arrival, resume) = (arrival.clone(), resume.clone());
            let (short_lived, exchanges) = (short_lived.clone(), exchanges.clone());
            async move {
                assert_eq!(fields["client_id"], "fixture-client");
                if fields["grant_type"] == "refresh_token" {
                    assert_eq!(fields["refresh_token"], "fixture-private-refresh");
                    assert_eq!(exchanges.fetch_add(1, Ordering::SeqCst), 0);
                    arrival.notify_one();
                    resume.notified().await;
                } else {
                    assert!(fields.get("code_verifier").is_some_and(|value| value.len() >= 43));
                }
                Json(json!({"access_token": "fixture-private-resource-oauth", "refresh_token": "fixture-private-refresh", "scope": "read", "expires_in": if short_lived.load(Ordering::SeqCst) { 1 } else { 3600 }, "team": {"id": "T_OAUTH"}}))
            }
        }))
        .route("/verify", get(move |headers: HeaderMap| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Json(json!({"ok": headers.get("authorization").is_some_and(|header| header == "Bearer fixture-private-resource-oauth")})) }
        }));
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let package = registry::install(&s, &json!({
        "manifest": {
            "id": "managed-resource-oauth", "name": "Managed resource OAuth", "apiVersion": 2, "version": "1",
            "capabilities": ["http", "resources", "oauth"], "hosts": ["api.example.test"], "agentAccess": {},
            "settings": [{"key": "clientId", "title": "Client", "type": "string", "required": true}],
            "resources": [
                {"name": "workspaces", "title": "Workspaces", "singular": "Workspace", "settings": [{"key": "team", "title": "Team", "type": "string"}]},
                {"name": "channels", "title": "Channels", "singular": "Channel", "parent": "workspaces", "form": true, "settings": [{"key": "verified", "title": "Verified", "type": "boolean", "hidden": true}]},
            ],
            "oauth": [{
                "name": "bot", "title": "Connect workspace", "collection": "workspaces", "pkce": true,
                "authorizationUrl": "https://api.example.test/authorize", "tokenUrl": "https://api.example.test/token", "clientId": "clientId", "scopes": ["read"],
                "token": {"accessToken": "/access_token", "refreshToken": "/refresh_token", "scope": "/scope", "expiresIn": "/expires_in"}, "metadata": {"team": "/team/id"},
            }],
        },
        "source": r#"return {form = function(event)
            local response = cairn.http.request({url = 'https://api.example.test/verify', auth = {configuration = 'parent', oauth = 'bot'}})
            assert(response.json.ok, 'The host did not use the selected OAuth authorization')
            return {values = {verified = true}}
        end}"#,
    })).await.unwrap();
    registry::approve(&s, "managed-resource-oauth", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "managed-resource-oauth",
        &json!({"name": "App", "enabled": true, "settings": {"clientId": "fixture-client"}}),
        None,
    )
    .await
    .unwrap();
    let parent = public_call(&s, &["manage"], "manage_plugin_resource", json!({
        "pluginId": "managed-resource-oauth", "instanceId": instance["id"], "collection": "workspaces", "action": "save", "data": {"name": "Workspace"},
    })).await;
    let parent = result(&parent).clone();
    assert_eq!(parent["secretKeys"], json!([]));
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let owner = async |method: &str, path: &str, data: Value| {
        let mut request = common::request(method, path)
            .header("content-type", "application/json")
            .body(Body::from(data.to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert(InstallationIdentity::trusted(
                InstallationRole::Owner,
                "fixture-owner",
            ));
        let response = common::send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        common::read_json(response).await
    };
    let oauth_path = format!(
        "/api/plugins/managed-resource-oauth/connections/{}/resources/workspaces/{}/oauth/bot",
        text(&instance, "id"),
        text(&parent, "id")
    );
    let connect = async || {
        let started = owner("POST", &oauth_path, json!({"scopes": ["read"]})).await;
        let path = format!(
            "/api/plugin-oauth/{}/callback?state={}&code=fixture-oauth-code",
            text(&instance, "id"),
            text(&started, "state")
        );
        let response = common::send(
            &app,
            common::request("GET", &path).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let finished = owner(
            "POST",
            &format!("{oauth_path}/finish"),
            json!({"state": started["state"]}),
        )
        .await;
        assert_eq!(finished["status"], "authorized");
        assert!(
            !finished
                .to_string()
                .contains("fixture-private-resource-oauth")
        );
    };
    connect().await;
    let args = json!({"pluginId": "managed-resource-oauth", "instanceId": instance["id"], "collection": "channels", "action": "form", "data": {"parentId": parent["id"], "settings": {}}});
    let preview = public_call(&s, &["manage"], "manage_plugin_resource", args.clone()).await;
    assert_eq!(result(&preview)["values"]["verified"], true);
    let before = requests.load(Ordering::SeqCst);
    let denied = call(&s, &configuration, "manage_plugin_resource", args.clone()).await;
    assert_eq!(
        denied["result"]["isError"], true,
        "A saved OAuth token is private even when secretKeys is empty: {denied}"
    );
    assert_eq!(requests.load(Ordering::SeqCst), before);
    connections::save(
        &s,
        "managed-resource-oauth",
        &json!({
            "name": "App",
            "enabled": true,
            "settings": {"clientId": "fixture-client"},
            "agentIds": [cairn_installation::config::MAIN_AGENT_ID],
        }),
        Some(text(&instance, "id")),
    )
    .await
    .unwrap();
    connect().await;
    let allowed = call(&s, &configuration, "manage_plugin_resource", args.clone()).await;
    assert_eq!(result(&allowed)["values"]["verified"], true);
    assert!(
        !allowed
            .to_string()
            .contains("fixture-private-resource-oauth")
    );

    expiring.store(true, Ordering::SeqCst);
    connect().await;
    let list = json!({"pluginId": "managed-resource-oauth", "instanceId": instance["id"], "collection": "channels", "action": "list"});
    let before = public_call(&s, &["manage"], "manage_plugin_resource", list.clone()).await;
    let provider_reads = requests.load(Ordering::SeqCst);
    let pending = tokio::spawn({
        let s = s.clone();
        let configuration = configuration.clone();
        let (instance, parent) = (instance.clone(), parent.clone());
        async move {
            call(&s, &configuration, "manage_plugin_resource", json!({
                "pluginId": "managed-resource-oauth", "instanceId": instance["id"], "collection": "channels", "action": "save",
                "data": {"name": "Must not publish after refresh", "parentId": parent["id"], "settings": {}},
            })).await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
    release.notify_one();
    let denied = tokio::time::timeout(std::time::Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert_eq!(requests.load(Ordering::SeqCst), provider_reads);
    let after = public_call(&s, &["manage"], "manage_plugin_resource", list).await;
    assert_eq!(result(&after), result(&before));
    assert_eq!(
        owner("GET", &oauth_path, Value::Null).await["status"],
        "reconnect"
    );
    let no_replay = public_call(&s, &["manage"], "manage_plugin_resource", args).await;
    assert_eq!(no_replay["result"]["isError"], true);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    server.abort();
}
