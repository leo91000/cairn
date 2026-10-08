mod common;

use common::Fixture;
use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn rotating_ipv6_addresses_in_one_prefix_cannot_bypass_client_registration_limits() {
    let app = Fixture::with_network(Default::default(), 5, "127.0.0.1".parse().unwrap()).await;
    for attempt in 0..11 {
        let response = app
            .client
            .post(format!("{}/oauth/register", app.url))
            .header(
                "x-forwarded-for",
                format!("2001:db8:1234:5678::{:x}", attempt + 1),
            )
            .json(&json!({
                "client_name": "Prefix quota",
                "redirect_uris": ["https://client.example/callback"],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if attempt < 10 {
                StatusCode::CREATED
            } else {
                StatusCode::TOO_MANY_REQUESTS
            }
        );
    }

    let response = app
        .client
        .post(format!("{}/oauth/register", app.url))
        .header("x-forwarded-for", "2001:db8:1234:5679::1")
        .json(&json!({ "redirect_uris": ["https://other.example/callback"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    app.close().await;
}
