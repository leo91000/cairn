mod common;

use axum::{Json, Router, routing::get};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use common::{Fixture, login};
use jwt_simple::prelude::*;
use serde_json::{Value, json};

#[tokio::test]
async fn passkey_app_links_advertise_only_the_configured_android_signing_certificate() {
    let app = Fixture::with_oauth(leo_official_service::OAuthProviders {
        android_certificates: vec![[7; 32]],
        ..Default::default()
    })
    .await;
    let response = app
        .client
        .get(format!("{}/.well-known/assetlinks.json", app.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!([{
            "relation": ["delegate_permission/common.get_login_creds"],
            "target": {
                "namespace": "android_app",
                "package_name": "dev.leo.manager",
                "sha256_cert_fingerprints": [std::iter::repeat_n("07", 32).collect::<Vec<_>>().join(":")],
            },
        }])
    );
    app.close().await;
}

#[tokio::test]
async fn credential_manager_google_uses_the_existing_verified_email_account_and_one_use_nonce() {
    let key = RS256KeyPair::generate(2048)
        .unwrap()
        .with_key_id("fixture-key");
    let components = key.public_key().to_components();
    let keys = json!({
        "keys": [{
            "kid": "fixture-key",
            "kty": "RSA",
            "alg": "RS256",
            "n": URL_SAFE_NO_PAD.encode(components.n),
            "e": URL_SAFE_NO_PAD.encode(components.e),
        }],
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let jwks = format!("http://{}/keys", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/keys",
                get(move || {
                    let keys = keys.clone();
                    async { Json(keys) }
                }),
            ),
        )
        .await
        .unwrap()
    });
    let app = Fixture::with_oauth(leo_official_service::OAuthProviders {
        google: Some(leo_official_service::OAuthProvider {
            client_id: "fixture-client".into(),
            client_secret: "fixture-secret".into(),
            authorization_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            userinfo_url: "https://openidconnect.googleapis.com/v1/userinfo".into(),
            emails_url: None,
        }),
        google_jwks_url: Some(jwks),
        ..Default::default()
    })
    .await;
    let (_, original) = login(&app, "alice@gmail.com").await;
    let response = app
        .client
        .post(format!("{}/api/account/oauth/google/start", app.url))
        .header("origin", &app.url)
        .json(&json!({"native": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let challenge: Value = response.json().await.unwrap();
    let nonce = challenge["nonce"].as_str().unwrap();
    assert_eq!(challenge["clientId"], "fixture-client");
    let credential = key
        .sign(
            Claims::with_custom_claims(
                json!({
                    "email": "Alice@gmail.com",
                    "email_verified": true,
                }),
                Duration::from_secs(300),
            )
            .with_subject("google-alice")
            .with_issuer("https://accounts.google.com")
            .with_audience("fixture-client")
            .with_nonce(nonce),
        )
        .unwrap();
    let request = || {
        app.client
            .post(format!("{}/api/account/oauth/google/callback", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .json(&json!({
                "challenge": challenge["challenge"],
                "credential": credential,
            }))
    };
    let response = request().send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .starts_with("leo_session=")
    );
    assert_eq!(
        response.json::<Value>().await.unwrap()["account"]["id"],
        original["account"]["id"]
    );
    assert_eq!(request().send().await.unwrap().status(), 401);

    let attacker = RS256KeyPair::generate(2048)
        .unwrap()
        .with_key_id("fixture-key");
    for invalid in [
        "nonce",
        "audience",
        "issuer",
        "signature",
        "expiration",
        "verified-email",
        "missing-expiration",
    ] {
        let response = app
            .client
            .post(format!("{}/api/account/oauth/google/start", app.url))
            .header("origin", &app.url)
            .json(&json!({"native": true}))
            .send()
            .await
            .unwrap();
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let challenge: Value = response.json().await.unwrap();
        let mut claims = Claims::with_custom_claims(
            json!({
                "email": "alice@gmail.com",
                "email_verified": invalid != "verified-email",
            }),
            Duration::from_secs(300),
        )
        .with_subject("google-alice")
        .with_issuer(if invalid == "issuer" {
            "https://attacker.example"
        } else {
            "https://accounts.google.com"
        })
        .with_audience(if invalid == "audience" {
            "another-client"
        } else {
            "fixture-client"
        })
        .with_nonce(if invalid == "nonce" {
            "another-nonce"
        } else {
            challenge["nonce"].as_str().unwrap()
        });
        if invalid == "expiration" {
            claims.expires_at = Some(UnixTimeStamp::from_secs(1));
        }
        if invalid == "missing-expiration" {
            claims.expires_at = None;
        }
        let signing_key = if invalid == "signature" {
            &attacker
        } else {
            &key
        };
        let credential = signing_key.sign(claims).unwrap();
        let response = app
            .client
            .post(format!("{}/api/account/oauth/google/callback", app.url))
            .header("origin", &app.url)
            .header("cookie", cookie)
            .json(&json!({
                "challenge": challenge["challenge"],
                "credential": credential,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "invalid {invalid}");
    }
    app.close().await;
    server.abort();
}

#[tokio::test]
async fn native_passkeys_accept_only_configured_certificate_origins_and_reuse_the_account() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use leo_official_service::OAuthProviders;
    use reqwest::StatusCode;
    use sha2::{Digest, Sha256};
    use webauthn_authenticator_rs::{
        AuthenticatorBackendHashedClientData, softpasskey::SoftPasskey,
    };
    use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse};

    let app = Fixture::with_oauth(OAuthProviders {
        android_certificates: vec![[7; 32]],
        ..Default::default()
    })
    .await;
    let (cookie, session) = login(&app, "native-passkey@example.test").await;
    let mut authenticator = SoftPasskey::new(true);
    let mut saved = None;
    for (certificate, status) in [(8u8, StatusCode::UNAUTHORIZED), (7, StatusCode::CREATED)] {
        let start = app
            .client
            .post(format!("{}/api/account/passkeys/register/start", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        let challenge: Value = start.json().await.unwrap();
        let mut options: CreationChallengeResponse =
            serde_json::from_value(challenge["options"].clone()).unwrap();
        let handle = options.public_key.user.id.clone();
        let selection = options.public_key.authenticator_selection.as_mut().unwrap();
        assert!(selection.require_resident_key);
        selection.require_resident_key = false;
        selection.resident_key = Some(webauthn_rs_proto::ResidentKeyRequirement::Discouraged);
        let client_data = serde_json::to_vec(&json!({
            "type": "webauthn.create",
            "challenge": challenge["options"]["publicKey"]["challenge"],
            "origin": format!("android:apk-key-hash:{}", URL_SAFE_NO_PAD.encode([certificate; 32])),
            "crossOrigin": false,
        }))
        .unwrap();
        let mut credential = authenticator
            .perform_register(
                Sha256::digest(&client_data).to_vec(),
                options.public_key,
                60_000,
            )
            .unwrap();
        credential.response.client_data_json = client_data.into();
        let id = credential.raw_id.clone();
        let finish = app
            .client
            .post(format!("{}/api/account/passkeys/register/finish", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({
                "challenge": challenge["challenge"],
                "label": "Android",
                "credential": credential,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(finish.status(), status);
        if status == StatusCode::CREATED {
            saved = Some((id, handle));
        }
    }
    let start = app
        .post("/api/account/passkeys/login/start", json!({}))
        .await;
    let browser = start.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let challenge: Value = start.json().await.unwrap();
    let mut options: RequestChallengeResponse =
        serde_json::from_value(challenge["options"].clone()).unwrap();
    assert!(options.public_key.allow_credentials.is_empty());
    let (id, handle) = saved.unwrap();
    options
        .public_key
        .allow_credentials
        .push(webauthn_rs_proto::AllowCredentials {
            type_: "public-key".into(),
            id,
            transports: None,
        });
    let client_data = serde_json::to_vec(&json!({
        "type": "webauthn.get",
        "challenge": challenge["options"]["publicKey"]["challenge"],
        "origin": format!("android:apk-key-hash:{}", URL_SAFE_NO_PAD.encode([7u8; 32])),
        "crossOrigin": false,
    }))
    .unwrap();
    let mut credential = authenticator
        .perform_auth(
            Sha256::digest(&client_data).to_vec(),
            options.public_key,
            60_000,
        )
        .unwrap();
    credential.response.client_data_json = client_data.into();
    credential.response.user_handle = Some(handle);
    let finish = app
        .client
        .post(format!("{}/api/account/passkeys/login/finish", app.url))
        .header("origin", &app.url)
        .header("cookie", browser)
        .json(&json!({
            "challenge": challenge["challenge"],
            "credential": credential,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(finish.status(), StatusCode::OK);
    let native_session: Value = finish.json().await.unwrap();
    assert_eq!(native_session["account"]["id"], session["account"]["id"]);
    app.close().await;
}

#[tokio::test]
async fn revoking_the_leo_session_during_google_verification_prevents_account_linking() {
    use std::sync::Arc;
    let key = RS256KeyPair::generate(2048)
        .unwrap()
        .with_key_id("fixture-key");
    let components = key.public_key().to_components();
    let keys = json!({
        "keys": [{
            "kid": "fixture-key",
            "kty": "RSA",
            "alg": "RS256",
            "n": URL_SAFE_NO_PAD.encode(components.n),
            "e": URL_SAFE_NO_PAD.encode(components.e),
        }],
    });
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let jwks = format!("http://{}/keys", listener.local_addr().unwrap());
    let entered_server = entered.clone();
    let release_server = release.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/keys",
                get(move || {
                    let entered = entered_server.clone();
                    let release = release_server.clone();
                    let keys = keys.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        Json(keys)
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let app = Fixture::with_oauth(leo_official_service::OAuthProviders {
        google: Some(leo_official_service::OAuthProvider {
            client_id: "fixture-client".into(),
            client_secret: "fixture-secret".into(),
            authorization_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            userinfo_url: "https://openidconnect.googleapis.com/v1/userinfo".into(),
            emails_url: None,
        }),
        google_jwks_url: Some(jwks),
        ..Default::default()
    })
    .await;
    let (cookie, session) = login(&app, "link@example.test").await;
    let start = app
        .authenticated(
            &cookie,
            &session,
            reqwest::Method::POST,
            "/api/account/oauth/google/start",
        )
        .json(&json!({ "native": true }))
        .send()
        .await
        .unwrap();
    let browser = start.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let challenge: Value = start.json().await.unwrap();
    let token = key
        .sign(
            Claims::with_custom_claims(
                json!({
                    "email": "link@example.test",
                    "email_verified": true,
                }),
                Duration::from_secs(300),
            )
            .with_subject("linked-google")
            .with_issuer("https://accounts.google.com")
            .with_audience("fixture-client")
            .with_nonce(challenge["nonce"].as_str().unwrap()),
        )
        .unwrap();
    let exchange = app
        .authenticated(
            &format!("{cookie}; {browser}"),
            &session,
            reqwest::Method::POST,
            "/api/account/oauth/google/callback",
        )
        .json(&json!({
            "challenge": challenge["challenge"],
            "credential": token,
        }));
    let exchange = tokio::spawn(async move { exchange.send().await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let logout = app
        .authenticated(
            &cookie,
            &session,
            reqwest::Method::POST,
            "/api/account/logout",
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 204);
    release.notify_one();
    assert_eq!(exchange.await.unwrap().status(), 401);
    app.close().await;
    server.abort();
}
