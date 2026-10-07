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
