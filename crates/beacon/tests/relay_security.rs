mod common;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderValue, Response},
    routing::{get, post},
};
use common::RelayedInstallation;
use reqwest::StatusCode;

#[tokio::test]
async fn contradictory_content_types_cannot_bypass_the_beacon_sandbox() {
    let routes = Router::new().route(
        "/api/fixture/ambiguous",
        get(|| async {
            let mut response = Response::new(Body::from(
                "<script>document.body.dataset.executed = 'yes'</script>",
            ));
            response
                .headers_mut()
                .append("content-type", HeaderValue::from_static("application/json"));
            response
                .headers_mut()
                .append("content-type", HeaderValue::from_static("text/html"));
            response
        }),
    );
    let relay = RelayedInstallation::new(routes).await;
    let response = relay.get("/fixture/ambiguous").send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get_all("content-type").iter().count(), 2);
    assert_eq!(
        response.headers().get("content-security-policy"),
        Some(&HeaderValue::from_static("sandbox")),
    );
    relay.close().await;
}

#[tokio::test]
async fn the_beacon_origin_imposes_security_even_on_a_hostile_installation_response() {
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
                    ("set-cookie", "cairn_session=fixture-forged"),
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
            let bytes = to_bytes(request.into_body(), cairn_protocol::MAX_BODY)
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
