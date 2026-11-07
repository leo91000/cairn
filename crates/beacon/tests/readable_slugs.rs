mod common;

use common::{Fixture, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx_core::query::query;

#[tokio::test]
async fn installation_slugs_are_unique_stable_and_only_listed_for_authorized_accounts() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "slugs-owner@example.test").await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        let code: Value = app
            .authenticated(
                &cookie,
                &session,
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
                    "name": "Same display name",
                    "protocol": cairn_protocol::PROTOCOL_VERSION,
                }),
            )
            .await
            .json()
            .await
            .unwrap();
        ids.push(identity["installationId"].as_str().unwrap().to_owned());
    }
    let installations: Value = app
        .authenticated(&cookie, &session, Method::GET, "/api/installations")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let slug = installations[0]["slug"].as_str().unwrap().to_owned();
    assert_ne!(installations[0]["slug"], installations[1]["slug"]);
    assert_eq!(slug.split('-').count(), 3);

    let response = app
        .authenticated(
            &cookie,
            &session,
            Method::PATCH,
            &format!("/api/installations/{}", ids[0]),
        )
        .json(&json!({ "name": "Renamed installation" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let (stored,): (String,) =
        sqlx_core::query_as::query_as("SELECT slug FROM installations WHERE id=$1")
            .bind(&ids[0])
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(stored, slug);
    assert!(
        query("UPDATE installations SET slug=$1 WHERE id=$2")
            .bind(&slug)
            .bind(&ids[1])
            .execute(&app.pool)
            .await
            .is_err()
    );

    let (other_cookie, other_session) = login(&app, "slugs-stranger@example.test").await;
    let other: Value = app
        .authenticated(
            &other_cookie,
            &other_session,
            Method::GET,
            "/api/installations",
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other, json!([]));
    app.close().await;
}
