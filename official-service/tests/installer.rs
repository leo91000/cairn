mod common;

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
    assert!(script.contains(&format!("LEO_OFFICIAL_ORIGIN='{}'", app.url)));
    assert!(!script.contains("__LEO_OFFICIAL_ORIGIN__"));
    assert!(script.contains("KVM"));
    app.close().await;
}
