//! Root consent stays inert until the exact owner setup is activated.
use super::*;
use axum::extract::Form;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Fixture {
    _root: TempDir,
    s: Arc<Service>,
    app: axum::Router,
    draft: Value,
    path: String,
    renewed: Arc<AtomicBool>,
    refreshes: Arc<AtomicUsize>,
    hold: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    interrupt: Arc<AtomicBool>,
    hold_verification: Arc<AtomicBool>,
    verification_entered: Arc<tokio::sync::Notify>,
    verification_release: Arc<tokio::sync::Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn manage(&self, action: &str, data: Value, flow: Option<&str>) -> Value {
        let token = common::relay_fixture::grant(&self.s, &["manage"]).await;
        let app = common::relay_fixture::router(self.s.clone()).await.unwrap();
        let mut args = json!({"pluginId": "setup-oauth", "action": action, "draftId": self.draft["id"], "data": data});
        if let Some(flow) = flow {
            args["flowId"] = json!(flow);
        }
        let response = common::send(
            &app,
            common::request("POST", "/mcp")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::from(
                    json!({
                        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": common::mcp_arguments("manage_plugin_setup", &args),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = common::read_json(response).await;
        assert_ne!(body["result"]["isError"], true, "{body}");
        assert!(!body.to_string().contains("draft-access-fixture"));
        assert!(!body.to_string().contains("draft-refresh-fixture"));
        body["result"]["structuredContent"]["result"].clone()
    }

    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        let s = Service::new(common::config(root.path())).await.unwrap();
        common::relay_fixture::claimed(&s).await.unwrap();
        s.plugins
            .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let renewed = Arc::new(AtomicBool::new(false));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let expected = renewed.clone();
        let calls = refreshes.clone();
        let hold = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (h, e, r) = (hold.clone(), entered.clone(), release.clone());
        let interrupt = Arc::new(AtomicBool::new(false));
        let interrupted = interrupt.clone();
        let hold_verification = Arc::new(AtomicBool::new(false));
        let verification_entered = Arc::new(tokio::sync::Notify::new());
        let verification_release = Arc::new(tokio::sync::Notify::new());
        let (vh, ve, vr) = (
            hold_verification.clone(),
            verification_entered.clone(),
            verification_release.clone(),
        );
        let provider = axum::Router::new()
            .route("/token", axum::routing::post(move |Form(fields): Form<HashMap<String, String>>| {
                let calls = calls.clone();
                let (hold, entered, release) = (h.clone(), e.clone(), r.clone());
                let interrupted = interrupted.clone();
                async move {
                if hold.load(Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                let refresh = fields["grant_type"] == "refresh_token";
                if refresh {
                    assert_eq!(fields["refresh_token"], "draft-refresh-fixture");
                    calls.fetch_add(1, Ordering::SeqCst);
                    if interrupted.swap(false, Ordering::SeqCst) {
                        return axum::response::Response::new(axum::body::Body::from_stream(futures_util::stream::iter([
                            Ok(b"{\"access_token\":".to_vec()),
                            Err(std::io::Error::other("Synthetic token body interruption")),
                        ])));
                    }
                } else {
                    assert_eq!(fields["grant_type"], "authorization_code");
                    assert!(!fields["code_verifier"].is_empty());
                }
                assert_eq!(fields["client_id"], "draft-client-fixture");
                axum::Json(json!({"access_token": if refresh {"draft-renewed-fixture"} else {"draft-access-fixture"}, "refresh_token": "draft-refresh-fixture",
                    "token_type": "Bearer", "expires_in": 3600, "scope": "app:read"})).into_response()
            }}))
            .route("/verify", axum::routing::get(move |headers: axum::http::HeaderMap| {
                let expected = expected.clone();
                let (hold, entered, release) = (vh.clone(), ve.clone(), vr.clone());
                async move {
                assert_eq!(headers["authorization"], if expected.load(Ordering::SeqCst) {"Bearer draft-renewed-fixture"} else {"Bearer draft-access-fixture"});
                if hold.swap(false, Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                axum::Json(json!({"ok": true}))
            }}));
        let (origin, server) = common::serve_locally(provider).await;
        s.plugins.route_http_to(Some(origin));
        let installed = registry::install(&s, &json!({
            "manifest": {"id": "setup-oauth", "name": "Setup OAuth", "version": "1", "apiVersion": 2,
                "capabilities": ["http", "resources"], "hosts": ["api.example.test"], "instanceForm": true,
                "credential": {"label": "App account", "header": "authorization", "prefix": "Bearer ",
                    "oauth": {"authorizationUrl": "https://api.example.test/authorize", "tokenUrl": "https://api.example.test/token",
                        "clientIdSetting": "clientId", "scopes": ["app:read"], "requireRefreshToken": true}},
                "settings": [{"key": "clientId", "title": "Client ID", "type": "string", "required": true},
                    {"key": "verified", "title": "Verification", "type": "string", "hidden": true, "required": true}],
                "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace", "form": true,
                    "settings": [{"key": "externalId", "title": "Workspace", "type": "select", "dynamic": true}]}]},
            "source": r#"return {form = function(event)
                if not event.credential and not event.resource then return nil end
                local response = cairn.http.request({url = 'https://api.example.test/verify', auth = true})
                local label = response.json.ok and 'Verified' or 'Denied'
                if event.resource then return {options = {externalId = {{value = 'T1', label = label}}}} end
                return {values = {verified = label}}
            end}"#
        })).await.unwrap();
        registry::approve(&s, "setup-oauth", "1", text(&installed, "digest"))
            .await
            .unwrap();
        let app = cairn_installation::http::router(s.clone()).await.unwrap();
        let response = common::send(&app, request("POST", "/api/plugins/setup-oauth/setup-drafts",
            json!({"name": "Inert OAuth app", "settings": {"clientId": "draft-client-fixture"}}))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let draft = common::read_json(response).await;
        let path = format!(
            "/api/plugins/setup-oauth/setup-drafts/{}",
            text(&draft, "id")
        );
        Self {
            _root: root,
            s,
            app,
            draft,
            path,
            renewed,
            refreshes,
            hold,
            entered,
            release,
            interrupt,
            hold_verification,
            verification_entered,
            verification_release,
            server,
        }
    }

    async fn start(&self) -> Value {
        let response = common::send(
            &self.app,
            request(
                "POST",
                &format!("{}/oauth", self.path),
                json!({"revision": self.draft["revision"]}),
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        common::read_json(response).await
    }

    async fn sign_in(&self) -> Value {
        let flow = self.start().await;
        let response = common::send(
            &self.app,
            request(
                "POST",
                "/api/plugins/oauth/callback",
                json!({"state": flow["id"], "code": "draft-code-fixture"}),
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        flow
    }

    async fn save_flow(&self, flow: &Value) -> Value {
        let response = common::send(&self.app, request("PATCH", &self.path, json!({
            "revision": self.draft["revision"], "name": "Inert OAuth app", "settings": self.draft["settings"],
            "credentialFlow": flow["id"],
        }))).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        common::read_json(response).await
    }

    async fn expire_token(&self, draft: &Value) {
        let key = cairn_installation::plugins::connections::credential_key(text(draft, "id"));
        let mut saved = self.s.vault.get(&key).await.unwrap().unwrap();
        // Prepare expiry of the actual code-grant credential; provider calls remain real.
        saved["oauth"]["expiresAt"] = json!(cairn_installation::config::now() - 1);
        self.s.vault.set(&key, &saved).await.unwrap();
        self.renewed.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn owner_mcp_can_finish_root_consent_and_activate_without_receiving_credentials() {
    let f = Fixture::new().await;
    let flow = f
        .manage("signin", json!({"revision": f.draft["revision"]}), None)
        .await;
    assert_eq!(flow["state"], "pending");
    let response = common::send(
        &f.app,
        request(
            "POST",
            "/api/plugins/oauth/callback",
            json!({
                "state": flow["id"], "code": "draft-code-fixture",
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let completed = f
        .manage("signin_status", json!({}), Some(text(&flow, "id")))
        .await;
    assert_eq!(completed["state"], "completed");
    let saved = f.manage("save", json!({
        "revision": f.draft["revision"], "name": f.draft["name"], "settings": f.draft["settings"],
        "credentialFlow": flow["id"],
    }), None).await;
    assert_eq!(saved["credential"], true);
    let form = f
        .manage("form", json!({"revision": saved["revision"]}), None)
        .await;
    assert_eq!(form["values"]["verified"], "Verified");
    let active = f
        .manage(
            "activate",
            json!({"revision": saved["revision"], "enabled": true}),
            None,
        )
        .await;
    assert_eq!(active["settings"]["verified"], "Verified");
    assert_eq!(active["agentIds"], json!([]));
}

#[tokio::test]
async fn inert_resource_forms_use_the_same_renewed_root_oauth_credential() {
    let f = Fixture::new().await;
    let flow = f.sign_in().await;
    let draft = f.save_flow(&flow).await;
    f.expire_token(&draft).await;
    let response = common::send(
        &f.app,
        request(
            "POST",
            &format!("{}/resources/workspace/form", f.path),
            json!({"setupRevision": draft["revision"], "settings": {}}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let form = common::read_json(response).await;
    assert_eq!(form["options"]["externalId"][0]["label"], "Verified");
    assert!(!form.to_string().contains("draft-renewed-fixture"));
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    let response = common::send(
        &f.app,
        request(
            "GET",
            &format!(
                "/api/plugins/setup-oauth/connections/{}",
                text(&draft, "id")
            ),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_legacy_pasted_oauth_draft_requires_new_consent_before_activation() {
    let f = Fixture::new().await;
    // Prepared earlier-draft storage: the old setup API accepted pasted OAuth.
    let key = cairn_installation::plugins::connections::credential_key(text(&f.draft, "id"));
    f.s.vault
        .set(&key, &json!({"token": "draft-access-fixture"}))
        .await
        .unwrap();
    let setup_key = format!("plugin-setup:{}", text(&f.draft, "id"));
    let mut snapshot = f.s.store.kv(&setup_key).await.unwrap().unwrap();
    snapshot["credential"] = json!(true);
    f.s.store
        .set(&setup_key, snapshot, f.draft["expiresAt"].as_i64())
        .await
        .unwrap();
    let response = common::send(
        &f.app,
        request(
            "POST",
            &format!("{}/activate", f.path),
            json!({"revision": f.draft["revision"], "enabled": true}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "{}",
        common::read_text(response).await
    );
    let response = common::send(&f.app, request("GET", &f.path, Value::Null)).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn activation_waits_for_a_concurrent_root_renewal_to_finish() {
    let f = Fixture::new().await;
    let flow = f.sign_in().await;
    let draft = f.save_flow(&flow).await;
    f.hold_verification.store(true, Ordering::SeqCst);
    let (app, path, revision) = (f.app.clone(), f.path.clone(), draft["revision"].clone());
    let activating = tokio::spawn(async move {
        common::send(
            &app,
            request(
                "POST",
                &format!("{path}/activate"),
                json!({"revision": revision, "enabled": true}),
            ),
        )
        .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        f.verification_entered.notified(),
    )
    .await
    .unwrap();
    f.expire_token(&draft).await;
    f.hold.store(true, Ordering::SeqCst);
    let (app, path, body) = (
        f.app.clone(),
        f.path.clone(),
        json!({"revision": draft["revision"], "settings": draft["settings"]}),
    );
    let refreshing = tokio::spawn(async move {
        common::send(&app, request("POST", &format!("{path}/form"), body)).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), f.entered.notified())
        .await
        .unwrap();
    f.verification_release.notify_one();
    let activation = activating.await.unwrap();
    let status = activation.status();
    let body = common::read_json(activation).await;
    f.release.notify_one();
    let refresh = refreshing.await.unwrap();
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "Activation promoted an unfinished refresh: {body}"
    );
    assert_eq!(
        refresh.status(),
        StatusCode::OK,
        "{}",
        common::read_text(refresh).await
    );
    let response = common::send(&f.app, request("GET", &f.path, Value::Null)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = common::send(
        &f.app,
        request(
            "POST",
            &format!("{}/activate", f.path),
            json!({"revision": draft["revision"], "enabled": true}),
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
async fn an_uncertain_setup_refresh_cannot_be_replayed_or_activated() {
    let f = Fixture::new().await;
    let flow = f.sign_in().await;
    let draft = f.save_flow(&flow).await;
    f.expire_token(&draft).await;
    f.interrupt.store(true, Ordering::SeqCst);
    for expected in [StatusCode::BAD_GATEWAY, StatusCode::UNAUTHORIZED] {
        let response = common::send(
            &f.app,
            request(
                "POST",
                &format!("{}/form", f.path),
                json!({"revision": draft["revision"], "settings": draft["settings"]}),
            ),
        )
        .await;
        assert_eq!(
            response.status(),
            expected,
            "{}",
            common::read_text(response).await
        );
    }
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    let response = common::send(
        &f.app,
        request(
            "POST",
            &format!("{}/activate", f.path),
            json!({"revision": draft["revision"], "enabled": true}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    let response = common::send(&f.app, request("GET", &f.path, Value::Null)).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn refreshing_setup_oauth_preserves_its_original_expiration() {
    let f = Fixture::new().await;
    let flow = f.sign_in().await;
    let draft = f.save_flow(&flow).await;
    let deadline = cairn_installation::config::now() + 2000;
    let setup_key = format!("plugin-setup:{}", text(&draft, "id"));
    let mut snapshot = f.s.store.kv(&setup_key).await.unwrap().unwrap();
    snapshot["expiresAt"] = json!(deadline);
    snapshot["createdAt"] = json!(deadline - 86_400_000);
    f.s.store
        .set(&setup_key, snapshot, Some(deadline))
        .await
        .unwrap();
    f.expire_token(&draft).await;
    let response = common::send(
        &f.app,
        request(
            "POST",
            &format!("{}/form", f.path),
            json!({"revision": draft["revision"], "settings": draft["settings"]}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    let remaining = (deadline - cairn_installation::config::now()).max(0) as u64;
    tokio::time::sleep(std::time::Duration::from_millis(remaining + 30)).await;
    let response = common::send(&f.app, request("GET", &f.path, Value::Null)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let key = cairn_installation::plugins::connections::credential_key(text(&draft, "id"));
    assert!(
        f.s.vault.get(&key).await.unwrap().is_none(),
        "Renewal extended a setup secret's lifetime"
    );
}

#[tokio::test]
async fn an_in_flight_root_consent_cannot_complete_after_setup_or_authority_changes() {
    for change in ["edit", "delete", "owner", "package"] {
        let f = Fixture::new().await;
        let flow = f.start().await;
        f.hold.store(true, Ordering::SeqCst);
        let app = f.app.clone();
        let body = json!({"state": flow["id"], "code": "draft-code-fixture"});
        let pending = tokio::spawn(async move {
            common::send(&app, request("POST", "/api/plugins/oauth/callback", body)).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), f.entered.notified())
            .await
            .unwrap();
        let mut authority = None;
        match change {
            "edit" | "delete" => {
                let method = if change == "edit" { "PATCH" } else { "DELETE" };
                let body = if change == "edit" {
                    json!({"revision": f.draft["revision"], "name": "Replaced setup", "settings": f.draft["settings"]})
                } else {
                    json!({"revision": f.draft["revision"]})
                };
                let response = common::send(&f.app, request(method, &f.path, body)).await;
                assert_eq!(response.status(), StatusCode::OK);
            }
            "owner" => {
                authority = Some(
                    common::relay_fixture::replace_owner_access(&f.s, "replacement-owner-access")
                        .await,
                );
            }
            "package" => {
                let package = registry::active(&f.s, "setup-oauth").await.unwrap();
                let mut manifest = serde_json::to_value(package.manifest).unwrap();
                manifest["version"] = json!("2");
                let installed =
                    registry::install(&f.s, &json!({"manifest": manifest, "source": "return {}"}))
                        .await
                        .unwrap();
                registry::approve(&f.s, "setup-oauth", "2", text(&installed, "digest"))
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        f.release.notify_one();
        let response = pending.await.unwrap();
        assert!(
            !response.status().is_success(),
            "Late consent accepted after {change}"
        );
        let response = common::send(
            &f.app,
            request(
                "GET",
                &format!("/api/plugins/setup-oauth/oauth/{}", text(&flow, "id")),
                Value::Null,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            common::read_json(response).await["state"],
            "failed",
            "{change}"
        );
        if let Some(server) = authority {
            server.abort();
        }
    }
}

#[tokio::test]
async fn setup_consent_cannot_be_used_for_another_draft_or_an_ordinary_connection() {
    let f = Fixture::new().await;
    let flow = f.sign_in().await;
    let response = common::send(
        &f.app,
        request(
            "POST",
            "/api/plugins/setup-oauth/setup-drafts",
            json!({"name": "Other draft", "settings": f.draft["settings"]}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let other = common::read_json(response).await;
    let response = common::send(&f.app, request("PATCH", &format!("/api/plugins/setup-oauth/setup-drafts/{}", text(&other,"id")), json!({
        "revision": other["revision"], "name": other["name"], "settings": other["settings"], "credentialFlow": flow["id"],
    }))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = common::send(&f.app, request("POST", "/api/plugins/setup-oauth/connections", json!({
        "name": "Unrelated account", "enabled": true, "settings": f.draft["settings"], "credentialFlow": flow["id"],
    }))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    // Refused cross-target consumption leaves the original consent available.
    assert_eq!(f.save_flow(&flow).await["credential"], true);
}

#[tokio::test]
async fn setup_forms_renew_expired_root_oauth_without_activating_the_instance() {
    let f = Fixture::new().await;
    let flow = f.sign_in().await;
    let draft = f.save_flow(&flow).await;
    f.expire_token(&draft).await;
    let response = common::send(
        &f.app,
        request(
            "POST",
            &format!("{}/form", f.path),
            json!({"revision": draft["revision"], "settings": draft["settings"]}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    assert_eq!(
        common::read_json(response).await["values"]["verified"],
        "Verified"
    );
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    let response = common::send(
        &f.app,
        request(
            "GET",
            &format!(
                "/api/plugins/setup-oauth/connections/{}",
                text(&draft, "id")
            ),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn changing_the_saved_oauth_client_requires_consent_again_before_activation() {
    let f = Fixture::new().await;
    let flow = f.sign_in().await;
    let draft = f.save_flow(&flow).await;
    let response = common::send(&f.app, request("PATCH", &f.path, json!({
        "revision": draft["revision"], "name": draft["name"], "settings": {"clientId": "another-client-fixture"},
    }))).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let changed = common::read_json(response).await;
    assert_eq!(changed["credential"], false);
    let response = common::send(
        &f.app,
        request(
            "POST",
            &format!("{}/activate", f.path),
            json!({"revision": changed["revision"], "enabled": true}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oauth_setup_rejects_pasted_tokens_instead_of_bypassing_consent() {
    let f = Fixture::new().await;
    let response = common::send(&f.app, request("PATCH", &f.path, json!({
        "revision": f.draft["revision"], "name": "Inert OAuth app", "settings": f.draft["settings"],
        "token": "pasted-oauth-fixture",
    }))).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn root_oauth_consent_can_be_saved_and_used_before_activating_a_setup_draft() {
    let f = Fixture::new().await;
    let flow = f.sign_in().await;
    let response = common::send(&f.app, request("PATCH", &f.path, json!({
        "revision": f.draft["revision"], "name": "Inert OAuth app", "settings": f.draft["settings"],
        "credentialFlow": flow["id"],
    }))).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let draft = common::read_json(response).await;
    assert_eq!(draft["credential"], true);
    assert!(!draft.to_string().contains("draft-access-fixture"));
    let response = common::send(
        &f.app,
        request(
            "GET",
            &format!(
                "/api/plugins/setup-oauth/connections/{}",
                text(&draft, "id")
            ),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = common::send(
        &f.app,
        request(
            "POST",
            &format!("{}/form", f.path),
            json!({"revision": draft["revision"], "settings": draft["settings"]}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let form = common::read_json(response).await;
    assert_eq!(form["values"]["verified"], "Verified");
    let response = common::send(
        &f.app,
        request(
            "POST",
            &format!("{}/activate", f.path),
            json!({"revision": draft["revision"], "enabled": true}),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let active = common::read_json(response).await;
    assert_eq!(active["id"], draft["id"]);
    assert_eq!(active["settings"]["verified"], "Verified");
    assert!(!active.to_string().contains("draft-refresh-fixture"));
    let response = common::send(&f.app, request("GET", &f.path, Value::Null)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "local encrypted credential TTL storage comparison; run alone"]
async fn measure_setup_credential_expiration_storage() {
    use cairn_installation::{plugins::connections::credential_key, store::Store};
    fn cpu_ms() -> f64 {
        let mut value = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut value) },
            0
        );
        value.tv_sec as f64 * 1000.0 + value.tv_nsec as f64 / 1_000_000.0
    }
    let f = Fixture::new().await;
    let flow = f.sign_in().await;
    let draft = f.save_flow(&flow).await;
    let key = credential_key(text(&draft, "id"));
    let value = f.s.vault.get(&key).await.unwrap().unwrap();
    let mut cases = Vec::new();
    for (name, expiry) in [
        ("live credential reference", None),
        ("inert setup expiry", draft["expiresAt"].as_i64()),
    ] {
        let mut writes = Vec::new();
        for sample in 0..6 {
            let cpu = cpu_ms();
            let at = std::time::Instant::now();
            let encrypted = f.s.vault.encrypt(&key, &value).unwrap();
            f.s.store
                .set(&format!("mcp-secret:{key}"), encrypted, expiry)
                .await
                .unwrap();
            writes.push(json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64() * 1000.0, "processCpuMs": cpu_ms()-cpu}));
        }
        let stored_key = format!("mcp-secret:{key}");
        let encrypted = f.s.store.kv(&stored_key).await.unwrap().unwrap();
        let encrypted_bytes = serde_json::to_vec(&encrypted).unwrap().len();
        let reader = Store::open(&f.s.config.data_dir).unwrap();
        let reads = reader.read(move |db| {
            let mut reads = Vec::new();
            for sample in 0..6 {
                let cpu = cpu_ms();
                let at = std::time::Instant::now();
                let read = db.kv(&stored_key)?.unwrap();
                reads.push(json!({"sample": sample, "wallMs": at.elapsed().as_secs_f64()*1000.0, "processCpuMs": cpu_ms()-cpu,
                    "returnedEncryptedJsonBytes": serde_json::to_vec(&read)?.len()}));
            }
            Ok(reads)
        }).await.unwrap();
        cases.push(json!({"name": name, "credentialJsonBytes": serde_json::to_vec(&value).unwrap().len(),
            "encryptedValueJsonBytes": encrypted_bytes, "expiration": if expiry.is_some() {"original setup deadline"} else {"none"},
            "reads": reads, "writes": writes}));
    }
    eprintln!(
        "SETUP_CREDENTIAL_EXPIRATION_STORAGE {}",
        json!({"cases": cases,
        "workload": "same actual synthetic code-grant credential; six native encryption+KV writes and six encrypted KV reads per case; reference first",
        "cache": "one reopened reader per case; first connection-cold plus five warm reads; OS caches retained",
        "limits": "bytes exclude keys, SQL rows, expiration column, indexes, WAL and pages; writes exclude authority and CAS guards; reads bypass decryption; no physical cold read, full renewal or production latency claim",
        "host": {"parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
            "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(), "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()}})
    );
}
