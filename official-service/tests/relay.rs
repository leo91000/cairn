mod common;

use common::{Fixture, RelayedInstallation, login};
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn owner_renames_a_relayed_installation_without_changing_its_identity() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let url = format!("{}/api/installations/{id}", app.url);
    let rename = |cookie: &str, csrf: &str, name: &str| {
        app.client
            .patch(&url)
            .header("origin", &app.url)
            .header("cookie", cookie)
            .header("x-csrf-token", csrf)
            .json(&json!({ "name": name }))
    };
    let csrf = relay.session["csrf"].as_str().unwrap();

    assert_eq!(
        rename(&relay.cookie, "", "Forbidden")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    let (foreign, session) = login(app, "foreign@example.test").await;
    assert_eq!(
        rename(&foreign, session["csrf"].as_str().unwrap(), "Foreign")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    for invalid in [" ".to_owned(), "x".repeat(101), "Invalid\nname".to_owned()] {
        assert_eq!(
            rename(&relay.cookie, csrf, &invalid)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    let response = rename(&relay.cookie, csrf, "  Home installation  ")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let session: Value = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["installations"][0]["name"], "Home installation");
    assert_eq!(session["installations"][0]["id"], id);

    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );

    relay.close().await;
}

#[tokio::test]
async fn owner_claims_an_installation_with_a_single_use_code() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "owner@example.test").await;
    let response = app
        .client
        .post(format!("{}/api/installations/claim-code", app.url))
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let claim: Value = response.json().await.unwrap();
    assert_eq!(claim["expiresIn"], 600);
    let claim_body = json!({
        "code": claim["code"],
        "name": "My installation",
        "protocol": 1,
    });
    let response = app
        .client
        .post(format!("{}/api/relay/claim", app.url))
        .json(&claim_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let identity: Value = response.json().await.unwrap();
    assert!(identity["token"].as_str().unwrap().len() >= 32);
    assert_eq!(
        app.client
            .post(format!("{}/api/relay/claim", app.url))
            .json(&claim_body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let session: Value = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        session["installations"][0]["id"],
        identity["installationId"]
    );
    assert_eq!(session["installations"][0]["name"], "My installation");
    app.close().await;
}

#[tokio::test]
async fn owner_uses_the_real_installation_api_over_an_outbound_relay() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let cookie = relay.cookie.clone();
    let session = relay.session.clone();
    let base = &relay.base;
    // Owner-only routes prove the connector uses the trusted owner context,
    // regardless of identity-looking headers supplied by a browser.
    assert_eq!(
        app.client
            .get(format!("{base}/accounts"))
            .header("cookie", &cookie)
            .header("x-leo-role", "member")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.client
            .get(format!("{base}/chats"))
            .header("x-leo-role", "owner")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.client
            .post(format!("{base}/chats"))
            .header("cookie", &cookie)
            .header("origin", &app.url)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.client
            .get(format!("{base}/session"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let request = |path: String, body: Value| {
        app.client
            .post(path)
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&body)
    };
    let response = request(format!("{base}/chats"), json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chat: Value = response.json().await.unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let response = request(
        format!("{base}/chats/{chat_id}/messages"),
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "text": "Relayed message",
        }),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let detail: Value = app
        .client
        .get(format!("{base}/chats/{chat_id}"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["messages"][0]["text"], "Relayed message");
    let attachment_path = format!(
        "{base}/chats/{chat_id}/attachments/{}?name=hostile.svg",
        uuid::Uuid::new_v4()
    );
    let response = app
        .client
        .put(&attachment_path)
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .body("<svg onload='alert(1)'/>")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .client
        .get(&attachment_path)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );

    let (other_cookie, _) = login(app, "other@example.test").await;
    assert_eq!(
        app.client
            .get(format!("{base}/chats"))
            .header("cookie", other_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    relay.close().await;
}
