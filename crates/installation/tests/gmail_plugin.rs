//! Gmail and Google OAuth are simulated locally; tests never use personal mail.
use common::AgentFixture;
use common::MissionFixture;

mod common;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cairn_installation::{
    config::{MAIN_AGENT_ID, now},
    plugins::{
        connections::{self, Connection},
        oauth, registry,
        scope::{self, Scope},
    },
    service::Service,
    validation::text,
};
use cairn_plugin_host::Host;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tempfile::TempDir;

#[derive(Default)]
struct Google {
    calls: Mutex<Vec<Value>>,
    no_refresh: Mutex<bool>,
    send_error: Mutex<bool>,
    broken_send: Mutex<bool>,
    pause_refresh: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}

async fn answer(google: Arc<Google>, request: Request<Body>) -> Response {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let method = request.method().to_string();
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = to_bytes(request.into_body(), 60 * 1024 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    google.calls.lock().unwrap().push(json!({ "path": path, "query": query, "method": method, "authorization": authorization, "body": body }));
    if path == "/token" {
        let fields: HashMap<String, String> = serde_urlencoded::from_bytes(&bytes).unwrap();
        let refresh = fields.get("refresh_token");
        let pause = google.pause_refresh.lock().unwrap().clone();
        if refresh.is_some()
            && let Some((started, resume)) = pause
        {
            started.notify_one();
            resume.notified().await;
        }
        let account = refresh
            .map(|s| s.trim_start_matches("refresh-"))
            .or_else(|| fields.get("code").map(String::as_str))
            .unwrap();
        let mut result = json!({ "access_token": format!("access-{account}"), "token_type": "Bearer", "expires_in": 3600 });
        if refresh.is_none() {
            assert!(fields.get("code_verifier").is_some_and(|v| v.len() >= 43));
            assert!(fields.contains_key("redirect_uri"));
            if !*google.no_refresh.lock().unwrap() {
                result["refresh_token"] = json!(format!("refresh-{account}"));
            }
        }
        return axum::Json(result).into_response();
    }
    if path == "/revoke" {
        return StatusCode::OK.into_response();
    }
    let account = authorization
        .strip_prefix("Bearer access-")
        .unwrap_or("invalid");
    if path.ends_with("/profile") {
        return axum::Json(json!({
            "emailAddress": format!("{account}@example.test"),
            "historyId": "18446744073709551610",
            "messagesTotal": 8,
            "threadsTotal": 3,
        }))
        .into_response();
    }
    if path.ends_with("/attachments/large") {
        return axum::Json(json!({ "data": URL_SAFE_NO_PAD.encode(vec![0xa5; 3 * 1024 * 1024]), "size": 3 * 1024 * 1024 })).into_response();
    }
    if path.ends_with("/messages/parent") {
        return axum::Json(json!({ "id": "parent", "threadId": "thread-one", "payload": { "headers": [
            { "name": "Subject", "value": "Facture été" }, { "name": "Message-ID", "value": "<parent@example.test>" },
            { "name": "References", "value": "<first@example.test>" },
        ] }, "raw": URL_SAFE_NO_PAD.encode(b"From: alice@example.test\r\nSubject: Facture\r\n\r\nOriginal") })).into_response();
    }
    if path.ends_with("/messages/send")
        || path.ends_with("/drafts")
        || path.ends_with("/drafts/draft-one")
    {
        if *google.broken_send.lock().unwrap() {
            let broken = futures_util::stream::once(async {
                Err::<axum::body::Bytes, _>(std::io::Error::other("synthetic interrupted send"))
            });
            return Response::new(Body::from_stream(broken));
        }
        if *google.send_error.lock().unwrap() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({ "error": { "status": "UNAVAILABLE" } })),
            )
                .into_response();
        }
        return axum::Json(
            json!({ "id": "sent-one", "threadId": "thread-one", "labelIds": ["SENT"] }),
        )
        .into_response();
    }
    axum::Json(json!({ "method": method, "path": path, "query": query, "body": body }))
        .into_response()
}

struct Fixture {
    _root: TempDir,
    s: Arc<Service>,
    google: Arc<Google>,
}

async fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    common::managed_codex_home(&root.path().join("home"));
    let s = Service::new(cairn_installation::config::Config {
        runner_url: "http://runner:4311".into(),
        ..common::config(root.path())
    })
    .await
    .unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let google = Arc::new(Google::default());
    let state = google.clone();
    let app = Router::new().fallback(move |request| answer(state.clone(), request));
    let (origin, _server) = common::serve_locally(app).await;
    s.plugins.route_http_to(Some(origin));
    Fixture {
        _root: root,
        s,
        google,
    }
}

fn settings() -> Value {
    json!({
        "clientId": "fixture-client",
        "allowSend": true,
        "allowDrafts": true,
        "allowModify": true,
        "allowDelete": true,
        "allowSettings": true,
        "allowSharing": true,
        "allowWatch": true,
        "allowAdmin": true,
    })
}

#[tokio::test]
async fn oauth_form_compares_defaulted_scope_settings_without_restoring_cleared_values() {
    let f = fixture().await;
    let settings = json!({ "clientId": "fixture-client", "allowSend": true });
    let started = oauth::start(
        &f.s,
        "gmail",
        &json!({
            "settings": settings, "secrets": { "clientSecret": "fixture-secret" },
        }),
        "cairn-account:fixture-owner",
    )
    .await
    .unwrap();
    oauth::callback(
        &f.s,
        &json!({ "state": started["id"], "code": "alice" }),
        "cairn-account:fixture-owner",
    )
    .await
    .unwrap();

    let completed = common::plugin_form(
        &f.s,
        "gmail",
        &json!({
            "settings": settings, "credentialFlow": started["id"],
            "secrets": { "clientSecret": "fixture-secret" },
        }),
    )
    .await
    .unwrap();
    assert_eq!(completed["values"]["email"], "alice@example.test");
    let changed = common::plugin_form(
        &f.s,
        "gmail",
        &json!({
            "settings": { "clientId": "fixture-client", "allowSend": true, "allowModify": true },
            "credentialFlow": started["id"], "secrets": { "clientSecret": "fixture-secret" },
        }),
    )
    .await;
    assert!(
        changed.is_err(),
        "Changing selected OAuth scopes must still require consent"
    );
}

#[tokio::test]
async fn forwarding_filters_require_sharing_for_inline_and_file_bodies() {
    let f = fixture().await;
    let mut account = connect(&f, "alice").await;
    let forwarding = json!({
        "criteria": { "query": "in:inbox" },
        "action": { "forward": "verified@example.test" },
    });
    let file = tool(
        &f,
        std::slice::from_ref(&account),
        json!({
            "operation": "fileCreate", "accountId": account.id,
            "file": { "name": "filter.json", "mediaType": "application/json",
                "data": forwarding.to_string(), "encoding": "text" },
        }),
    )
    .await
    .unwrap();
    let inline = json!({
        "operation": "request", "accountId": account.id,
        "method": "users.settings.filters.create", "body": forwarding,
    });
    let uploaded = json!({
        "operation": "request", "accountId": account.id,
        "method": "users.settings.filters.create",
        "options": { "bodyFile": { "id": file["id"], "format": "json" } },
    });
    for request in [&inline, &uploaded] {
        assert_eq!(
            tool(&f, std::slice::from_ref(&account), request.clone())
                .await
                .unwrap()["ok"],
            true
        );
    }
    let mut reduced = account.settings.clone();
    reduced["allowSharing"] = json!(false);
    reduced["allowSend"] = json!(false);
    account = serde_json::from_value(
        common::save_plugin(
            &f.s,
            "gmail",
            &json!({
                "name": "alice", "enabled": true, "settings": reduced,
            }),
            Some(&account.id),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    for request in [&inline, &uploaded] {
        let error = tool(&f, std::slice::from_ref(&account), request.clone())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("allowSharing"), "{error}");
    }
    let filter_calls = || {
        f.google
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| {
                call["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("/settings/filters"))
            })
            .count()
    };
    assert_eq!(filter_calls(), 2, "Denied forwarding never reaches Google");
    let ordinary = tool(&f, std::slice::from_ref(&account), json!({
        "operation": "request", "accountId": account.id,
        "method": "users.settings.filters.create",
        "body": { "criteria": { "query": "in:inbox" }, "action": { "addLabelIds": ["STARRED"] } },
    })).await.unwrap();
    assert_eq!(ordinary["ok"], true);
    assert_eq!(
        filter_calls(),
        3,
        "Non-forwarding filters still work with settings permission alone"
    );
}

async fn sign_in(f: &Fixture, account: &str, instance: Option<&str>) -> Value {
    let started = oauth::start(&f.s, "gmail", &json!({ "instance": instance, "settings": settings(), "secrets": { "clientSecret": "fixture-secret" } }), "cairn-account:fixture-owner").await.unwrap();
    let url = url::Url::parse(text(&started, "verificationUri")).unwrap();
    let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["access_type"], "offline");
    assert_eq!(params["state"], started["id"]);
    assert!(params["state"].starts_with("plugin."));
    assert_eq!(
        url::Url::parse(&params["redirect_uri"]).unwrap().path(),
        "/plugins/oauth/callback"
    );
    assert!(!started.to_string().contains("fixture-secret"));
    oauth::callback(
        &f.s,
        &json!({ "state": started["id"], "code": account }),
        "cairn-account:fixture-owner",
    )
    .await
    .unwrap();
    started
}

async fn connect(f: &Fixture, account: &str) -> Connection {
    let flow = sign_in(f, account, None).await;
    let saved = common::save_plugin(
        &f.s,
        "gmail",
        &json!({
            "name": account,
            "enabled": true,
            "settings": settings(),
            "secrets": { "clientSecret": "fixture-secret" },
            "credentialFlow": flow["id"],
            "agentIds": [MAIN_AGENT_ID],
        }),
        None,
    )
    .await
    .unwrap();
    assert!(!saved.to_string().contains("access-"));
    serde_json::from_value(saved).unwrap()
}

async fn tool(
    f: &Fixture,
    accounts: &[Connection],
    input: Value,
) -> cairn_installation::error::Result<Value> {
    let package = registry::active(&f.s, "gmail").await?;
    let scope = Scope::Agent {
        agent_id: MAIN_AGENT_ID.to_owned(),
        instances: accounts.to_vec(),
    };
    scope::invoke(
        &f.s,
        &package,
        "tool",
        json!({ "input": input, "instances": scope.instances() }),
        &scope,
    )
    .await
}

#[tokio::test]
async fn oauth_accounts_refresh_and_authorization_are_isolated() {
    let f = fixture().await;
    let alice = connect(&f, "alice").await;
    let bob = connect(&f, "bob").await;
    let accounts = vec![alice.clone(), bob.clone()];
    let listed = tool(&f, &accounts, json!({ "operation": "accounts" }))
        .await
        .unwrap();
    assert_eq!(listed["accounts"][0]["email"], "alice@example.test");
    assert_eq!(listed["accounts"][1]["email"], "bob@example.test");
    assert!(
        tool(
            &f,
            &accounts,
            json!({ "operation": "request", "method": "users.getProfile" })
        )
        .await
        .is_err()
    );
    assert!(
        tool(
            &f,
            std::slice::from_ref(&alice),
            json!({ "operation": "request", "accountId": bob.id, "method": "users.getProfile" })
        )
        .await
        .is_err()
    );
    for account in &accounts {
        let profile = tool(&f, &accounts, json!({ "operation": "request", "accountId": account.id, "method": "users.getProfile" })).await.unwrap();
        assert_eq!(profile["data"]["emailAddress"], account.settings["email"]);
        assert_eq!(profile["data"]["historyId"], "18446744073709551610");
    }
    let key = connections::credential_key(&alice.id);
    let mut credential = f.s.vault.get(&key).await.unwrap().unwrap();
    credential["oauth"]["expiresAt"] = json!(now() - 1);
    f.s.vault.set(&key, &credential).await.unwrap();
    let (first, second) = tokio::join!(
        tool(
            &f,
            &accounts,
            json!({ "operation": "request", "accountId": alice.id, "method": "users.getProfile" })
        ),
        tool(
            &f,
            &accounts,
            json!({ "operation": "request", "accountId": alice.id, "method": "users.getProfile" })
        )
    );
    first.unwrap();
    second.unwrap();
    let updated = f.s.vault.get(&key).await.unwrap().unwrap();
    assert_eq!(updated["oauth"]["refreshToken"], "refresh-alice");
    assert!(updated["oauth"]["expiresAt"].as_i64().unwrap() > now());
    let refreshes = f
        .google
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v["path"] == "/token")
        .count();
    assert_eq!(
        refreshes, 3,
        "two initial exchanges, one serialized refresh"
    );
    common::enable_plugin(&f.s, "gmail", &alice.id, &json!({ "enabled": false }))
        .await
        .unwrap();
    assert!(
        tool(
            &f,
            &accounts,
            json!({ "operation": "request", "accountId": alice.id, "method": "users.getProfile" })
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn oauth_rejects_wrong_owner_replay_and_missing_offline_access() {
    let f = fixture().await;
    let started = oauth::start(
        &f.s,
        "gmail",
        &json!({ "settings": settings(), "secrets": { "clientSecret": "fixture-secret" } }),
        "owner-one",
    )
    .await
    .unwrap();
    assert!(
        f.s.plugins
            .oauth_flows
            .status("gmail", text(&started, "id"), "owner-two")
            .is_err()
    );
    assert!(
        oauth::callback(
            &f.s,
            &json!({ "state": started["id"], "code": "alice" }),
            "owner-two"
        )
        .await
        .is_err()
    );
    oauth::callback(
        &f.s,
        &json!({ "state": started["id"], "code": "alice" }),
        "owner-one",
    )
    .await
    .unwrap();
    assert!(
        oauth::callback(
            &f.s,
            &json!({ "state": started["id"], "code": "bob" }),
            "owner-one"
        )
        .await
        .is_err()
    );
    *f.google.no_refresh.lock().unwrap() = true;
    let next = oauth::start(
        &f.s,
        "gmail",
        &json!({ "settings": settings(), "secrets": { "clientSecret": "fixture-secret" } }),
        "owner-one",
    )
    .await
    .unwrap();
    let error = oauth::callback(
        &f.s,
        &json!({ "state": next["id"], "code": "bob" }),
        "owner-one",
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("offline"));
    assert_eq!(
        f.s.plugins
            .oauth_flows
            .status("gmail", text(&next, "id"), "owner-one")
            .unwrap()["state"],
        "failed"
    );
}

#[tokio::test]
async fn mime_reply_draft_large_attachments_and_send_receipts() {
    let f = fixture().await;
    let account = connect(&f, "alice").await;
    let accounts = [account.clone()];
    let file = tool(&f, &accounts, json!({
        "operation": "fileCreate",
        "accountId": account.id,
        "file": { "name": "été.pdf", "mediaType": "application/pdf", "data": "JVBERi1maXh0dXJl", "encoding": "base64" },
    })).await.unwrap();
    let request = json!({
        "operation": "reply",
        "accountId": account.id,
        "messageId": "parent",
        "requestId": "invoice-followup-one",
        "message": { "to": [{ "email": "restaurant@example.test", "name": "Café été" }], "text": "Bonjour, voici la facture.", "html": "<p>Bonjour, voici la facture.</p>", "attachments": [{ "id": file["id"], "name": "été.pdf", "mediaType": "application/pdf" }] },
    });
    let first = tool(&f, &accounts, request.clone()).await.unwrap();
    assert_eq!(first["data"]["id"], "sent-one");
    let replay = tool(&f, &accounts, request.clone()).await.unwrap();
    assert_eq!(replay, first);
    let sends = f
        .google
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|v| {
            v["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("/messages/send"))
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["body"]["threadId"], "thread-one");
    let raw = URL_SAFE_NO_PAD
        .decode(text(&sends[0]["body"], "raw"))
        .unwrap();
    let raw = String::from_utf8(raw).unwrap();
    assert!(raw.contains("In-Reply-To: <parent@example.test>"), "{raw}");
    assert!(
        raw.contains("References: <first@example.test> <parent@example.test>"),
        "{raw}"
    );
    assert!(
        raw.contains("multipart/alternative") && raw.contains("application/pdf"),
        "{raw}"
    );
    assert!(!raw.contains("fixture-secret") && !raw.contains("access-alice"));
    tool(
        &f,
        &accounts,
        json!({
            "operation": "fileAppend",
            "accountId": account.id,
            "file": { "id": file["id"], "offset": file["size"], "data": "changed" },
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        tool(&f, &accounts, request.clone()).await.unwrap()["ok"],
        false,
        "changed attachment bytes cannot replay a confirmed intent"
    );
    let mut different = request;
    different["message"]["text"] = json!("Different");
    assert_eq!(tool(&f, &accounts, different).await.unwrap()["ok"], false);
    let draft = tool(&f, &accounts, json!({
        "operation": "draft",
        "accountId": account.id,
        "message": { "to": ["restaurant@example.test"], "subject": "Brouillon", "text": "Bonjour" },
    })).await.unwrap();
    assert!(draft["ok"].as_bool().unwrap());
    let call = f
        .google
        .calls
        .lock()
        .unwrap()
        .iter()
        .find(|v| v["path"].as_str().is_some_and(|p| p.ends_with("/drafts")))
        .unwrap()
        .clone();
    assert!(call["body"]["message"]["raw"].as_str().is_some());
    let downloaded = tool(
        &f,
        &accounts,
        json!({
            "operation": "attachment",
            "accountId": account.id,
            "messageId": "parent",
            "attachmentId": "large",
            "name": "large.pdf",
        }),
    )
    .await
    .unwrap();
    assert_eq!(downloaded["file"]["size"], 3 * 1024 * 1024);
    let read = tool(
        &f,
        &accounts,
        json!({
            "operation": "fileRead",
            "accountId": account.id,
            "file": { "id": downloaded["file"]["id"], "offset": 2_000_000, "limit": 100 },
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        URL_SAFE_NO_PAD.decode(text(&read, "data")).unwrap(),
        vec![0xa5; 100]
    );
    let bob = connect(&f, "bob").await;
    assert!(tool(&f, std::slice::from_ref(&bob), json!({ "operation": "fileRead", "accountId": bob.id, "file": { "id": downloaded["file"]["id"] } })).await.is_err());
    assert!(tool(&f, &accounts, json!({
        "operation": "send",
        "accountId": account.id,
        "requestId": "injection",
        "message": { "to": ["victim@example.test\r\nBcc: other@example.test"], "text": "bad" },
    })).await.is_err());
}

#[tokio::test]
async fn every_discovered_gmail_method_is_callable_without_cross_account_paths() {
    let package = registry::builtin("gmail").unwrap();
    let source = include_str!("../../../plugins/gmail/catalog.luau");
    let catalog: Value = serde_json::from_str(
        source
            .split_once("[====[")
            .unwrap()
            .1
            .split_once("]====]")
            .unwrap()
            .0,
    )
    .unwrap();
    assert_eq!(catalog["methods"].as_object().unwrap().len(), 79);
    let mut settings = settings();
    settings["email"] = json!("alice@example.test");
    let host = Host::default();
    for (operation, method) in catalog["methods"].as_object().unwrap() {
        let mut parameters = json!({});
        for (name, rule) in method["parameters"].as_object().unwrap() {
            if rule["required"] != true || name == "userId" {
                continue;
            }
            parameters[name] = if rule["enum"].is_array() {
                rule["enum"][0].clone()
            } else {
                json!("alias+test@example.test")
            };
        }
        let expected = method["method"].clone();
        let result = host.invoke(&PathBuf::from(env!("CARGO_BIN_EXE_cairn")), &package.manifest, &package.source, "tool", json!({
            "input": { "operation": "request", "accountId": "account-a", "method": operation, "parameters": parameters },
            "instances": [{ "id": "account-a", "name": "Alice", "settings": settings }],
        }), |method, args| { let expected = expected.clone(); async move {
            assert_eq!(method, "http.request"); assert_eq!(args["instance"], "account-a"); assert_eq!(args["method"], expected);
            let url = url::Url::parse(text(&args, "url")).unwrap();
            assert_eq!(url.host_str(), Some("gmail.googleapis.com"));
            assert!(url.path().starts_with("/gmail/v1/users/me/"), "{operation}: {url}");
            assert!(url.path().contains("%40") || !url.path().contains("alias"));
            Ok(json!({ "status": 200, "headers": {}, "json": { "routed": true } }))
        }}).await.unwrap_or_else(|e| panic!("{operation}: {e}"));
        assert_eq!(result["data"]["routed"], true, "{operation}");
    }
}

#[tokio::test]
async fn sign_in_is_saved_once_and_cannot_replace_a_mailbox() {
    let f = fixture().await;
    let flow = sign_in(&f, "alice", None).await;
    let input = json!({
        "name": "Alice",
        "enabled": true,
        "settings": settings(),
        "secrets": { "clientSecret": "fixture-secret" },
        "credentialFlow": flow["id"],
    });
    let (first, second) = tokio::join!(
        common::save_plugin(&f.s, "gmail", &input, None),
        common::save_plugin(&f.s, "gmail", &input, None)
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "one OAuth flow cannot create two accounts"
    );
    let account = first.or(second).unwrap();
    let next = sign_in(&f, "bob", Some(text(&account, "id"))).await;
    let mut replace = input;
    replace["credentialFlow"] = next["id"].clone();
    let error = common::save_plugin(&f.s, "gmail", &replace, Some(text(&account, "id")))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different mailbox"));
    assert_eq!(
        connections::get(&f.s, "gmail", text(&account, "id"))
            .await
            .unwrap()
            .settings["email"],
        "alice@example.test"
    );
}

#[tokio::test]
async fn ambiguous_sends_and_confirmed_service_errors_are_never_retried_automatically() {
    let f = fixture().await;
    let account = connect(&f, "alice").await;
    let accounts = [account.clone()];
    let request = json!({
        "operation": "send",
        "accountId": account.id,
        "requestId": "interrupted",
        "message": { "to": ["restaurant@example.test"], "subject": "Facture", "text": "Bonjour" },
    });
    *f.google.broken_send.lock().unwrap() = true;
    let interrupted = tool(&f, &accounts, request.clone()).await.unwrap();
    assert_eq!(interrupted["ok"], false);
    *f.google.broken_send.lock().unwrap() = false;
    let replay = tool(&f, &accounts, request.clone()).await.unwrap();
    assert_eq!(replay["ok"], false);
    assert_eq!(replay["retryable"], false);
    assert_eq!(replay["rfcMessageId"], interrupted["rfcMessageId"]);
    assert_eq!(
        f.google
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("/messages/send")))
            .count(),
        1
    );
    *f.google.send_error.lock().unwrap() = true;
    let mut retry = request;
    retry["requestId"] = json!("confirmed-refusal");
    let refused = tool(&f, &accounts, retry.clone()).await.unwrap();
    assert_eq!(refused["status"], 503);
    assert_eq!(refused["retryable"], false);
    *f.google.send_error.lock().unwrap() = false;
    assert_eq!(tool(&f, &accounts, retry).await.unwrap()["status"], 503);
}

#[tokio::test]
async fn revocation_during_refresh_cannot_publish_new_credentials() {
    let f = fixture().await;
    let account = connect(&f, "alice").await;
    let key = connections::credential_key(&account.id);
    let mut credential = f.s.vault.get(&key).await.unwrap().unwrap();
    credential["oauth"]["expiresAt"] = json!(now() - 1);
    f.s.vault.set(&key, &credential).await.unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *f.google.pause_refresh.lock().unwrap() = Some((started.clone(), resume.clone()));
    let request =
        json!({ "operation": "request", "accountId": account.id, "method": "users.getProfile" });
    let account_id = account.id.clone();
    let revoke = async {
        started.notified().await;
        common::enable_plugin(&f.s, "gmail", &account_id, &json!({ "enabled": false }))
            .await
            .unwrap();
        resume.notify_one();
    };
    let accounts = [account];
    let (result, ()) = tokio::join!(tool(&f, &accounts, request), revoke);
    assert!(result.is_err());
    assert_eq!(
        f.s.vault.get(&key).await.unwrap().unwrap()["oauth"]["expiresAt"],
        credential["oauth"]["expiresAt"]
    );
}

#[tokio::test]
async fn owner_disconnect_removes_tokens_and_confirms_provider_revocation() {
    let f = fixture().await;
    let account = connect(&f, "alice").await;
    let owner = common::relay_fixture::context(&f.s).await;
    let app = common::relay_fixture::router(f.s.clone()).await.unwrap();
    let request = common::request(
        "POST",
        &format!("/api/plugins/gmail/connections/{}/disconnect", account.id),
    )
    .header("x-test-relay-token", text(&owner, "value"))
    .body(Body::empty())
    .unwrap();
    let response = common::send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let result = common::read_json(response).await;
    assert_eq!(result["disconnected"], true);
    assert_eq!(result["revoked"], true);
    assert!(
        f.s.vault
            .get(&connections::credential_key(&account.id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !connections::get(&f.s, "gmail", &account.id)
            .await
            .unwrap()
            .enabled
    );
    assert!(
        common::enable_plugin(&f.s, "gmail", &account.id, &json!({ "enabled": true }))
            .await
            .is_err()
    );
    assert!(
        f.google
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["path"] == "/revoke")
    );
}

#[tokio::test]
async fn private_cli_transfers_use_the_run_and_current_account_grant() {
    let f = fixture().await;
    let account = connect(&f, "alice").await;
    let task =
        f.s.task_fixture(
            json!({ "name": "Transfer", "prompt": "Transfer fixtures", "agentId": MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let run =
        f.s.enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap();
    let run =
        f.s.store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Running),
                    boundary: Some(serde_json::json!({"isolated": true})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    let run = f.s.store.run(&run.id).await.unwrap();
    let configuration = f.s.mcps.run_configuration(&f.s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let created = f.s.plugins.command(&f.s, bearer, &json!({ "plugin": "gmail", "file": { "method": "create", "instance": account.id, "name": "invoice.pdf" } })).await.unwrap()["value"].clone();
    let bytes = vec![0xa6; 65_536];
    let append = json!({
        "plugin": "gmail",
        "file": { "method": "append", "instance": account.id, "id": created["id"], "offset": 0, "data": URL_SAFE_NO_PAD.encode(&bytes), "encoding": "base64url" },
    });
    assert_eq!(
        f.s.plugins.command(&f.s, bearer, &append).await.unwrap()["value"]["size"],
        65_536
    );
    assert!(
        f.s.plugins.command(&f.s, bearer, &append).await.is_err(),
        "a confirmed chunk cannot be appended twice"
    );
    let read = json!({ "plugin": "gmail", "file": { "method": "read", "instance": account.id, "id": created["id"] } });
    let result = f.s.plugins.command(&f.s, bearer, &read).await.unwrap();
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(text(&result["value"], "data"))
            .unwrap(),
        bytes
    );
    f.s.agent_fixture(
        json!({ "name": "Main agent", "plugins": [] }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    assert!(f.s.plugins.command(&f.s, bearer, &read).await.is_err());
}

#[tokio::test]
async fn installation_management_revocation_during_oauth_refresh_cannot_publish_tokens() {
    let f = fixture().await;
    let account = connect(&f, "alice").await;
    f.s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": true } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let task =
        f.s.task_fixture(
            json!({ "name": "Check account", "prompt": "Check Gmail", "agentId": MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let run =
        f.s.enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap();
    let run =
        f.s.store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Running),
                    boundary: Some(serde_json::json!({"isolated": true})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    let run = f.s.store.run(&run.id).await.unwrap();
    let configuration = f.s.mcps.run_configuration(&f.s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let key = connections::credential_key(&account.id);
    let mut credential = f.s.vault.get(&key).await.unwrap().unwrap();
    credential["oauth"]["expiresAt"] = json!(now() - 1);
    f.s.vault.set(&key, &credential).await.unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *f.google.pause_refresh.lock().unwrap() = Some((started.clone(), resume.clone()));
    let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": common::mcp_arguments("manage_plugin", &json!({ "pluginId": "gmail", "kind": "test", "instanceId": account.id })),
    });
    let request = common::request("POST", "/mcp-installation")
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let revoke = async {
        started.notified().await;
        f.s.agent_fixture(
            json!({ "name": "Main agent", "access": { "installation": false } }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
        resume.notify_one();
    };
    let (response, ()) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(common::send(&app, request), revoke)
    })
    .await
    .unwrap();
    let response = common::read_json(response).await;
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert_eq!(
        f.s.vault.get(&key).await.unwrap().unwrap()["oauth"]["expiresAt"],
        credential["oauth"]["expiresAt"]
    );
}

/// An installation management run of Main agent, through the installation MCP endpoint.
async fn manage(f: &Fixture, args: Value) -> Value {
    f.s.agent_fixture(
        json!({ "name": "Main agent", "access": { "installation": true } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let task =
        f.s.task_fixture(
            json!({ "name": "Set up Gmail", "prompt": "Add a mailbox", "agentId": MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let run =
        f.s.enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap();
    let run =
        f.s.store
            .change_run(
                text(&run, "id"),
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Running),
                    boundary: Some(serde_json::json!({"isolated": true})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    let run = f.s.store.run(&run.id).await.unwrap();
    let configuration = f.s.mcps.run_configuration(&f.s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN");
    let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": common::mcp_arguments("manage_plugin", &args),
    });
    let request = common::request("POST", "/mcp-installation")
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    common::read_json(common::send(&app, request).await).await
}

#[tokio::test]
async fn a_new_mailbox_reuses_the_google_app_without_exposing_its_secret() {
    let f = fixture().await;
    let alice = connect(&f, "alice").await;
    let owner = "cairn-account:fixture-owner";

    // Only the permissions are typed: the client comes from Alice's account.
    let reuse = json!({ "sharedFrom": alice.id, "settings": { "allowSend": true } });
    let started = oauth::start(&f.s, "gmail", &reuse, owner).await.unwrap();
    let url = url::Url::parse(text(&started, "verificationUri")).unwrap();
    let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params["client_id"], "fixture-client");
    assert!(!started.to_string().contains("fixture-secret"));
    oauth::callback(
        &f.s,
        &json!({ "state": started["id"], "code": "carol" }),
        owner,
    )
    .await
    .unwrap();

    let mut draft = reuse.clone();
    draft["credentialFlow"] = started["id"].clone();
    let form = common::plugin_form(&f.s, "gmail", &draft).await.unwrap();
    assert_eq!(form["values"]["email"], "carol@example.test", "{form}");
    assert!(!form.to_string().contains("fixture-secret"));

    draft["name"] = json!("carol@example.test");
    draft["enabled"] = json!(true);
    let saved = common::save_plugin(&f.s, "gmail", &draft, None)
        .await
        .unwrap();
    assert!(!saved.to_string().contains("fixture-secret"));
    let carol: Connection = serde_json::from_value(saved).unwrap();
    assert_eq!(carol.settings["clientId"], "fixture-client");
    assert_eq!(carol.settings["allowSend"], true);
    assert_eq!(carol.settings["email"], "carol@example.test");
    assert_eq!(carol.secret_keys, ["clientSecret"]);
    let secrets =
        f.s.vault
            .get(&connections::secrets_key(&carol.id))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(secrets["fields"]["clientSecret"], "fixture-secret");
    let credential =
        f.s.vault
            .get(&connections::credential_key(&carol.id))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(credential["oauth"]["clientId"], "fixture-client");
    assert_eq!(credential["oauth"]["clientSecret"], "fixture-secret");

    // An edit keeps its own setup: only a new instance reuses another one's.
    let error = common::save_plugin(&f.s, "gmail", &draft, Some(&carol.id))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Only a new instance"), "{error}");
    let edit = json!({ "instance": carol.id, "sharedFrom": alice.id, "settings": {} });
    let error = oauth::start(&f.s, "gmail", &edit, owner).await.unwrap_err();
    assert!(error.to_string().contains("Only a new instance"), "{error}");

    // Reuse names an instance of the same package, and only shared settings move.
    let unknown = json!({ "sharedFrom": uuid::Uuid::new_v4().to_string(), "settings": {} });
    assert!(oauth::start(&f.s, "gmail", &unknown, owner).await.is_err());
    let invalid = json!({ "sharedFrom": 7, "settings": {} });
    assert!(oauth::start(&f.s, "gmail", &invalid, owner).await.is_err());
    let elsewhere = json!({ "name": "Vault", "enabled": true, "sharedFrom": alice.id, "secrets": { "token": "ops_fixture" } });
    let error = common::save_plugin(&f.s, "onepassword", &elsewhere, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("nothing to reuse"), "{error}");
}

#[tokio::test]
async fn installation_management_reuses_only_a_granted_google_app() {
    let f = fixture().await;
    let granted = connect(&f, "alice").await;
    let flow = sign_in(&f, "bob", None).await;
    let ungranted = common::save_plugin(
        &f.s,
        "gmail",
        &json!({
            "name": "bob",
            "enabled": true,
            "settings": settings(),
            "secrets": { "clientSecret": "fixture-secret" },
            "credentialFlow": flow["id"],
        }),
        None,
    )
    .await
    .unwrap();

    let refused = manage(
        &f,
        json!({
            "pluginId": "gmail",
            "kind": "signin",
            "data": { "sharedFrom": ungranted["id"], "settings": {}, "secrets": {} },
        }),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(refused.to_string().contains("grant"), "{refused}");

    let started = manage(
        &f,
        json!({
            "pluginId": "gmail",
            "kind": "signin",
            "data": { "sharedFrom": granted.id, "settings": {}, "secrets": {} },
        }),
    )
    .await;
    assert_ne!(started["result"]["isError"], true, "{started}");
    assert!(started.to_string().contains("fixture-client"), "{started}");
    assert!(!started.to_string().contains("fixture-secret"));

    let setup = manage(&f, json!({ "pluginId": "gmail", "kind": "setup" })).await;
    let steps = setup.to_string();
    assert!(steps.contains("Google Cloud app"), "{setup}");
    assert!(steps.contains("callbackUrl"), "{setup}");
}

#[tokio::test]
async fn refresh_does_not_post_after_revocation_during_credential_preparation() {
    use std::{sync::mpsc, task::Poll, time::Duration};
    let f = fixture().await;
    let account = connect(&f, "alice").await;
    assert!(account.agent_id.is_empty());
    let key = connections::credential_key(&account.id);
    let mut credential = f.s.vault.get(&key).await.unwrap().unwrap();
    credential["oauth"]["expiresAt"] = json!(now() - 1);
    f.s.vault.set(&key, &credential).await.unwrap();
    let token_requests = || {
        f.google
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call["path"] == "/token")
            .count()
    };
    let before = token_requests();

    // Occupy the current four SQLite readers. Polling the token future queues
    // its initial local validation; the following gate then parks the reader
    // after that validation, before the future can retrieve its credential.
    // The writer remains independent, just as it does during a real I/O wait.
    let mut release = Vec::new();
    let mut readers = Vec::new();
    for _ in 0..4 {
        let entered = Arc::new(tokio::sync::Notify::new());
        let signal = entered.clone();
        let (send, receive) = mpsc::channel();
        let store = f.s.store.clone();
        readers.push(tokio::spawn(async move {
            store
                .read(move |_| {
                    signal.notify_one();
                    let _ = receive.recv();
                    Ok(())
                })
                .await
        }));
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .unwrap();
        release.push(send);
    }
    tokio::task::yield_now().await;
    let mut token = Box::pin(oauth::access_token(&f.s, &account));
    assert!(matches!(futures_util::poll!(token.as_mut()), Poll::Pending));
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let (send, receive) = mpsc::channel();
    let mut after_validation = Box::pin(f.s.store.read(move |_| {
        signal.notify_one();
        let _ = receive.recv();
        Ok(())
    }));
    assert!(matches!(
        futures_util::poll!(after_validation.as_mut()),
        Poll::Pending
    ));
    release.remove(0).send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let mut disabled = account.clone();
    disabled.enabled = false;
    disabled.revision = uuid::Uuid::new_v4().to_string();
    f.s.store
        .transaction(move |db| db.save_plugin_instance(&disabled))
        .await
        .unwrap();
    for gate in release {
        gate.send(()).unwrap();
    }
    send.send(()).unwrap();
    after_validation.await.unwrap();
    for reader in readers {
        reader.await.unwrap().unwrap();
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(10), token)
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        token_requests(),
        before,
        "refresh POST escaped after its local authority was revoked"
    );
    assert!(f.s.vault.get(&key).await.unwrap().as_ref() == Some(&credential));
}
