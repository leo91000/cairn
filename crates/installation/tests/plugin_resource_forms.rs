mod common;
#[path = "plugin_resource_forms/oauth.rs"]
mod oauth;

use axum::{Router, body::Body, http::StatusCode, response::Response};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    plugins::{connections, registry, resources, setup},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tempfile::TempDir;

const SOURCE: &str = r#"local function choices(event)
    assert(event.submit == false and event.changed == nil)
    assert(event.lookup.field == 'channel')
    local query = event.lookup.query
    if query == 'derive' then return {values = {label = 'must not apply'}} end
    if query == 'oversize' then
        local items = {}
        for index = 1, 201 do table.insert(items, {value = tostring(index), label = 'Channel'}) end
        return {options = {channel = items}}
    end
    if query == 'unknown' then return {options = {channel = cairn.array()}} end
    if query ~= '' then
        return {options = {channel = {{value = 'found', label = query}}}, pages = {channel = {nextCursor = cairn.json.null}}}
    end
    if event.lookup.cursor == nil then
        return {options = {channel = {{value = 'C1', label = 'General'}}}, pages = {channel = {nextCursor = 'empty'}}}
    end
    if event.lookup.cursor == 'empty' then
        return {options = {channel = cairn.array()}, pages = {channel = {nextCursor = 'last'}}}
    end
    assert(event.lookup.cursor == 'last')
    return {options = {channel = {{value = 'C2', label = 'Engineering'}}}, pages = {channel = {nextCursor = cairn.json.null}}}
end
return {form = function(event)
    if event.resource == nil then
        if event.values.channel == 'hold-root' then
            local response = cairn.http.request({url = 'https://api.example.test/root-form', auth = true})
            assert(response.json.ok, 'The host injected the wrong root credential')
        end
        if event.lookup then return choices(event) end
        return nil
    end
    assert(event.app.id == event.instance and event.app.settings.token == nil)
    assert(event.resource.collection == 'channels')
    assert(event.values.token == nil)
    if event.resource.id then
        assert(type(event.resource.settings) == 'table')
        assert(event.resource.settings.token == nil)
    else
        assert(event.resource.settings == nil)
    end
    if event.resource.parent == nil then
        return {errors = {parentId = 'Choose a workspace first.'}}
    end
    assert(event.resource.parent.settings.token == nil)
    local function check(part, auth)
        local result = cairn.http.request({url = 'https://api.example.test/' .. part, auth = auth})
        assert(result.json.ok, 'The host injected the wrong credential')
        return result.json
    end
    check('root', true)
    check('app', {secret = 'token', prefix = 'App '})
    local parent = check('parent', {configuration = 'parent', secret = 'token', prefix = 'Parent '})
    check('child', {configuration = 'self', secret = 'token', prefix = 'Child '})
    if event.lookup then return choices(event) end
    if event.values.channel == 'forbidden' then
        return {errors = {channel = 'This channel is not available.'}}
    end
    return {
        options = {channel = {{value = 'C1', label = 'General'}}},
        values = {label = parent.name .. (event.submit and ' verified' or ' preview')},
        view = {{type = 'alert', tone = 'info', text = 'Only the selected workspace is queried.'}}
    }
end}"#;

async fn request(
    app: &Router,
    path: &str,
    method: &str,
    body: Value,
    role: InstallationRole,
) -> Response {
    let mut request = common::request(method, path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(role, "fixture-owner"));
    common::send(app, request).await
}

struct Fixture {
    _root: TempDir,
    _service: Arc<Service>,
    app: Router,
    path: String,
    root: Value,
    parent: Value,
    hold: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    server: tokio::task::JoinHandle<()>,
    inert: bool,
}

impl Fixture {
    async fn new(inert: bool) -> Self {
        Self::with_program(inert, false).await
    }

    async fn with_program(inert: bool, cold: bool) -> Self {
        Self::with_features(inert, cold, false).await
    }

    async fn with_features(inert: bool, cold: bool, remote: bool) -> Self {
        let root = TempDir::new().unwrap();
        let s = Service::new(common::config(root.path())).await.unwrap();
        common::relay_fixture::claimed(&s).await.unwrap();
        s.plugins
            .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let mut package_input = json!({
            "manifest": {
                "id": "resourceforms",
                "name": "Resource forms",
                "version": "1",
                "apiVersion": 2,
                "capabilities": ["http", "resources"],
                "hosts": ["api.example.test"],
                "credential": {
                    "label": "App credential",
                    "header": "authorization",
                    "prefix": "Root ",
                },
                "settings": [{
                    "key": "token",
                    "title": "App secret",
                    "type": "secret",
                    "required": true,
                }],
                "resources": [
                    {
                        "name": "workspaces",
                        "title": "Workspaces",
                        "singular": "Workspace",
                        "settings": [{
                            "key": "token",
                            "title": "Workspace secret",
                            "type": "secret",
                            "required": true,
                        }],
                    },
                    {
                        "name": "channels",
                        "title": "Channels",
                        "singular": "Channel",
                        "parent": "workspaces",
                        "form": true,
                        "settings": [
                            {
                                "key": "channel",
                                "title": "Channel",
                                "type": "select",
                                "dynamic": true,
                                "required": true,
                            },
                            {
                                "key": "label",
                                "title": "Verified name",
                                "type": "string",
                                "hidden": true,
                                "required": true,
                            },
                            {
                                "key": "token",
                                "title": "Channel secret",
                                "type": "secret",
                                "required": true,
                            },
                        ],
                    },
                ],
            },
            "source": SOURCE,
        });
        if remote {
            package_input["manifest"]["instanceForm"] = json!(true);
            let choice = json!({
                "key": "channel", "title": "Channel", "type": "select", "dynamic": true, "remote": true,
            });
            package_input["manifest"]["settings"]
                .as_array_mut()
                .unwrap()
                .push(choice);
            package_input["manifest"]["resources"][1]["settings"][0]["remote"] = json!(true);
        }
        if cold {
            package_input["manifest"]["capabilities"]
                .as_array_mut()
                .unwrap()
                .push(json!("exec"));
            package_input["manifest"]["programs"] = json!([{
                "name": "probe",
                "installer": "programs/probe.luau",
            }]);
            package_input["modules"] = json!({"programs/probe.luau":r#"return {
                resolve = function() return {version = '1', url = 'https://api.example.test/probe', format = 'binary', executable = 'probe'} end,
                install = function(event)
                    local archive = cairn.download({url = event.release.url})
                    return cairn.archive.executable({archive = archive.id, format = 'binary', path = 'probe'})
                end,
            }"#});
        }
        let package = registry::install(&s, &package_input).await.unwrap();
        let cold_server = if cold {
            let publisher = Router::new().route(
                "/probe",
                axum::routing::get(|| async { "#!/bin/sh\nexit 0\n" }),
            );
            let (origin, server) = common::serve_locally(publisher).await;
            s.plugins.route_http_to(Some(origin));
            Some(server)
        } else {
            None
        };
        registry::approve(&s, "resourceforms", "1", text(&package, "digest"))
            .await
            .unwrap();
        if let Some(server) = cold_server {
            server.abort();
            // Simulate an approved package whose regenerable program cache was lost.
            std::fs::remove_dir_all(s.config.data_dir.join("plugin-programs/resourceforms"))
                .unwrap();
        }
        let input = json!({"name": "App", "token": "root-fixture", "secrets": {"token": "app-fixture"}, "enabled": false});
        let instance = if inert {
            let mut input = input.clone();
            input.as_object_mut().unwrap().remove("enabled");
            setup::save(&s, "resourceforms", "fixture-owner", &input, None)
                .await
                .unwrap()
        } else {
            connections::save(&s, "resourceforms", &input, None)
                .await
                .unwrap()
        };
        let path = format!(
            "/api/plugins/resourceforms/{}/{}/resources",
            if inert { "setup-drafts" } else { "connections" },
            text(&instance, "id")
        );
        let app = cairn_installation::http::router(s.clone()).await.unwrap();
        let hold = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (h, e, r) = (hold.clone(), entered.clone(), release.clone());
        let provider = Router::new().route("/{part}", axum::routing::get(move |axum::extract::Path(part): axum::extract::Path<String>, headers: axum::http::HeaderMap| {
            let (hold, entered, release) = (h.clone(), e.clone(), r.clone());
            async move {
                if (part == "parent" || part == "root-form") && hold.load(Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                let expected = match part.as_str() {
                    "root" | "root-form" => "Root root-fixture", "app" => "App app-fixture",
                    "parent" => "Parent parent-fixture", "child" => "Child child-fixture", _ => "",
                };
                axum::Json(json!({"ok": headers.get("authorization").is_some_and(|value| value == expected), "name": "General"}))
            }
        }));
        let (origin, server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let mut f = Self {
            _root: root,
            _service: s,
            app,
            path,
            root: instance,
            parent: Value::Null,
            hold,
            entered,
            release,
            server,
            inert,
        };
        let body = f.body(
            json!({"name": "Workspace", "enabled": false, "secrets": {"token": "parent-fixture"}}),
        );
        let response = request(
            &f.app,
            &format!("{}/workspaces", f.path),
            "POST",
            body,
            InstallationRole::Owner,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        f.parent = common::read_json(response).await;
        f
    }

    fn body(&self, mut body: Value) -> Value {
        if self.inert {
            body["setupRevision"] = self.root["revision"].clone();
        }
        body
    }

    fn input(&self) -> Value {
        self.body(json!({"name": "Channel", "parentId": self.parent["id"], "settings": {"channel": "C1", "label": "untrusted client label"}, "secrets": {"token": "child-fixture"}}))
    }
}

#[tokio::test]
async fn ordinary_root_changes_during_provider_io_refuse_forms_and_stale_saves() {
    for mode in ["preview", "lookup", "submit"] {
        for remove in [false, true] {
            let f = Fixture::with_features(false, false, true).await;
            f.hold.store(true, Ordering::SeqCst);
            let root_path = format!(
                "/api/plugins/resourceforms/connections/{}",
                text(&f.root, "id")
            );
            let mut body = json!({"instance": f.root["id"], "settings": {"channel": "hold-root"}});
            if mode == "lookup" {
                body["lookup"] = json!({"field": "channel", "query": "needle"});
            }
            if mode == "submit" {
                body["name"] = json!("Stale root form");
                body["enabled"] = json!(false);
            }
            let app = f.app.clone();
            let path = if mode == "submit" {
                root_path.clone()
            } else {
                "/api/plugins/resourceforms/form".to_owned()
            };
            let pending = tokio::spawn(async move {
                request(
                    &app,
                    &path,
                    if mode == "submit" { "PUT" } else { "POST" },
                    body,
                    InstallationRole::Owner,
                )
                .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
                .await
                .unwrap();
            let changed = if remove {
                json!({})
            } else {
                json!({
                    "name": "Changed root",
                    "enabled": false,
                    "token": "replacement-fixture",
                })
            };
            let response = request(
                &f.app,
                &root_path,
                if remove { "DELETE" } else { "PUT" },
                changed,
                InstallationRole::Owner,
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{}",
                common::read_text(response).await
            );
            f.hold.store(false, Ordering::SeqCst);
            f.release.notify_one();
            let response = pending.await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::CONFLICT,
                "mode={mode}, remove={remove}: {}",
                common::read_text(response).await
            );
            let current = f
                ._service
                .store
                .get(connections::KIND, text(&f.root, "id"))
                .await
                .unwrap();
            if remove {
                assert!(
                    current.is_none(),
                    "A stale form must not recreate the removed root"
                );
            } else {
                assert_eq!(current.unwrap()["name"], "Changed root");
            }
            f.server.abort();
        }
    }
}

#[tokio::test]
async fn remote_choice_lookups_work_in_all_four_configuration_scopes_without_deriving_values() {
    for inert in [false, true] {
        let f = Fixture::with_features(inert, false, true).await;
        let root_path = if inert {
            format!(
                "/api/plugins/resourceforms/setup-drafts/{}/form",
                text(&f.root, "id")
            )
        } else {
            "/api/plugins/resourceforms/form".to_owned()
        };
        let mut root_input = json!({"settings": {"channel": "saved-off-page"}});
        if inert {
            root_input["revision"] = f.root["revision"].clone();
        } else {
            root_input["instance"] = f.root["id"].clone();
        }
        for (path, base) in [
            (root_path, root_input),
            (format!("{}/channels/form", f.path), f.input()),
        ] {
            for (lookup, choice, next) in [
                (
                    json!({"field": "channel", "query": ""}),
                    json!("C1"),
                    json!("empty"),
                ),
                (
                    json!({"field": "channel", "query": "", "cursor": "empty"}),
                    Value::Null,
                    json!("last"),
                ),
                (
                    json!({"field": "channel", "query": "", "cursor": "last"}),
                    json!("C2"),
                    Value::Null,
                ),
                (
                    json!({"field": "channel", "query": "needle"}),
                    json!("found"),
                    Value::Null,
                ),
            ] {
                let mut input = base.clone();
                input["lookup"] = lookup.clone();
                let denied = request(
                    &f.app,
                    &path,
                    "POST",
                    input.clone(),
                    InstallationRole::Member,
                )
                .await;
                assert_eq!(denied.status(), StatusCode::FORBIDDEN);
                let response = request(&f.app, &path, "POST", input, InstallationRole::Owner).await;
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "{}",
                    common::read_text(response).await
                );
                let response = common::read_json(response).await;
                assert_eq!(response["lookup"], lookup);
                assert_eq!(response["options"]["channel"][0]["value"], choice);
                assert_eq!(response["pages"]["channel"]["nextCursor"], next);
                assert!(response["pages"]["channel"].get("nextCursor").is_some());
                assert!(response["values"].is_null() || response["values"] == json!({}));
                assert!(!response.to_string().contains("fixture"));
            }
            let mut input = base.clone();
            input["lookup"] = json!({"field": "channel", "query": "unknown"});
            let response = request(&f.app, &path, "POST", input, InstallationRole::Owner).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(common::read_json(response).await.get("pages").is_none());
            for (lookup, changed, status) in [
                (
                    json!({"field": "token", "query": ""}),
                    Value::Null,
                    StatusCode::BAD_REQUEST,
                ),
                (
                    json!({"field": "channel", "query": "x", "cursor": ""}),
                    Value::Null,
                    StatusCode::BAD_REQUEST,
                ),
                (
                    json!({"field": "channel", "query": "x".repeat(201)}),
                    Value::Null,
                    StatusCode::BAD_REQUEST,
                ),
                (
                    json!({"field": "channel", "query": ""}),
                    json!("channel"),
                    StatusCode::BAD_REQUEST,
                ),
                (
                    json!({"field": "channel", "query": "derive"}),
                    Value::Null,
                    StatusCode::BAD_GATEWAY,
                ),
                (
                    json!({"field": "channel", "query": "oversize"}),
                    Value::Null,
                    StatusCode::BAD_GATEWAY,
                ),
            ] {
                let mut input = base.clone();
                input["lookup"] = lookup;
                input["changed"] = changed;
                let response = request(&f.app, &path, "POST", input, InstallationRole::Owner).await;
                assert_eq!(
                    response.status(),
                    status,
                    "{}",
                    common::read_text(response).await
                );
            }
        }
        // A caller cannot turn final validation into a lookup by adding metadata.
        let mut input = f.input();
        input["lookup"] = json!({"field": "channel", "query": "derive"});
        let response = request(
            &f.app,
            &format!("{}/channels", f.path),
            "POST",
            input,
            InstallationRole::Owner,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = request(
            &f.app,
            &format!("{}/channels", f.path),
            "POST",
            f.input(),
            InstallationRole::Owner,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        assert_eq!(
            common::read_json(response).await["settings"]["label"],
            "General verified"
        );
        f.server.abort();
    }
}

#[tokio::test]
async fn active_and_inert_resource_forms_bind_app_parent_and_temporary_self_credentials() {
    for inert in [false, true] {
        let f = Fixture::new(inert).await;
        let path = format!("{}/channels", f.path);
        let denied = request(
            &f.app,
            &format!("{path}/form"),
            "POST",
            f.input(),
            InstallationRole::Member,
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let preview = request(
            &f.app,
            &format!("{path}/form"),
            "POST",
            f.input(),
            InstallationRole::Owner,
        )
        .await;
        assert_eq!(
            preview.status(),
            StatusCode::OK,
            "{}",
            common::read_text(preview).await
        );
        let preview = common::read_json(preview).await;
        assert_eq!(preview["options"]["channel"][0]["value"], "C1");
        assert_eq!(preview["values"]["label"], "General preview");
        assert!(!preview.to_string().contains("fixture"));
        let saved = request(&f.app, &path, "POST", f.input(), InstallationRole::Owner).await;
        assert_eq!(
            saved.status(),
            StatusCode::OK,
            "{}",
            common::read_text(saved).await
        );
        let saved = common::read_json(saved).await;
        assert_eq!(saved["settings"]["label"], "General verified");
        assert_eq!(saved["secretKeys"], json!(["token"]));
        let mut edit = f.input();
        edit["resource"] = saved["id"].clone();
        edit["revision"] = saved["revision"].clone();
        edit["secrets"] = json!({});
        let edited = request(
            &f.app,
            &format!("{path}/form"),
            "POST",
            edit,
            InstallationRole::Owner,
        )
        .await;
        assert_eq!(
            edited.status(),
            StatusCode::OK,
            "{}",
            common::read_text(edited).await
        );
        let mut invalid = f.input();
        invalid["settings"]["channel"] = json!("forbidden");
        let refused = request(&f.app, &path, "POST", invalid, InstallationRole::Owner).await;
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert!(
            common::read_text(refused)
                .await
                .contains("This channel is not available")
        );
        f.server.abort();
    }
}

#[tokio::test]
async fn parent_replacement_during_provider_io_refuses_atomic_resource_publication() {
    for inert in [false, true] {
        for lookup in [false, true] {
            let f = Fixture::with_features(inert, false, lookup).await;
            f.hold.store(true, Ordering::SeqCst);
            let (app, path, mut body) = (
                f.app.clone(),
                format!("{}/channels{}", f.path, if lookup { "/form" } else { "" }),
                f.input(),
            );
            if lookup {
                body["lookup"] = json!({"field": "channel", "query": ""});
            }
            let pending = tokio::spawn(async move {
                request(&app, &path, "POST", body, InstallationRole::Owner).await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
                .await
                .unwrap();
            let changed = f.body(json!({"name": "Workspace changed", "enabled": false, "revision": f.parent["revision"]}));
            let response = request(
                &f.app,
                &format!("{}/workspaces/{}", f.path, text(&f.parent, "id")),
                "PUT",
                changed,
                InstallationRole::Owner,
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{}",
                common::read_text(response).await
            );
            f.hold.store(false, Ordering::SeqCst);
            f.release.notify_one();
            let response = pending.await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::CONFLICT,
                "{}",
                common::read_text(response).await
            );
            let listed = request(
                &f.app,
                &format!("{}/channels", f.path),
                "GET",
                json!({}),
                InstallationRole::Owner,
            )
            .await;
            assert_eq!(common::read_json(listed).await["items"], json!([]));
            let fresh = request(
                &f.app,
                &format!("{}/channels", f.path),
                "POST",
                f.input(),
                InstallationRole::Owner,
            )
            .await;
            assert_eq!(
                fresh.status(),
                StatusCode::OK,
                "{}",
                common::read_text(fresh).await
            );
            f.server.abort();
        }
    }
}

#[tokio::test]
async fn scoped_forms_never_resolve_another_root_or_the_other_namespace() {
    for inert in [false, true] {
        let f = Fixture::new(inert).await;
        let other = if inert {
            connections::save(&f._service, "resourceforms", &json!({"name":"Other", "token":"root-fixture", "secrets":{"token":"app-fixture"}, "enabled":false}), None).await.unwrap()
        } else {
            setup::save(
                &f._service,
                "resourceforms",
                "fixture-owner",
                &json!({"name":"Other", "token":"root-fixture", "secrets":{"token":"app-fixture"}}),
                None,
            )
            .await
            .unwrap()
        };
        let path = format!(
            "/api/plugins/resourceforms/{}/{}/resources/workspaces",
            if inert { "connections" } else { "setup-drafts" },
            text(&other, "id")
        );
        let mut input = json!({"name":"Other workspace", "secrets":{"token":"parent-fixture"}});
        if !inert {
            input["setupRevision"] = other["revision"].clone();
        }
        let parent = request(&f.app, &path, "POST", input, InstallationRole::Owner).await;
        assert_eq!(parent.status(), StatusCode::OK);
        let parent = common::read_json(parent).await;
        let mut input = f.input();
        input["parentId"] = parent["id"].clone();
        let response = request(
            &f.app,
            &format!("{}/channels/form", f.path),
            "POST",
            input,
            InstallationRole::Owner,
        )
        .await;
        assert!(
            matches!(
                response.status(),
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ),
            "{}",
            common::read_text(response).await
        );
        // A stored identifier in the opposite namespace never becomes saved self.
        let mut input = f.input();
        input["resource"] = parent["id"].clone();
        input["revision"] = parent["revision"].clone();
        let response = request(
            &f.app,
            &format!("{}/channels/form", f.path),
            "POST",
            input,
            InstallationRole::Owner,
        )
        .await;
        assert!(matches!(
            response.status(),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ));
        f.server.abort();
    }
}

#[tokio::test]
async fn forms_discard_provider_results_after_root_package_owner_or_setup_expiry_changes() {
    for inert in [false, true] {
        for lookup in [false, true] {
            for change in ["root", "package", "owner", "expiry"] {
                if change == "expiry" && !inert {
                    continue;
                }
                let f = Fixture::with_features(inert, false, lookup).await;
                let changed = Arc::new(AtomicBool::new(false));
                let signal = changed.clone();
                let authority = Router::new().route("/api/relay/00000000-0000-4000-8000-000000000055/task-authors", axum::routing::get(move || {
                let signal = signal.clone();
                async move { axum::Json(json!({"owner":{"account_id":"fixture-owner","access_id":if signal.load(Ordering::SeqCst) {"replacement-access"} else {"fixture-owner"}},"members":[]})) }
            }));
                let (origin, authority_server) = common::serve_locally(authority).await;
                let identity_path = f
                    ._service
                    .config
                    .data_dir
                    .join("installation-relay/identity.json");
                let mut identity: Value =
                    serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
                identity["origin"] = json!(origin);
                std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
                f.hold.store(true, Ordering::SeqCst);
                let (app, path, mut input) = (
                    f.app.clone(),
                    format!("{}/channels{}", f.path, if lookup { "/form" } else { "" }),
                    f.input(),
                );
                if lookup {
                    input["lookup"] = json!({"field": "channel", "query": ""});
                }
                let pending = tokio::spawn(async move {
                    request(&app, &path, "POST", input, InstallationRole::Owner).await
                });
                tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
                    .await
                    .unwrap();
                match change {
                    "root" => {
                        if inert {
                            setup::save(
                                &f._service,
                                "resourceforms",
                                "fixture-owner",
                                &json!({"name":"Replaced", "revision": f.root["revision"]}),
                                Some(text(&f.root, "id")),
                            )
                            .await
                            .unwrap();
                        } else {
                            connections::save(
                                &f._service,
                                "resourceforms",
                                &json!({"name":"Replaced", "enabled": false}),
                                Some(text(&f.root, "id")),
                            )
                            .await
                            .unwrap();
                        }
                    }
                    "package" => {
                        let original = registry::active(&f._service, "resourceforms")
                            .await
                            .unwrap();
                        let mut manifest = serde_json::to_value(&original.manifest).unwrap();
                        manifest["version"] = json!("2");
                        let installed = registry::install(
                            &f._service,
                            &json!({"manifest":manifest,"source":original.source.main}),
                        )
                        .await
                        .unwrap();
                        registry::approve(
                            &f._service,
                            "resourceforms",
                            "2",
                            text(&installed, "digest"),
                        )
                        .await
                        .unwrap();
                    }
                    "owner" => {
                        changed.store(true, Ordering::SeqCst);
                    }
                    "expiry" => {
                        f._service
                            .store
                            .delete(&format!("plugin-setup:{}", text(&f.root, "id")))
                            .await
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
                f.hold.store(false, Ordering::SeqCst);
                f.release.notify_one();
                let response = pending.await.unwrap();
                let expected = if change == "owner" {
                    StatusCode::FORBIDDEN
                } else if inert && change != "package" {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::CONFLICT
                };
                assert_eq!(
                    response.status(),
                    expected,
                    "{inert}/{change}: {}",
                    common::read_text(response).await
                );
                let children = if inert {
                    f._service
                        .store
                        .keys(&format!("plugin-setup-resource:{}:", text(&f.root, "id")))
                        .await
                        .unwrap()
                        .into_iter()
                        .map(|(_, v)| v)
                        .collect::<Vec<_>>()
                } else {
                    f._service.store.list(resources::KIND).await.unwrap()
                };
                assert!(children.iter().all(|v| v["collection"] != "channels"));
                authority_server.abort();
                f.server.abort();
            }
        }
    }
}

#[tokio::test]
async fn cold_resource_forms_and_submit_use_pollable_owner_operations() {
    for inert in [false, true] {
        let f = Fixture::with_program(inert, true).await;
        for suffix in ["/form", ""] {
            let response = request(
                &f.app,
                &format!("{}/channels{suffix}", f.path),
                "POST",
                f.input(),
                InstallationRole::Owner,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let accepted = common::read_json(response).await;
            let id = text(&accepted, "operation");
            assert!(!id.is_empty(), "{accepted}");
            let completed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let state = f._service.plugins.operations.get(id).unwrap();
                    if state["state"] != "running" {
                        break state;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(completed["state"], "completed", "{completed}");
            if suffix.is_empty() {
                assert_eq!(completed["result"]["settings"]["label"], "General verified");
            } else {
                assert_eq!(completed["result"]["values"]["label"], "General preview");
            }
        }
        f.server.abort();
    }
}

#[tokio::test]
async fn resource_form_callbacks_keep_the_same_partial_values_as_root_forms() {
    for inert in [false, true] {
        let root = TempDir::new().unwrap();
        let s = Service::new(common::config(root.path())).await.unwrap();
        common::relay_fixture::claimed(&s).await.unwrap();
        s.plugins
            .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let fields = json!([
            {"key": "count", "title": "Count", "type": "number", "default": 5},
            {"key": "label", "title": "Label", "type": "string", "default": "preset"},
            {"key": "choice", "title": "Choice", "type": "select", "options": [{"value": "a", "label": "A"}], "default": "a"},
            {"key": "wasCleared", "title": "Cleared", "type": "boolean", "hidden": true},
            {"key": "submitted", "title": "Submitted", "type": "boolean", "hidden": true},
        ]);
        let package = registry::install(&s, &json!({
            "manifest": {
                "id": "partialforms", "name": "Partial forms", "version": "1", "apiVersion": 2,
                "capabilities": ["resources"], "instanceForm": true, "settings": fields,
                "resources": [{"name": "entries", "title": "Entries", "singular": "Entry", "form": true, "settings": fields}],
            },
            "source": r#"return {form = function(event)
                local values = event.values
                values.wasCleared = values.count == nil and values.label == nil and values.choice == nil
                values.submitted = event.submit == true
                return {values = values}
            end}"#,
        })).await.unwrap();
        registry::approve(&s, "partialforms", "1", text(&package, "digest"))
            .await
            .unwrap();
        let mut input =
            json!({"name": "App", "settings": {"count": 2, "label": "saved", "choice": "a"}});
        if !inert {
            input["enabled"] = json!(false);
        }
        let instance = if inert {
            setup::save(&s, "partialforms", "fixture-owner", &input, None)
                .await
                .unwrap()
        } else {
            connections::save(&s, "partialforms", &input, None)
                .await
                .unwrap()
        };
        let path = format!(
            "/api/plugins/partialforms/{}/{}",
            if inert { "setup-drafts" } else { "connections" },
            text(&instance, "id")
        );
        let app = cairn_installation::http::router(s.clone()).await.unwrap();
        for settings in [
            json!({}),
            json!({"count": null, "label": null, "choice": null}),
            json!({"count": 0, "label": "kept", "choice": "a"}),
        ] {
            let mut input = json!({"settings": settings});
            let root_path = if inert {
                input["revision"] = instance["revision"].clone();
                format!("{path}/form")
            } else {
                input["instance"] = instance["id"].clone();
                "/api/plugins/partialforms/form".to_owned()
            };
            let root_preview = request(
                &app,
                &root_path,
                "POST",
                input.clone(),
                InstallationRole::Owner,
            )
            .await;
            assert_eq!(
                root_preview.status(),
                StatusCode::OK,
                "{}",
                common::read_text(root_preview).await
            );
            let root_preview = common::read_json(root_preview).await;
            input.as_object_mut().unwrap().remove("instance");
            if inert {
                input.as_object_mut().unwrap().remove("revision");
                input["setupRevision"] = instance["revision"].clone();
            }
            let resource_preview = request(
                &app,
                &format!("{path}/resources/entries/form"),
                "POST",
                input,
                InstallationRole::Owner,
            )
            .await;
            assert_eq!(
                resource_preview.status(),
                StatusCode::OK,
                "{}",
                common::read_text(resource_preview).await
            );
            let resource_preview = common::read_json(resource_preview).await;
            assert_eq!(
                resource_preview["values"], root_preview["values"],
                "inert={inert}, settings={settings}"
            );
        }
        let mut input =
            json!({"name": "Entry", "settings": {"count": null, "label": null, "choice": null}});
        if inert {
            input["setupRevision"] = instance["revision"].clone();
        }
        let saved = request(
            &app,
            &format!("{path}/resources/entries"),
            "POST",
            input,
            InstallationRole::Owner,
        )
        .await;
        assert_eq!(
            saved.status(),
            StatusCode::OK,
            "{}",
            common::read_text(saved).await
        );
        let saved = common::read_json(saved).await;
        assert_eq!(saved["settings"]["wasCleared"], true);
        assert_eq!(saved["settings"]["submitted"], true);
        // Completion after the callback retains the existing server save defaults.
        assert_eq!(saved["settings"]["count"], 5);
    }
}

#[tokio::test]
async fn installed_files_are_rechecked_after_the_last_configuration_owner_lookup() {
    use std::sync::atomic::AtomicUsize;

    for inert in [false, true] {
        for operation in ["preview", "lookup", "resource", "root"] {
            let f = Fixture::with_features(inert, false, operation == "lookup").await;
            let calls = Arc::new(AtomicUsize::new(0));
            let hold_at = Arc::new(AtomicUsize::new(usize::MAX));
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let (count, gate, entering, releasing) = (
                calls.clone(),
                hold_at.clone(),
                entered.clone(),
                release.clone(),
            );
            let authority = Router::new().route(
                "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
                axum::routing::get(move || {
                    let (calls, hold_at, entered, release) = (
                        count.clone(),
                        gate.clone(),
                        entering.clone(),
                        releasing.clone(),
                    );
                    async move {
                        if calls.fetch_add(1, Ordering::SeqCst) + 1
                            == hold_at.load(Ordering::SeqCst)
                        {
                            entered.notify_one();
                            release.notified().await;
                        }
                        axum::Json(json!({
                            "owner": {"account_id": "fixture-owner", "access_id": "fixture-owner"},
                            "members": [],
                        }))
                    }
                }),
            );
            let (origin, authority_server) = common::serve_locally(authority).await;
            let identity_path = f
                ._service
                .config
                .data_dir
                .join("installation-relay/identity.json");
            let mut identity: Value =
                serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
            identity["origin"] = json!(origin);
            std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
            let (path, method, mut body) = if operation == "root" {
                let body = if inert {
                    json!({"name": "Owner update", "revision": f.root["revision"]})
                } else {
                    json!({"name": "Owner update", "enabled": false})
                };
                (
                    format!(
                        "/api/plugins/resourceforms/{}/{}",
                        if inert { "setup-drafts" } else { "connections" },
                        text(&f.root, "id")
                    ),
                    if inert { "PATCH" } else { "PUT" },
                    body,
                )
            } else {
                (
                    format!(
                        "{}/channels{}",
                        f.path,
                        if matches!(operation, "preview" | "lookup") {
                            "/form"
                        } else {
                            ""
                        }
                    ),
                    "POST",
                    f.input(),
                )
            };
            if operation == "lookup" {
                body["lookup"] = json!({"field": "channel", "query": ""});
            }
            // Calibrate through the real endpoint, so this gate follows its last
            // owner lookup rather than a hard-coded implementation call count.
            let baseline =
                request(&f.app, &path, method, body.clone(), InstallationRole::Owner).await;
            assert_eq!(baseline.status(), StatusCode::OK, "{inert}/{operation}");
            let baseline = common::read_json(baseline).await;
            let lookups = calls.swap(0, Ordering::SeqCst);
            assert!(lookups > 0);
            if inert && operation == "root" {
                body["revision"] = baseline["revision"].clone();
            }
            let approval = f
                ._service
                .store
                .get("plugin-packages", "package:resourceforms")
                .await
                .unwrap();
            let resources = f._service.store.list(resources::KIND).await.unwrap();
            let setups = f._service.store.keys("plugin-setup:").await.unwrap();
            hold_at.store(lookups, Ordering::SeqCst);
            let app = f.app.clone();
            let pending = tokio::spawn(async move {
                request(&app, &path, method, body, InstallationRole::Owner).await
            });
            tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
                .await
                .unwrap();
            let source = f
                ._service
                .config
                .data_dir
                .join("plugins/resourceforms/1/main.luau");
            let changed = format!(
                "{}\n-- changed during the last owner lookup\n",
                std::fs::read_to_string(&source).unwrap()
            );
            std::fs::write(source, changed).unwrap();
            release.notify_one();
            let response = pending.await.unwrap();
            let status = response.status();
            authority_server.abort();
            f.server.abort();
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{inert}/{operation}: stale installed files were accepted"
            );
            assert!(
                f._service
                    .store
                    .get("plugin-packages", "package:resourceforms")
                    .await
                    .unwrap()
                    == approval
            );
            assert!(
                f._service.store.list(resources::KIND).await.unwrap() == resources,
                "A refused configuration published resources"
            );
            assert!(
                f._service.store.keys("plugin-setup:").await.unwrap() == setups,
                "A refused configuration changed its setup"
            );
        }
    }
}
