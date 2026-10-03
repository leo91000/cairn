mod common;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::Request,
    routing::{get, post},
};
use common::RelayedInstallation;
use reqwest::StatusCode;

#[tokio::test]
async fn the_official_origin_imposes_security_even_on_a_hostile_installation_response() {
    let routes = Router::new().route(
        "/api/fixture/html",
        get(|| async {
            (
                [
                    ("content-type", "text/html"),
                    ("content-security-policy", "default-src * 'unsafe-inline'"),
                    ("x-content-type-options", "permissive"),
                    ("x-frame-options", "SAMEORIGIN"),
                    ("cache-control", "public, max-age=3600"),
                    ("set-cookie", "leo_session=fixture-forged"),
                ],
                "<script>parent.postMessage('hostile', '*')</script>",
            )
        }),
    );
    let relay = RelayedInstallation::new(routes).await;
    let response = relay.get("/fixture/html").send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-security-policy"], "sandbox");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("x-frame-options"));
    assert!(!response.headers().contains_key("set-cookie"));
    assert_eq!(
        response.text().await.unwrap(),
        "<script>parent.postMessage('hostile', '*')</script>"
    );
    relay.close().await;
}

#[tokio::test]
async fn a_large_binary_body_round_trips_through_the_real_installation() {
    let routes = Router::new().route(
        "/api/fixture/echo",
        post(|request: Request| async {
            let bytes = to_bytes(request.into_body(), leo_relay_protocol::MAX_BODY)
                .await
                .unwrap();
            Body::from(bytes)
        }),
    );
    let relay = RelayedInstallation::new(routes).await;
    let body = [0_u8, 128, 255].repeat(2_000_000);
    let response = relay
        .app
        .client
        .post(format!("{}/fixture/echo", relay.base))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .header("content-type", "application/octet-stream")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), body);
    relay.close().await;
}
