mod common;

use cairn_beacon::InstallationHosts;
use common::{Fixture, login};
use reqwest::{Method, StatusCode, header};
use serde_json::{Value, json};
use sqlx_core::{query::query, query_as::query_as};

async fn claim(app: &Fixture, cookie: &str, session: &Value) -> (String, String) {
    let code: Value = app
        .authenticated(
            cookie,
            session,
            Method::POST,
            "/api/installations/claim-code",
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let identity: Value = app
        .post(
            "/api/relay/claim",
            json!({
                "code": code["code"],
                "name": "Hosted installation",
                "protocol": 4,
            }),
        )
        .await
        .json()
        .await
        .unwrap();
    let id = identity["installationId"].as_str().unwrap().to_owned();
    let (slug,): (String,) = query_as("SELECT slug FROM installations WHERE id=$1")
        .bind(&id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    (id, slug)
}

#[tokio::test]
async fn registered_hosts_share_the_same_session_but_cannot_target_another_installation() {
    let app = Fixture::with_subdomains().await;
    let (cookie, session) = login(&app, "host-owner@example.test").await;
    let (id, slug) = claim(&app, &cookie, &session).await;
    let (other, _) = claim(&app, &cookie, &session).await;
    let origin = app
        .browser_origin
        .replace("cairn.localhost", &format!("{slug}.cairn.localhost"));
    let authority = reqwest::Url::parse(&origin).unwrap().authority().to_owned();
    let hosted = |method: Method, path: &str| {
        app.client
            .request(method, format!("{}{path}", app.url))
            .header("host", &authority)
            .header("origin", &origin)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
    };
    let response = hosted(Method::GET, "/api/account/session")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .next()
            .is_none()
    );
    let current: Value = response.json().await.unwrap();
    assert_eq!(current["account"]["id"], session["account"]["id"]);
    assert_eq!(
        hosted(Method::PATCH, &format!("/api/installations/{id}"))
            .json(&json!({ "name": "Renamed" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        hosted(Method::PATCH, &format!("/api/installations/{other}"))
            .json(&json!({ "name": "Crossed host" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        hosted(Method::POST, "/api/account/passkeys/login/start")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        hosted(Method::GET, "/api/account/notifications")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let bad_origin = hosted(Method::PATCH, &format!("/api/installations/{id}"))
        .header("origin", &app.browser_origin)
        .json(&json!({ "name": "Wrong origin" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_origin.status(), StatusCode::FORBIDDEN);
    let cross_site_stream = hosted(
        Method::GET,
        &format!("/api/installations/{id}/direct/unknown/events"),
    )
    .header("origin", "https://evil.example")
    .send()
    .await
    .unwrap();
    assert_eq!(cross_site_stream.status(), StatusCode::FORBIDDEN);
    let (sessions,): (i64,) = query_as("SELECT count(*) FROM web_sessions")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(sessions, 1);
    assert_eq!(
        hosted(Method::POST, "/api/account/logout")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let after: Value = app
        .authenticated(&cookie, &session, Method::GET, "/api/account/session")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["authenticated"], false);
    app.close().await;
}

#[tokio::test]
async fn legacy_cookie_migration_keeps_the_proof_deadline_and_rejects_unknown_hosts() {
    let app = Fixture::with_subdomains().await;
    let (cookie, session) = login(&app, "cookie-owner@example.test").await;
    let (before,): (String,) = query_as(
        "SELECT concat(digest,':',csrf,':',expires_at,':',last_proof_at) FROM web_sessions",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let response = app
        .authenticated(&cookie, &session, Method::GET, "/api/account/session")
        .send()
        .await
        .unwrap();
    let headers = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(headers.len(), 2);
    assert!(headers[0].contains("Max-Age=0"));
    assert!(!headers[0].contains("Domain="));
    assert!(headers[1].contains("Domain=cairn.localhost"));
    assert!(headers[1].starts_with(&cookie));
    let (after,): (String,) = query_as(
        "SELECT concat(digest,':',csrf,':',expires_at,':',last_proof_at) FROM web_sessions",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    let port = reqwest::Url::parse(&app.browser_origin)
        .unwrap()
        .port()
        .unwrap();
    for host in [
        format!("unknown.cairn.localhost:{port}"),
        format!("a.b.cairn.localhost:{port}"),
        format!("cairn.localhost.evil.test:{port}"),
        "cairn.localhost:1".into(),
    ] {
        let response = app
            .client
            .get(format!("{}/api/account/session", app.url))
            .header("host", host)
            .header("cookie", &cookie)
            .header(
                "x-forwarded-host",
                reqwest::Url::parse(&app.browser_origin)
                    .unwrap()
                    .authority(),
            )
            .send()
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::MISDIRECTED_REQUEST
        ));
    }
    let response = app
        .authenticated(
            &format!("{cookie}; cairn_session=different"),
            &session,
            Method::GET,
            "/api/account/session",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    query("DELETE FROM web_sessions")
        .execute(&app.pool)
        .await
        .unwrap();
    app.close().await;
}

#[test]
fn production_cookies_require_https_and_scope_only_the_shared_session() {
    assert!(InstallationHosts::new("http://cairn.example", true).is_err());
    assert!(InstallationHosts::new("http://127.0.0.1:4311", true).is_err());
    assert!(InstallationHosts::new("http://localhost:4311", true).is_err());
    let hosts = InstallationHosts::new("https://cairn.example", true).unwrap();
    let shell = hosts.shell("<div id=\"app\" data-beacon></div>");
    assert!(shell.contains("data-beacon-origin=\"https://cairn.example\""));
    assert!(shell.contains("data-beacon-subdomains=\"true\""));
}
