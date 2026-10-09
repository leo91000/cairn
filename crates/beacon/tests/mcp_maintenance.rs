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

#[tokio::test]
async fn beacon_login_claim_and_token_quotas_share_each_ipv6_prefix() {
    let app = Fixture::with_network(Default::default(), 5, "127.0.0.1".parse().unwrap()).await;
    let cases = [
        (
            "/api/account/email-code",
            10,
            json!({ "email": "invalid" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/account/verify",
            30,
            json!({ "challenge": "unknown", "code": "wrong" }),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/api/relay/claim",
            30,
            json!({
                "code": "unknown",
                "name": "Test",
                "protocol": 1,
            }),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/oauth/token",
            60,
            json!({ "grant_type": "invalid" }),
            StatusCode::BAD_REQUEST,
        ),
    ];

    for (route, maximum, body, initial_status) in cases {
        for attempt in 0..=maximum {
            let request = app
                .client
                .post(format!("{}{route}", app.url))
                .header("origin", &app.url)
                .header(
                    "x-forwarded-for",
                    format!("2001:db8:abcd:1234::{:x}", attempt + 1),
                );
            let response = if route == "/oauth/token" {
                request.form(&body)
            } else {
                request.json(&body)
            }
            .send()
            .await
            .unwrap();
            assert_eq!(
                response.status(),
                if attempt < maximum {
                    initial_status
                } else {
                    StatusCode::TOO_MANY_REQUESTS
                },
                "{route} attempt {attempt}"
            );
        }
    }

    app.close().await;
}

#[tokio::test]
async fn ipv4_mapped_addresses_and_untrusted_headers_cannot_select_fresh_buckets() {
    for trusted_proxy in [true, false] {
        let proxies = if trusted_proxy {
            "127.0.0.1".parse().unwrap()
        } else {
            Default::default()
        };

        let app = Fixture::with_network(Default::default(), 5, proxies).await;

        for attempt in 0..11 {
            let forwarded = if trusted_proxy {
                if attempt % 2 == 0 {
                    "192.0.2.1".to_string()
                } else {
                    "::ffff:192.0.2.1".to_string()
                }
            } else {
                format!("2001:db8:{}::1", attempt + 1)
            };
            let response = app
                .client
                .post(format!("{}/oauth/register", app.url))
                .header("x-forwarded-for", forwarded)
                .json(&json!({ "redirect_uris": ["https://client.example/callback"] }))
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

        if trusted_proxy {
            let response = app
                .client
                .post(format!("{}/oauth/register", app.url))
                .header("x-forwarded-for", "192.0.2.2")
                .json(&json!({ "redirect_uris": ["https://other.example/callback"] }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
        }

        app.close().await;
    }
}
