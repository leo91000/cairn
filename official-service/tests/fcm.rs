use axum::{Json, Router, extract::Form, routing::post};
use jwt_simple::prelude::*;
use leo_official_service::{FcmPushSender, PushSender, PushSubscription};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[tokio::test]
async fn only_expired_or_invalid_device_tokens_are_removed_not_provider_outages() {
    let key = RS256KeyPair::generate(2048).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route(
            "/token",
            post(|| async {
                Json(json!({
                    "access_token": "fixture-access",
                    "expires_in": 3600,
                }))
            }),
        )
        .route(
            "/send",
            post(|Json(body): Json<Value>| async move {
                let code = body["message"]["token"]
                    .as_str()
                    .unwrap()
                    .parse::<u16>()
                    .unwrap();
                let error = match code {
                    400 => "INVALID_ARGUMENT",
                    404 | 403 => "UNREGISTERED",
                    _ => "UNAVAILABLE",
                };
                (
                    axum::http::StatusCode::from_u16(code).unwrap(),
                    Json(json!({
                        "error": {
                            "details": [{
                                "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                                "errorCode": error,
                            }],
                        },
                    })),
                )
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let account = json!({
        "project_id": "fixture-project",
        "client_email": "fixture@example.test",
        "private_key": key.to_pem().unwrap(),
    });
    let sender = FcmPushSender::with_endpoints(
        &account.to_string(),
        &format!("{origin}/token"),
        &format!("{origin}/send"),
    )
    .unwrap();
    for (code, gone) in [
        (400, true),
        (404, true),
        (403, false),
        (429, false),
        (503, false),
    ] {
        let subscription: PushSubscription = serde_json::from_value(json!({
            "endpoint": format!("fcm:{code}"),
            "keys": { "p256dh": "", "auth": "" },
        }))
        .unwrap();
        let result = sender.send(&subscription, &json!({})).await;
        assert_eq!(
            matches!(result, Err(leo_official_service::PushError::Gone)),
            gone,
            "HTTP {code}"
        );
    }
    server.abort();
}

#[tokio::test]
async fn native_sender_authenticates_and_sends_only_data_with_a_cached_access_token() {
    let key = RS256KeyPair::generate(2048).unwrap();
    let public = key.public_key();
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let audience = format!("{origin}/token");
    let captured = calls.clone();
    let exchanges = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let token_calls = exchanges.clone();
    let app = Router::new()
        .route(
            "/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let public = public.clone();
                let audience = audience.clone();
                token_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    assert_eq!(
                        form["grant_type"],
                        "urn:ietf:params:oauth:grant-type:jwt-bearer"
                    );
                    let claims = public
                        .verify_token::<Value>(
                            &form["assertion"],
                            Some(VerificationOptions {
                                allowed_issuers: Some(HashSet::from_strings(&[
                                    "fixture@example.test",
                                ])),
                                allowed_audiences: Some(HashSet::from_strings(&[&audience])),
                                ..Default::default()
                            }),
                        )
                        .unwrap();
                    assert_eq!(
                        claims.custom["scope"],
                        "https://www.googleapis.com/auth/firebase.messaging"
                    );
                    Json(json!({
                        "access_token": "fixture-access",
                        "expires_in": 3600,
                    }))
                }
            }),
        )
        .route(
            "/send",
            post(
                move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                    let captured = captured.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer fixture-access");
                        captured.lock().unwrap().push(body);
                        Json(json!({"name": "fixture-message"}))
                    }
                },
            ),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let account = json!({
        "project_id": "fixture-project",
        "client_email": "fixture@example.test",
        "private_key": key.to_pem().unwrap(),
    });
    let sender = FcmPushSender::with_endpoints(
        &account.to_string(),
        &format!("{origin}/token"),
        &format!("{origin}/send"),
    )
    .unwrap();
    let subscription: PushSubscription = serde_json::from_value(json!({
        "endpoint": "fcm:fixture-device",
        "keys": { "p256dh": "", "auth": "" },
    }))
    .unwrap();
    for _ in 0..2 {
        sender
            .send(
                &subscription,
                &json!({
                    "accountId": "account",
                    "installationId": "installation",
                    "chatId": "chat",
                    "questionId": "question",
                    "title": "Private title",
                    "body": "Private body",
                }),
            )
            .await
            .unwrap();
    }
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(exchanges.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(calls[0]["message"]["token"], "fixture-device");
    assert!(calls[0]["message"].get("notification").is_none());
    assert_eq!(
        calls[0]["message"]["data"],
        json!({
            "accountId": "account",
            "installationId": "installation",
            "chatId": "chat",
            "questionId": "question",
        })
    );
    server.abort();
}
