//! Requester authority across root credential renewal and HTTP receipt writes.
//! Reuses the signed admission and real worker of the routed-run fixture.
use super::*;
use cairn_installation::plugins::{oauth::credential, panels};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

struct Account {
    instance: Value,
    key: String,
    refreshes: Arc<AtomicUsize>,
    expires: Arc<AtomicU64>,
    hold_refresh: Arc<AtomicBool>,
    refresh_entered: Arc<Notify>,
    refresh_release: Arc<Notify>,
    writes: Arc<AtomicUsize>,
    tool_entered: Arc<Notify>,
    hold_response: Arc<AtomicBool>,
    response_entered: Arc<Notify>,
    response_release: Arc<Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Account {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Account {
    async fn new(f: &Started) -> Self {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let expires = Arc::new(AtomicU64::new(3600));
        let hold_refresh = Arc::new(AtomicBool::new(false));
        let refresh_entered = Arc::new(Notify::new());
        let refresh_release = Arc::new(Notify::new());
        let (count, expiry, hold, entered, release) = (
            refreshes.clone(),
            expires.clone(),
            hold_refresh.clone(),
            refresh_entered.clone(),
            refresh_release.clone(),
        );
        let token =
            move |axum::extract::Form(fields): axum::extract::Form<HashMap<String, String>>| {
                let (count, expiry, hold, entered, release) = (
                    count.clone(),
                    expiry.clone(),
                    hold.clone(),
                    entered.clone(),
                    release.clone(),
                );
                async move {
                    let revision = if fields["grant_type"] == "refresh_token" {
                        let revision = count.fetch_add(1, Ordering::SeqCst) + 1;
                        assert_eq!(
                            fields["refresh_token"],
                            format!("fixture-refresh-{}", revision - 1)
                        );
                        if revision == 1 && hold.load(Ordering::SeqCst) {
                            entered.notify_one();
                            release.notified().await;
                        }
                        revision
                    } else {
                        assert_eq!(fields["code"], "fixture-code");
                        0
                    };
                    axum::Json(json!({
                        "access_token": format!("fixture-access-{revision}"),
                        "refresh_token": format!("fixture-refresh-{revision}"),
                        "token_type": "Bearer",
                        "expires_in": expiry.load(Ordering::SeqCst),
                        "scope": "account:read",
                    }))
                }
            };
        let writes = Arc::new(AtomicUsize::new(0));
        let hold_response = Arc::new(AtomicBool::new(false));
        let response_entered = Arc::new(Notify::new());
        let response_release = Arc::new(Notify::new());
        let (sent, hold, entered, release) = (
            writes.clone(),
            hold_response.clone(),
            response_entered.clone(),
            response_release.clone(),
        );
        let mutate = move || {
            let (sent, hold, entered, release) =
                (sent.clone(), hold.clone(), entered.clone(), release.clone());
            async move {
                sent.fetch_add(1, Ordering::SeqCst);
                if hold.load(Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                axum::Json(json!({"private": "retired-receipt-result"}))
            }
        };
        let tool_entered = Arc::new(Notify::new());
        let ready = tool_entered.clone();
        let provider = axum::Router::new()
            .route(
                "/ready",
                axum::routing::get(move || {
                    let ready = ready.clone();
                    async move {
                        ready.notify_one();
                        axum::Json(json!({"ok": true}))
                    }
                }),
            )
            .route("/token", axum::routing::post(token))
            .route(
                "/probe",
                axum::routing::get(|| async { axum::Json(json!({"ok": true})) }),
            )
            .route("/write", axum::routing::post(mutate));
        let (origin, server) = common::serve_locally(provider).await;
        f.s.plugins.route_http_to(Some(origin));
        let package = registry::install(&f.s, &json!({
            "manifest": {
                "id": "caller-oauth",
                "name": "Caller OAuth",
                "version": "1",
                "apiVersion": 2,
                "instanceTest": true,
                "agentAccess": {},
                "capabilities": ["http"],
                "hosts": ["api.example.test"],
                "credential": {
                    "label": "Account",
                    "header": "authorization",
                    "prefix": "Bearer ",
                    "oauth": {
                        "authorizationUrl": "https://api.example.test/authorize",
                        "tokenUrl": "https://api.example.test/token",
                        "clientIdSetting": "clientId",
                        "scopes": ["account:read"],
                        "requireRefreshToken": true,
                    },
                },
                "settings": [{"key": "clientId", "title": "Client", "type": "string", "required": true}],
                "tools": [{
                    "name": "caller_oauth_probe",
                    "description": "Use the granted account",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"write": {"type": "boolean"}, "key": {"type": "string"}, "reportEntered": {"type": "boolean"}},
                    },
                }],
            },
            "source": r#"return {
                test = function(event)
                    return {ok = cairn.http.request({url = 'https://api.example.test/probe', auth = true}).json.ok}
                end,
                tool = function(event)
                    if event.input.reportEntered then cairn.http.request({url = 'https://api.example.test/ready'}) end
                    if event.input.write then
                        return cairn.http.request({url = 'https://api.example.test/write', method = 'POST',
                            auth = true, json = {value = 'fixture'}, idempotencyKey = event.input.key}).json
                    end
                    return cairn.http.request({url = 'https://api.example.test/probe', auth = true}).json
                end,
            }"#,
        })).await.unwrap();
        registry::approve(&f.s, "caller-oauth", "1", text(&package, "digest"))
            .await
            .unwrap();
        let flow = credential::start(
            &f.s,
            "caller-oauth",
            &json!({
                "settings": {"clientId": "fixture-client"},
            }),
            "cairn-account:fixture-owner",
        )
        .await
        .unwrap();
        credential::callback(
            &f.s,
            &json!({"state": flow["id"], "code": "fixture-code"}),
            "cairn-account:fixture-owner",
        )
        .await
        .unwrap();
        let instance = connections::save(
            &f.s,
            "caller-oauth",
            &json!({
                "name": "Granted account",
                "enabled": true,
                "agentIds": [f.run["snapshot"]["agent"]["id"]],
                "settings": {"clientId": "fixture-client"},
                "credentialFlow": flow["id"],
            }),
            None,
        )
        .await
        .unwrap();
        let key = connections::credential_key(text(&instance, "id"));
        assert!(
            agents::call_tool(&f.s, &f.bearer, "caller_oauth_probe", &json!({}))
                .await
                .unwrap()
                .unwrap()["ok"]
                .as_bool()
                .unwrap()
        );
        Self {
            instance,
            key,
            refreshes,
            expires,
            hold_refresh,
            refresh_entered,
            refresh_release,
            writes,
            tool_entered,
            hold_response,
            response_entered,
            response_release,
            server,
        }
    }

    async fn expire(&self, s: &Service) {
        let mut saved = s.vault.get(&self.key).await.unwrap().unwrap();
        saved["oauth"]["expiresAt"] = json!(now() - 1);
        s.vault.set(&self.key, &saved).await.unwrap();
    }
}

async fn retire_source(f: &Started) {
    let binding = &f.delivery["routing"];
    let routes = cairn_installation::plugins::routes::list(
        &f.s,
        "router",
        &f.connection.id,
        "workspace",
        text(binding, "resource"),
        None,
    )
    .await
    .unwrap();
    let route = routes["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|route| route["id"] == binding["id"])
        .unwrap();
    cairn_installation::plugins::routes::people(
        &f.s,
        "router",
        &f.connection.id,
        "workspace",
        text(binding, "resource"),
        text(binding, "id"),
        "fixture-owner",
        "alice",
        &json!({"action": "remove", "revision": route["revision"]}),
    )
    .await
    .unwrap();
}

async fn reached(signal: &Notify) {
    tokio::time::timeout(std::time::Duration::from_secs(5), signal.notified())
        .await
        .unwrap();
}

async fn root_renewal_after_change(phase: &str) {
    let f = Started::new(None).await;
    let account = Account::new(&f).await;
    account.expire(&f.s).await;
    account.expires.store(1, Ordering::SeqCst);
    account.hold_refresh.store(true, Ordering::SeqCst);
    let before = f.s.vault.get(&account.key).await.unwrap().unwrap();
    let owner = if phase == "mutex" {
        let (s, instance) = (f.s.clone(), text(&account.instance, "id").to_owned());
        let owner = tokio::spawn(async move { panels::test(&s, "caller-oauth", &instance).await });
        reached(&account.refresh_entered).await;
        Some(owner)
    } else {
        None
    };
    let (s, bearer) = (f.s.clone(), f.bearer.clone());
    let tool = tokio::spawn(async move {
        agents::call_tool(
            &s,
            &bearer,
            "caller_oauth_probe",
            &json!({"reportEntered": true}),
        )
        .await
    });
    reached(&account.tool_entered).await;
    if phase == "provider" {
        reached(&account.refresh_entered).await;
    } else {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let pending = !tool.is_finished();
    retire_source(&f).await;
    account.refresh_release.notify_one();
    if let Some(owner) = owner {
        owner.await.unwrap().unwrap();
    }
    let result = tool.await.unwrap();
    assert!(
        pending,
        "The original tool did not reach its held {phase} boundary"
    );
    assert!(result.is_err());
    assert_eq!(
        account.refreshes.load(Ordering::SeqCst),
        1,
        "A retired caller started another refresh after the mutex wait"
    );
    if phase == "provider" {
        let current = f.s.vault.get(&account.key).await.unwrap().unwrap();
        assert_eq!(
            current["token"], before["token"],
            "Retired work published the successor credential"
        );
        assert_eq!(current["oauth"]["refreshing"], true);
        assert!(current["oauth"].get("refreshToken").is_none());
        assert!(
            panels::test(&f.s, "caller-oauth", text(&account.instance, "id"))
                .await
                .is_err()
        );
        assert_eq!(
            account.refreshes.load(Ordering::SeqCst),
            1,
            "An uncertain refresh was repeated"
        );
    }
}

#[tokio::test]
async fn root_renewal_waiting_for_the_account_mutex_refuses_a_revoked_requester() {
    root_renewal_after_change("mutex").await;
}

#[tokio::test]
async fn root_renewal_response_cannot_be_saved_after_the_requester_is_revoked() {
    root_renewal_after_change("provider").await;
}

async fn hold_writer(f: &Started) -> (std::sync::mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
    let (release, blocked) = std::sync::mpsc::channel();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (s, route) = (f.s.clone(), text(&f.delivery["routing"], "id").to_owned());
    let writer = tokio::spawn(async move {
        s.store
            .transaction(move |db| {
                entered.send(()).unwrap();
                blocked
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                db.remove("plugin-routes", &route)
            })
            .await
            .unwrap();
    });
    ready.await.unwrap();
    (release, writer)
}

async fn receipt_after_change(phase: &str) {
    let f = Started::new(None).await;
    let account = Account::new(&f).await;
    account
        .hold_response
        .store(phase == "complete", Ordering::SeqCst);
    let barrier = if phase == "claim" {
        Some(hold_writer(&f).await)
    } else {
        None
    };
    let (s, bearer) = (f.s.clone(), f.bearer.clone());
    let tool = tokio::spawn(async move {
        agents::call_tool(
            &s,
            &bearer,
            "caller_oauth_probe",
            &json!({
                "write": true, "key": "source-receipt",
            }),
        )
        .await
    });
    let (release, writer) = if let Some(barrier) = barrier {
        barrier
    } else {
        reached(&account.response_entered).await;
        let barrier = hold_writer(&f).await;
        account.response_release.notify_one();
        barrier
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let pending = !tool.is_finished();
    release.send(()).unwrap();
    writer.await.unwrap();
    let result = tool.await.unwrap();
    assert!(pending, "The tool did not wait for receipt {phase}");
    assert!(result.is_err());
    let receipts = f.s.store.list("plugin-http-receipts").await.unwrap();
    if phase == "claim" {
        assert_eq!(account.writes.load(Ordering::SeqCst), 0);
        assert!(
            receipts.is_empty(),
            "A refused, unsent call poisoned its receipt key"
        );
    } else {
        assert_eq!(account.writes.load(Ordering::SeqCst), 1);
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            receipts[0]["state"], "pending",
            "Retired work published the provider's private response"
        );
        assert!(
            !serde_json::to_string(&receipts)
                .unwrap()
                .contains("retired-receipt-result")
        );
    }
}

#[tokio::test]
async fn an_http_claim_cannot_poison_a_key_after_requester_revocation() {
    receipt_after_change("claim").await;
}

#[tokio::test]
async fn an_http_response_cannot_be_committed_after_requester_revocation() {
    receipt_after_change("complete").await;
}
