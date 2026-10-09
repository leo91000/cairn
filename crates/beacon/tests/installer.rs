mod common;

use sha2::{Digest, Sha256};

#[tokio::test]
async fn official_service_serves_a_root_installer_without_putting_claim_codes_in_urls() {
    let app = common::Fixture::new().await;
    let response = app
        .client
        .get(format!("{}/install.sh", app.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/x-shellscript; charset=utf-8"
    );
    let script = response.text().await.unwrap();
    assert!(script.starts_with("#!/usr/bin/env bash"));
    assert!(script.contains(&format!("CAIRN_BEACON_ORIGIN='{}'", app.url)));
    assert!(!script.contains("__CAIRN_BEACON_ORIGIN__"));
    assert!(script.contains("KVM"));
    let host = app
        .client
        .get(format!("{}/install/host.py", app.url))
        .send()
        .await
        .unwrap();
    assert_eq!(host.status(), 200);
    assert_eq!(host.headers()["cache-control"], "no-store");
    let host = host.text().await.unwrap();
    assert!(host.contains("CAIRN_INSTALLATION_CLAIM_CODE"));
    let checksum = hex::encode(Sha256::digest(host.as_bytes()));
    assert!(script.contains(&format!("CAIRN_HOST_SHA256={checksum}")));
    assert!(script.contains("--proto '=https'"));
    app.close().await;
}

#[tokio::test]
async fn release_is_unavailable_until_the_operator_approves_an_immutable_image() {
    let image = format!(
        "ghcr.io/leo91000/cairn@sha256:{}",
        "1".repeat(64)
    );
    assert!(
        cairn_beacon::installer::release_router(Some(
            "ghcr.io/leo91000/cairn:latest".into()
        ))
        .is_err()
    );
    for approved in [None, Some(image.clone())] {
        let router = cairn_beacon::installer::release_router(approved.clone()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/install/release", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        match approved {
            Some(image) => {
                assert_eq!(response.status(), 200);
                assert_eq!(
                    response.json::<serde_json::Value>().await.unwrap()["image"],
                    image
                );
            }
            None => assert_eq!(response.status(), 503),
        }
        server.abort();
    }
}
