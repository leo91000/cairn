mod common;

use common::RelayedInstallation;
use serde_json::Value;
use std::time::Duration;

async fn session(relay: &RelayedInstallation) -> Value {
    relay
        .app
        .client
        .get(format!("{}/api/account/session", relay.app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn beacon_session_reports_the_installations_actual_tunnel_availability() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    assert_eq!(session(&relay).await["installations"][0]["online"], true);

    relay.stop.cancel();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if session(&relay).await["installations"][0]["online"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("disconnect must promptly mark the installation offline");
    relay.close().await;
}
