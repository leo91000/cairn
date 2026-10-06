mod common;

use common::Fixture;
use leo_official_service::router;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sqlx_core::query::query;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

// SoftPasskey signs real WebAuthn proofs but lacks resident-key storage. This client
// adapter retains the credential/user handle and supplies them locally when the
// server sends an empty discovery list, as a resident authenticator would do.
struct SoftwarePasskey {
    inner: webauthn_authenticator_rs::WebauthnAuthenticator<
        webauthn_authenticator_rs::softpasskey::SoftPasskey,
    >,
    credential_id: Vec<u8>,
    user_handle: Vec<u8>,
}

impl SoftwarePasskey {
    fn new(verified: bool) -> Self {
        Self {
            inner: webauthn_authenticator_rs::WebauthnAuthenticator::new(
                webauthn_authenticator_rs::softpasskey::SoftPasskey::new(verified),
            ),
            credential_id: Vec::new(),
            user_handle: Vec::new(),
        }
    }

    fn do_registration(
        &mut self,
        origin: url::Url,
        mut options: webauthn_rs::prelude::CreationChallengeResponse,
    ) -> Result<webauthn_rs::prelude::RegisterPublicKeyCredential, String> {
        let selection = options.public_key.authenticator_selection.as_mut().unwrap();
        assert!(selection.require_resident_key);
        self.user_handle = options.public_key.user.id.as_ref().to_vec();
        selection.require_resident_key = false;
        selection.resident_key = Some(webauthn_rs_proto::ResidentKeyRequirement::Discouraged);

        let credential = self
            .inner
            .do_registration(origin, options)
            .map_err(|error| format!("{error:?}"))?;
        self.credential_id = credential.raw_id.as_ref().to_vec();
        Ok(credential)
    }

    fn do_authentication(
        &mut self,
        origin: url::Url,
        mut options: webauthn_rs::prelude::RequestChallengeResponse,
    ) -> Result<webauthn_rs::prelude::PublicKeyCredential, String> {
        assert!(options.public_key.allow_credentials.is_empty());
        options
            .public_key
            .allow_credentials
            .push(webauthn_rs_proto::AllowCredentials {
                type_: "public-key".into(),
                id: self.credential_id.clone().into(),
                transports: None,
            });

        let mut credential = self
            .inner
            .do_authentication(origin, options)
            .map_err(|error| format!("{error:?}"))?;
        credential.response.user_handle = Some(self.user_handle.clone().into());
        Ok(credential)
    }
}

#[tokio::test]
async fn email_code_creates_a_verified_leo_account_and_a_persistent_session() {
    let app = Fixture::new().await;
    let response = app
        .post(
            "/api/account/email-code",
            json!({ "email": " Alice@Example.test " }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let challenge: Value = response.json().await.unwrap();
    assert!(challenge.get("code").is_none());
    let (email, code) = app.mail.0.lock().unwrap()[0].clone();
    assert_eq!(email, "alice@example.test");
    let response = app
        .post(
            "/api/account/verify",
            json!({ "challenge": challenge["challenge"], "code": code }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    let account: Value = response.json().await.unwrap();
    assert_eq!(account["account"]["email"], "alice@example.test");
    assert!(account["csrf"].as_str().unwrap().len() >= 32);
    let response = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", cookie.split(';').next().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session: Value = response.json().await.unwrap();
    assert_eq!(session["account"], account["account"]);
    assert_eq!(session["installations"], json!([]));
    app.close().await;
}

#[tokio::test]
async fn foreign_and_missing_origins_cannot_start_or_complete_sign_in() {
    let app = Fixture::new().await;
    for origin in [None, Some("https://attacker.example")] {
        for route in ["email-code", "verify"] {
            let mut request = app.client.post(format!("{}/api/account/{route}", app.url));
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = request
                .json(&json!({
                    "email": "alice@example.test",
                    "challenge": "missing",
                    "code": "12345678",
                }))
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    assert!(app.mail.0.lock().unwrap().is_empty());
    app.close().await;
}

impl Fixture {
    async fn code(&self, email: &str) -> (Value, String) {
        let response = self
            .post("/api/account/email-code", json!({ "email": email }))
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let challenge: Value = response.json().await.unwrap();
        let code = self.mail.0.lock().unwrap().last().unwrap().1.clone();
        (challenge["challenge"].clone(), code)
    }

    async fn verify(&self, challenge: &Value, code: &str) -> reqwest::Response {
        self.post(
            "/api/account/verify",
            json!({ "challenge": challenge, "code": code }),
        )
        .await
    }
}

#[tokio::test]
async fn logout_requires_origin_and_csrf_and_revokes_the_server_session() {
    let app = Fixture::new().await;
    let (challenge, code) = app.code("logout@example.test").await;
    let response = app.verify(&challenge, &code).await;
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session: Value = response.json().await.unwrap();
    for (origin, csrf) in [
        ("https://attacker.test", session["csrf"].as_str().unwrap()),
        (app.url.as_str(), "wrong"),
        (app.url.as_str(), ""),
    ] {
        let response = app
            .client
            .post(format!("{}/api/account/logout", app.url))
            .header("origin", origin)
            .header("cookie", &cookie)
            .header("x-csrf-token", csrf)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = app
        .client
        .post(format!("{}/api/account/logout", app.url))
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let response = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let session: Value = response.json().await.unwrap();
    assert_eq!(session["authenticated"], false);
    app.close().await;
}

#[tokio::test]
async fn five_failed_attempts_invalidate_a_challenge_and_success_is_single_use() {
    let app = Fixture::new().await;
    let (challenge, code) = app.code("attempts@example.test").await;
    for _ in 0..5 {
        assert_eq!(
            app.verify(&challenge, "wrong").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        app.verify(&challenge, &code).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let (challenge, code) = app.code("single-use@example.test").await;
    let (first, second) =
        tokio::join!(app.verify(&challenge, &code), app.verify(&challenge, &code));
    let statuses = [first.status(), second.status()];
    assert!(statuses.contains(&StatusCode::OK));
    assert!(statuses.contains(&StatusCode::UNAUTHORIZED));
    app.close().await;
}

#[tokio::test]
async fn email_delivery_is_limited_across_processes_and_pending_codes_keep_working() {
    let app = Fixture::new().await;
    let (challenge, code) = app.code("rate@example.test").await;
    let response = app
        .post(
            "/api/account/email-code",
            json!({ "email": " RATE@EXAMPLE.TEST " }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(app.mail.0.lock().unwrap().len(), 1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_url = format!("http://{}", listener.local_addr().unwrap());
    let second_router = router(app.pool.clone(), app.mail.clone(), second_url.clone())
        .await
        .unwrap();
    let second = tokio::spawn(async move {
        axum::serve(
            listener,
            second_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let response = app
        .client
        .post(format!("{second_url}/api/account/email-code"))
        .header("origin", &second_url)
        .json(&json!({ "email": "rate@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let second_challenge: Value = response.json().await.unwrap();
    assert_ne!(second_challenge["challenge"], challenge);
    assert_eq!(app.mail.0.lock().unwrap().len(), 1);
    assert_eq!(
        app.verify(&second_challenge["challenge"], &code)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.verify(&challenge, &code).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.post(
            "/api/account/email-code",
            json!({ "email": "rate@example.test" })
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    second.abort();
    app.close().await;
}

#[tokio::test]
async fn a_peer_cannot_bypass_delivery_limits_by_changing_email_or_forwarded_ip() {
    let app = Fixture::new().await;
    for index in 0..11 {
        let response = app
            .client
            .post(format!("{}/api/account/email-code", app.url))
            .header("origin", &app.url)
            .header("x-forwarded-for", format!("192.0.2.{index}"))
            .json(&json!({ "email": format!("rate-{index}@example.test") }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if index < 10 {
                StatusCode::ACCEPTED
            } else {
                StatusCode::TOO_MANY_REQUESTS
            }
        );
    }
    for index in 0..31 {
        assert_eq!(
            app.verify(&json!("missing"), "wrong").await.status(),
            if index < 30 {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::TOO_MANY_REQUESTS
            }
        );
    }
    app.close().await;
}

#[tokio::test]
async fn invalid_email_is_rejected_before_delivery() {
    let app = Fixture::new().await;
    for email in [
        "",
        "not-an-email",
        "a@",
        "@example.test",
        "a\r\nb@example.test",
        "a@@example.test",
    ] {
        let response = app
            .post("/api/account/email-code", json!({ "email": email }))
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(app.mail.0.lock().unwrap().is_empty());
    app.close().await;
}

#[tokio::test]
async fn expired_codes_and_sessions_cannot_authenticate_and_sign_in_reuses_the_account() {
    let app = Fixture::new().await;
    let (challenge, code) = app.code("expired@example.test").await;
    query("UPDATE email_codes SET expires_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        app.verify(&challenge, &code).await.status(),
        StatusCode::UNAUTHORIZED
    );
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let (challenge, code) = app.code("expired@example.test").await;
    let response = app.verify(&challenge, &code).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let first: Value = response.json().await.unwrap();
    query("UPDATE web_sessions SET expires_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let response = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    let session: Value = response.json().await.unwrap();
    assert_eq!(session["authenticated"], false);
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
        .execute(&app.pool)
        .await
        .unwrap();
    let (challenge, code) = app.code(" EXPIRED@EXAMPLE.TEST ").await;
    let response = app.verify(&challenge, &code).await;
    assert_eq!(response.status(), StatusCode::OK);
    let second: Value = response.json().await.unwrap();
    assert_eq!(second["account"], first["account"]);
    app.close().await;
}

#[tokio::test]
async fn the_last_sign_in_method_cannot_be_removed() {
    let app = Fixture::new().await;
    let (challenge, code) = app.code("methods@example.test").await;
    let response = app.verify(&challenge, &code).await;
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session: Value = response.json().await.unwrap();
    let response = app
        .client
        .get(format!("{}/api/account/methods", app.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let methods: Value = response.json().await.unwrap();
    assert_eq!(methods["methods"].as_array().unwrap().len(), 1);
    assert_eq!(methods["methods"][0]["kind"], "email");
    let response = app
        .client
        .post(format!("{}/api/account/methods/remove", app.url))
        .header("origin", &app.url)
        .header("cookie", cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .json(&json!({ "id": methods["methods"][0]["id"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    app.close().await;
}

#[derive(Clone)]
struct OAuthMockState {
    authorizations: Arc<Mutex<std::collections::HashMap<String, (String, String)>>>,
    profiles: Arc<Mutex<Value>>,
}

struct OAuthMock {
    url: String,
    server: JoinHandle<()>,
    state: OAuthMockState,
}

impl OAuthMock {
    async fn new() -> Self {
        use axum::{
            Json, Router,
            extract::State,
            routing::{get, post},
        };
        let state = OAuthMockState {
            authorizations: Arc::new(Mutex::new(std::collections::HashMap::new())),
            profiles: Arc::new(Mutex::new(json!({
                "google": {
                    "sub": "google-alice",
                    "email": " Alice@Example.test ",
                    "email_verified": true,
                    "hd": "example.test",
                },
                "github": {
                    "id": 52,
                    "email": "untrusted@example.test",
                },
                "emails": [
                    {
                        "email": "untrusted@example.test",
                        "primary": false,
                        "verified": false,
                    },
                    {
                        "email": "alice@example.test",
                        "primary": true,
                        "verified": true,
                    },
                ],
            }))),
        };
        let app = Router::new()
            .route("/token", post(|State(state): State<OAuthMockState>, axum::Form(body): axum::Form<std::collections::HashMap<String, String>>| async move {
                use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
                use sha2::{Digest, Sha256};
                assert_eq!(body["grant_type"], "authorization_code");
                assert_eq!(body["client_secret"], "test-only");
                let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(body["code_verifier"].as_bytes()));
                let (client_id, redirect) = state.authorizations.lock().unwrap()[&challenge].clone();
                assert_eq!(body["client_id"], client_id);
                assert_eq!(body["redirect_uri"], redirect);

                Json(json!({
                    "access_token": body["code"],
                    "token_type": "Bearer",
                }))
            }))
            .route("/google/userinfo", get(|State(state): State<OAuthMockState>, headers: axum::http::HeaderMap| async move {
                let mut profile = state.profiles.lock().unwrap()["google"].clone();
                profile["email_verified"] = json!(profile["email_verified"] == true && headers["authorization"] == "Bearer verified");
                Json(profile)
            }))
            .route("/github/user", get(|State(state): State<OAuthMockState>| async move {
                Json(state.profiles.lock().unwrap()["github"].clone())
            }))
            .route("/github/emails", get(|State(state): State<OAuthMockState>, headers: axum::http::HeaderMap| async move {
                let mut emails = state.profiles.lock().unwrap()["emails"].clone();
                emails[1]["verified"] = json!(emails[1]["verified"] == true && headers["authorization"] == "Bearer verified");
                Json(emails)
            }))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, server, state }
    }

    fn providers(&self) -> leo_official_service::OAuthProviders {
        leo_official_service::OAuthProviders {
            google: Some(leo_official_service::OAuthProvider {
                client_id: "google-test".into(),
                client_secret: "test-only".into(),
                authorization_url: format!("{}/authorize", self.url),
                token_url: format!("{}/token", self.url),
                userinfo_url: format!("{}/google/userinfo", self.url),
                emails_url: None,
            }),
            github: Some(leo_official_service::OAuthProvider {
                client_id: "github-test".into(),
                client_secret: "test-only".into(),
                authorization_url: format!("{}/authorize", self.url),
                token_url: format!("{}/token", self.url),
                userinfo_url: format!("{}/github/user", self.url),
                emails_url: Some(format!("{}/github/emails", self.url)),
            }),
        }
    }

    fn expect_authorization(&self, url: &url::Url, redirect: String) {
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["redirect_uri"], redirect);
        self.state.authorizations.lock().unwrap().insert(
            params["code_challenge"].clone(),
            (params["client_id"].clone(), redirect),
        );
    }

    async fn attempt(
        &self,
        app: &Fixture,
        name: &str,
        session: Option<(&str, &str)>,
    ) -> reqwest::Response {
        let mut request = app
            .client
            .post(format!("{}/api/account/oauth/{name}/start", app.url))
            .header("origin", &app.url)
            .json(&json!({}));
        if let Some((cookie, csrf)) = session {
            request = request
                .header("cookie", cookie)
                .header("x-csrf-token", csrf);
        }
        let start = request.send().await.unwrap();
        assert_eq!(start.status(), StatusCode::OK);
        let browser = start.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let start: Value = start.json().await.unwrap();
        let authorize = url::Url::parse(start["url"].as_str().unwrap()).unwrap();
        self.expect_authorization(
            &authorize,
            format!("{}/api/account/oauth/{name}/callback", app.url),
        );
        let state = authorize
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .to_string();
        let cookie = session.map_or(browser.clone(), |(cookie, _)| {
            format!("{browser}; {cookie}")
        });
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
            .get(format!(
                "{}/api/account/oauth/{name}/callback?state={state}&code=verified",
                app.url
            ))
            .header("cookie", cookie)
            .send()
            .await
            .unwrap()
    }
}

#[track_caller]
fn assert_oauth_rejected(response: &reqwest::Response) {
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/?sign_in_error=oauth");
    assert!(
        response
            .headers()
            .get_all("set-cookie")
            .iter()
            .all(|value| !value.to_str().unwrap().starts_with("leo_session="))
    );
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
}

#[track_caller]
fn assert_oauth_signed_in(response: &reqwest::Response) {
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/");
    assert!(
        response
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|value| value.to_str().unwrap().starts_with("leo_session="))
    );
}

#[tokio::test]
async fn verified_google_and_github_emails_attach_to_the_same_leo_account() {
    let provider = OAuthMock::new().await;
    let app = Fixture::with_oauth(provider.providers()).await;
    let (challenge, code) = app.code("alice@example.test").await;
    let response = app.verify(&challenge, &code).await;
    let initial_cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let account: Value = response.json().await.unwrap();
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    for (name, scope) in [("google", "openid email"), ("github", "user:email")] {
        let response = if name == "github" {
            app.client
                .post(format!("{}/api/account/oauth/{name}/start", app.url))
                .header("origin", &app.url)
                .header("cookie", &initial_cookie)
                .header("x-csrf-token", account["csrf"].as_str().unwrap())
                .json(&json!({}))
                .send()
                .await
                .unwrap()
        } else {
            app.post(&format!("/api/account/oauth/{name}/start"), json!({}))
                .await
        };
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let start: Value = response.json().await.unwrap();
        let authorize = url::Url::parse(start["url"].as_str().unwrap()).unwrap();
        provider.expect_authorization(
            &authorize,
            format!("{}/api/account/oauth/{name}/callback", app.url),
        );
        let params: std::collections::HashMap<_, _> =
            authorize.query_pairs().into_owned().collect();
        assert_eq!(params["scope"], scope);
        assert_eq!(params["code_challenge_method"], "S256");
        assert!(authorize.as_str().starts_with(&provider.url));
        let callback = format!(
            "{}/api/account/oauth/{name}/callback?state={}&code=verified",
            app.url, params["state"]
        );
        assert_oauth_rejected(&client.get(&callback).send().await.unwrap());
        let response = client
            .get(&callback)
            .header(
                "cookie",
                if name == "github" {
                    format!("{cookie}; {initial_cookie}")
                } else {
                    cookie.clone()
                },
            )
            .send()
            .await
            .unwrap();
        assert_oauth_signed_in(&response);
        let session_cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let session: Value = app
            .client
            .get(format!("{}/api/account/session", app.url))
            .header("cookie", session_cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(session["account"], account["account"]);
        let methods: Value = app
            .client
            .get(format!("{}/api/account/methods", app.url))
            .header("cookie", session_cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let method = methods["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|method| method["kind"] == name)
            .unwrap();
        let removed = app
            .client
            .post(format!("{}/api/account/methods/remove", app.url))
            .header("origin", &app.url)
            .header("cookie", session_cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({ "id": method["id"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);
        let start = app
            .post(&format!("/api/account/oauth/{name}/start"), json!({}))
            .await;
        let removed_cookie = start.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let start: Value = start.json().await.unwrap();
        let authorize = url::Url::parse(start["url"].as_str().unwrap()).unwrap();
        provider.expect_authorization(
            &authorize,
            format!("{}/api/account/oauth/{name}/callback", app.url),
        );
        let params: std::collections::HashMap<_, _> =
            authorize.query_pairs().into_owned().collect();
        let removed_callback = format!(
            "{}/api/account/oauth/{name}/callback?state={}&code=verified",
            app.url, params["state"]
        );
        assert_oauth_rejected(
            &client
                .get(removed_callback)
                .header("cookie", removed_cookie)
                .send()
                .await
                .unwrap(),
        );

        // Re-linking is an explicit mutation of an authenticated account.
        let linked = app
            .client
            .post(format!("{}/api/account/oauth/{name}/start", app.url))
            .header("origin", &app.url)
            .header("cookie", session_cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(linked.status(), StatusCode::OK);
        let browser = linked.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let linked: Value = linked.json().await.unwrap();
        let url = url::Url::parse(linked["url"].as_str().unwrap()).unwrap();
        provider.expect_authorization(
            &url,
            format!("{}/api/account/oauth/{name}/callback", app.url),
        );
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .to_string();
        let link_callback = format!(
            "{}/api/account/oauth/{name}/callback?state={state}&code=verified",
            app.url
        );
        assert_oauth_signed_in(
            &client
                .get(&link_callback)
                .header("cookie", format!("{browser}; {session_cookie}"))
                .send()
                .await
                .unwrap(),
        );

        assert_oauth_rejected(
            &client
                .get(&callback)
                .header("cookie", cookie)
                .send()
                .await
                .unwrap(),
        );
    }
    for name in ["google", "github"] {
        let start = app
            .post(&format!("/api/account/oauth/{name}/start"), json!({}))
            .await;
        let browser = start.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let start: Value = start.json().await.unwrap();
        let url = url::Url::parse(start["url"].as_str().unwrap()).unwrap();
        provider.expect_authorization(
            &url,
            format!("{}/api/account/oauth/{name}/callback", app.url),
        );
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .to_string();
        let callback = format!(
            "{}/api/account/oauth/{name}/callback?state={state}&code=unverified",
            app.url
        );
        assert_oauth_rejected(
            &client
                .get(&callback)
                .header("cookie", &browser)
                .send()
                .await
                .unwrap(),
        );
        assert_oauth_rejected(
            &client
                .get(&callback)
                .header("cookie", &browser)
                .send()
                .await
                .unwrap(),
        );
    }
    provider.server.abort();
    app.close().await;
}

#[tokio::test]
async fn passkeys_and_email_reactivation_preserve_methods_with_small_database_pools() {
    for connections in [1, 5] {
        let app =
            Fixture::with_pool_size(leo_official_service::OAuthProviders::default(), connections)
                .await;
        let (challenge, code) = app.code("passkey@example.test").await;
        let response = app.verify(&challenge, &code).await;
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let session: Value = response.json().await.unwrap();
        let response = app
            .client
            .post(format!("{}/api/account/passkeys/register/start", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let registration: Value = response.json().await.unwrap();
        let mut authenticator = SoftwarePasskey::new(true);
        let credential = authenticator
            .do_registration(
                url::Url::parse(&app.url).unwrap(),
                serde_json::from_value(registration["options"].clone()).unwrap(),
            )
            .unwrap();
        let response = app
            .client
            .post(format!("{}/api/account/passkeys/register/finish", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({
                "challenge": registration["challenge"],
                "credential": credential,
                "label": "Laptop",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let known = app
            .post(
                "/api/account/passkeys/login/start",
                json!({ "email": "passkey@example.test" }),
            )
            .await;
        let unknown = app
            .post(
                "/api/account/passkeys/login/start",
                json!({ "email": "unknown@example.test" }),
            )
            .await;
        assert_eq!(known.status(), StatusCode::OK);
        assert_eq!(unknown.status(), StatusCode::OK);
        let mut known: Value = known.json().await.unwrap();
        let mut unknown: Value = unknown.json().await.unwrap();
        assert_eq!(known["options"]["publicKey"]["allowCredentials"], json!([]));
        assert_eq!(
            unknown["options"]["publicKey"]["allowCredentials"],
            json!([])
        );
        for response in [&mut known, &mut unknown] {
            response["challenge"] = json!("random challenge");
            response["options"]["publicKey"]["challenge"] = json!("random challenge");
        }
        assert_eq!(known, unknown);

        let start = app
            .post(
                "/api/account/passkeys/login/start",
                json!({ "email": "passkey@example.test" }),
            )
            .await;
        assert_eq!(start.status(), StatusCode::OK);
        let browser = start.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let login: Value = start.json().await.unwrap();
        let assertion = authenticator
            .do_authentication(
                url::Url::parse(&app.url).unwrap(),
                serde_json::from_value(login["options"].clone()).unwrap(),
            )
            .unwrap();
        let payload = json!({
            "challenge": login["challenge"],
            "credential": assertion,
        });
        let response = app
            .client
            .post(format!("{}/api/account/passkeys/login/finish", app.url))
            .header("origin", &app.url)
            .header("cookie", &browser)
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let signed_in: Value = response.json().await.unwrap();
        assert_eq!(signed_in["account"], session["account"]);
        let methods: Value = app
            .client
            .get(format!("{}/api/account/methods", app.url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let email_method = methods["methods"]
            .as_array()
            .unwrap()
            .iter()
            .find(|method| method["kind"] == "email")
            .unwrap();
        let response = app
            .client
            .post(format!("{}/api/account/methods/remove", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({ "id": email_method["id"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second'")
            .execute(&app.pool)
            .await
            .unwrap();
        let (email_challenge, email_code) = app.code("passkey@example.test").await;
        assert_eq!(
            app.verify(&email_challenge, &email_code).await.status(),
            StatusCode::UNAUTHORIZED
        );

        let response = app
            .client
            .post(format!("{}/api/account/verify", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({ "challenge": email_challenge, "code": email_code }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let methods: Value = app
            .client
            .get(format!("{}/api/account/methods", app.url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(methods["methods"].as_array().unwrap().len(), 2);
        let remove = |id: Value| {
            app.client
                .post(format!("{}/api/account/methods/remove", app.url))
                .header("origin", &app.url)
                .header("cookie", &cookie)
                .header("x-csrf-token", session["csrf"].as_str().unwrap())
                .json(&json!({ "id": id }))
                .send()
        };
        let (first, second) = tokio::join!(
            remove(methods["methods"][0]["id"].clone()),
            remove(methods["methods"][1]["id"].clone())
        );
        let statuses = [first.unwrap().status(), second.unwrap().status()];
        assert!(statuses.contains(&StatusCode::NO_CONTENT));
        assert!(statuses.contains(&StatusCode::CONFLICT));

        let response = app
            .client
            .post(format!("{}/api/account/passkeys/login/finish", app.url))
            .header("origin", &app.url)
            .header("cookie", &browser)
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        app.close().await;
    }
}

#[tokio::test]
async fn passkeys_require_user_verification_origin_session_binding_and_current_credentials() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let app = Fixture::new().await;
    let (challenge, code) = app.code("proofs@example.test").await;
    let response = app.verify(&challenge, &code).await;
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session: Value = response.json().await.unwrap();
    let signed = |route: &str, body: Value| {
        app.client
            .post(format!("{}{route}", app.url))
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&body)
            .send()
    };
    assert_eq!(
        app.post("/api/account/passkeys/register/start", json!({}))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = app
        .client
        .post(format!("{}/api/account/passkeys/register/start", app.url))
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let origin = url::Url::parse(&app.url).unwrap();
    let mut authenticator = SoftwarePasskey::new(true);
    // Registration without user verification is not sufficient to add a passkey.
    let start: Value = signed("/api/account/passkeys/register/start", json!({}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut unverified = SoftwarePasskey::new(false);
    let mut unverified_options = start["options"].clone();
    unverified_options["publicKey"]["authenticatorSelection"]["userVerification"] =
        json!("discouraged");
    let credential = unverified
        .do_registration(
            origin.clone(),
            serde_json::from_value(unverified_options).unwrap(),
        )
        .unwrap();
    let response = signed(
        "/api/account/passkeys/register/finish",
        json!({
            "challenge": start["challenge"],
            "credential": credential,
            "label": "Unverified",
        }),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let start: Value = signed("/api/account/passkeys/register/start", json!({}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let credential = authenticator
        .do_registration(
            origin.clone(),
            serde_json::from_value(start["options"].clone()).unwrap(),
        )
        .unwrap();
    let registration = json!({
        "challenge": start["challenge"],
        "credential": credential,
        "label": "Verified",
    });

    let (other_challenge, other_code) = app.code("other@example.test").await;
    let response = app.verify(&other_challenge, &other_code).await;
    let other_cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let other: Value = response.json().await.unwrap();
    let response = app
        .client
        .post(format!("{}/api/account/passkeys/register/finish", app.url))
        .header("origin", &app.url)
        .header("cookie", other_cookie)
        .header("x-csrf-token", other["csrf"].as_str().unwrap())
        .json(&registration)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        signed(
            "/api/account/passkeys/register/finish",
            registration.clone()
        )
        .await
        .unwrap()
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        signed("/api/account/passkeys/register/finish", registration)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    for scenario in ["origin", "expiry", "removed"] {
        let start = app
            .post(
                "/api/account/passkeys/login/start",
                json!({ "email": "proofs@example.test" }),
            )
            .await;
        let browser = start.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let start: Value = start.json().await.unwrap();
        let assertion = authenticator
            .do_authentication(
                origin.clone(),
                serde_json::from_value(start["options"].clone()).unwrap(),
            )
            .unwrap();
        let mut proof = serde_json::to_value(assertion).unwrap();
        match scenario {
            "origin" => {
                let bytes = URL_SAFE_NO_PAD
                    .decode(proof["response"]["clientDataJSON"].as_str().unwrap())
                    .unwrap();
                let mut data: Value = serde_json::from_slice(&bytes).unwrap();
                data["origin"] = json!("https://attacker.test");
                proof["response"]["clientDataJSON"] =
                    json!(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&data).unwrap()));
            }
            "expiry" => {
                query("UPDATE sign_in_challenges SET expires_at = now() - interval '1 second'")
                    .execute(&app.pool)
                    .await
                    .unwrap();
            }
            "removed" => {
                let methods: Value = app
                    .client
                    .get(format!("{}/api/account/methods", app.url))
                    .header("cookie", &cookie)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                let passkey = methods["methods"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|method| method["kind"] == "passkey")
                    .unwrap();
                assert_eq!(
                    signed(
                        "/api/account/methods/remove",
                        json!({ "id": passkey["id"] })
                    )
                    .await
                    .unwrap()
                    .status(),
                    StatusCode::NO_CONTENT
                );
            }
            _ => unreachable!(),
        }
        let payload = json!({
            "challenge": start["challenge"],
            "credential": proof,
        });
        let response = app
            .client
            .post(format!("{}/api/account/passkeys/login/finish", app.url))
            .header("origin", &app.url)
            .header("cookie", &browser)
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{scenario}");
        assert!(
            response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("leo_passkey=;")
        );
        assert!(
            response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        let response = app
            .client
            .post(format!("{}/api/account/passkeys/login/finish", app.url))
            .header("origin", &app.url)
            .header("cookie", &browser)
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{scenario} replay"
        );
    }
    app.close().await;
}

#[tokio::test]
async fn existing_accounts_require_authoritative_google_email_or_authenticated_linking() {
    let provider = OAuthMock::new().await;
    provider.state.profiles.lock().unwrap()["google"]["hd"] = Value::Null;
    let app = Fixture::with_oauth(provider.providers()).await;
    let (challenge, code) = app.code("alice@example.test").await;
    let response = app.verify(&challenge, &code).await;
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session: Value = response.json().await.unwrap();
    let csrf = session["csrf"].as_str().unwrap();

    for name in ["google", "github"] {
        let rejected = provider.attempt(&app, name, None).await;
        assert_oauth_rejected(&rejected);
        let linked = provider.attempt(&app, name, Some((&cookie, csrf))).await;
        assert_oauth_signed_in(&linked);
        let signed_in = provider.attempt(&app, name, None).await;
        assert_oauth_signed_in(&signed_in);
    }

    let (challenge, code) = app.code("other@example.test").await;
    let response = app.verify(&challenge, &code).await;
    let other_cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let other: Value = response.json().await.unwrap();
    provider.state.profiles.lock().unwrap()["google"]["email"] = json!("other@example.test");
    provider.state.profiles.lock().unwrap()["emails"][1]["email"] = json!("other@example.test");
    for name in ["google", "github"] {
        assert_oauth_rejected(
            &provider
                .attempt(
                    &app,
                    name,
                    Some((&other_cookie, other["csrf"].as_str().unwrap())),
                )
                .await,
        );
        let fresh = Fixture::with_oauth(provider.providers()).await;
        let (first, second) = tokio::join!(
            provider.attempt(&fresh, name, None),
            provider.attempt(&fresh, name, None)
        );
        for response in [first, second] {
            assert_oauth_signed_in(&response);
        }
        fresh.close().await;
    }
    app.close().await;

    provider.state.profiles.lock().unwrap()["google"]["email"] = json!("alice@gmail.com");
    let gmail = Fixture::with_oauth(provider.providers()).await;
    let (challenge, code) = gmail.code("alice@gmail.com").await;
    assert_eq!(
        gmail.verify(&challenge, &code).await.status(),
        StatusCode::OK
    );
    assert_oauth_signed_in(&provider.attempt(&gmail, "google", None).await);
    gmail.close().await;
    provider.server.abort();
}

#[tokio::test]
async fn oauth_denials_redirect_to_sign_in_and_clear_the_browser_cookie() {
    let provider = OAuthMock::new().await;
    let app = Fixture::with_oauth(provider.providers()).await;
    let start = app.post("/api/account/oauth/google/start", json!({})).await;
    let browser = start.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let start: Value = start.json().await.unwrap();
    let url = url::Url::parse(start["url"].as_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .to_string();
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client
        .get(format!(
            "{}/api/account/oauth/google/callback?state={state}&error=access_denied",
            app.url
        ))
        .header("cookie", &browser)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/?sign_in_error=oauth");
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let response = client
        .get(format!(
            "{}/api/account/oauth/google/callback?state={state}&code=verified",
            app.url
        ))
        .header("cookie", &browser)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["location"], "/?sign_in_error=oauth");
    provider.server.abort();
    app.close().await;
}

#[tokio::test]
async fn a_verified_passkey_confirms_only_its_bound_session_before_account_deletion() {
    let relay = common::RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let mut authenticator = SoftwarePasskey::new(true);
    let registration: Value = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            reqwest::Method::POST,
            "/api/account/passkeys/register/start",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let credential = authenticator
        .do_registration(
            url::Url::parse(&app.url).unwrap(),
            serde_json::from_value(registration["options"].clone()).unwrap(),
        )
        .unwrap();
    let registered = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            reqwest::Method::POST,
            "/api/account/passkeys/register/finish",
        )
        .json(&json!({
            "challenge": registration["challenge"],
            "credential": credential,
            "label": "Confirmation key",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), StatusCode::CREATED);
    query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
        .execute(&app.pool).await.unwrap();
    let (other_cookie, other_session) = common::login(app, "relay-owner@example.test").await;
    query("UPDATE web_sessions SET authenticated_at = NULL")
        .execute(&app.pool)
        .await
        .unwrap();

    let start = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            reqwest::Method::POST,
            "/api/account/passkeys/reauth/start",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);
    let start: Value = start.json().await.unwrap();
    let credential = authenticator
        .do_authentication(
            url::Url::parse(&app.url).unwrap(),
            serde_json::from_value(start["options"].clone()).unwrap(),
        )
        .unwrap();
    let proof = json!({ "challenge": start["challenge"], "credential": credential });
    let cross_session = app
        .authenticated(
            &other_cookie,
            &other_session,
            reqwest::Method::POST,
            "/api/account/passkeys/reauth/finish",
        )
        .json(&proof)
        .send()
        .await
        .unwrap();
    assert_eq!(cross_session.status(), StatusCode::UNAUTHORIZED);
    let confirmed = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            reqwest::Method::POST,
            "/api/account/passkeys/reauth/finish",
        )
        .json(&proof)
        .send()
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::NO_CONTENT);
    assert!(!confirmed.headers().contains_key("set-cookie"));
    let replay = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            reqwest::Method::POST,
            "/api/account/passkeys/reauth/finish",
        )
        .json(&proof)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    let other_delete = app
        .authenticated(
            &other_cookie,
            &other_session,
            reqwest::Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(other_delete.status(), StatusCode::FORBIDDEN);
    let deleted = app
        .authenticated(
            &relay.cookie,
            &relay.session,
            reqwest::Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "relay-owner@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    relay.close().await;
}

#[tokio::test]
async fn oauth_sign_in_requires_an_email_confirmation_before_account_deletion() {
    let provider = OAuthMock::new().await;
    let app = Fixture::with_oauth(provider.providers()).await;
    let response = provider.attempt(&app, "google", None).await;
    assert_oauth_signed_in(&response);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let session: Value = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rejected = app
        .authenticated(
            cookie,
            &session,
            reqwest::Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "alice@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    let (challenge, code) = app.code("alice@example.test").await;
    let confirmed = app
        .authenticated(
            cookie,
            &session,
            reqwest::Method::POST,
            "/api/account/reauth/email",
        )
        .json(&json!({ "challenge": challenge, "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::NO_CONTENT);
    let deleted = app
        .authenticated(
            cookie,
            &session,
            reqwest::Method::POST,
            "/api/account/delete",
        )
        .json(&json!({ "email": "alice@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    app.close().await;
    provider.server.abort();
}
