mod common;

use common::{RelayedInstallation, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn only_the_owner_can_forget_an_installation_and_its_machine_keeps_its_data() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let path = relay
        .installation
        .config
        .data_dir
        .join("installation-relay/identity.json");
    let identity: Value = serde_json::from_slice(&tokio::fs::read(path).await.unwrap()).unwrap();
    let id = identity["installationId"].as_str().unwrap();
    let request = |method: Method, route: &str, cookie: &str, csrf: &str| {
        relay
            .app
            .client
            .request(method, format!("{}{route}", relay.app.url))
            .header("origin", &relay.app.url)
            .header("cookie", cookie)
            .header("x-csrf-token", csrf)
    };
    let csrf = relay.session["csrf"].as_str().unwrap();
    let chat: Value = request(
        Method::POST,
        &format!("/api/installations/{id}/api/chats"),
        &relay.cookie,
        csrf,
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let invitation: Value = request(
        Method::POST,
        &format!("/api/installations/{id}/sharing/invitations"),
        &relay.cookie,
        csrf,
    )
    .json(&json!({ "email": "token-member@example.test" }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let (member_cookie, member_session) = login(&relay.app, "token-member@example.test").await;
    let member_csrf = member_session["csrf"].as_str().unwrap();
    assert_eq!(
        request(
            Method::POST,
            &format!(
                "/api/account/invitations/{}/accept",
                invitation["id"].as_str().unwrap()
            ),
            &member_cookie,
            member_csrf
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NO_CONTENT
    );
    let route = format!("/api/installations/{id}");
    assert_eq!(
        request(Method::DELETE, &route, &member_cookie, member_csrf)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(Method::DELETE, &route, &relay.cookie, "wrong")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        relay
            .app
            .client
            .delete(format!("{}{route}", relay.app.url))
            .header("cookie", &relay.cookie)
            .header("x-csrf-token", csrf)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut stream = relay.get("/chats/stream").send().await.unwrap();
    stream.chunk().await.unwrap().unwrap();
    assert_eq!(
        request(Method::DELETE, &route, &relay.cookie, csrf)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("forget must close the active tunnel and its streams");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let installations: Value = request(Method::GET, "/api/installations", &relay.cookie, csrf)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(installations, json!([]));
    let member_installations: Value = request(
        Method::GET,
        "/api/installations",
        &member_cookie,
        member_csrf,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(member_installations, json!([]));
    assert_eq!(
        relay
            .app
            .post(
                "/api/relay/device-claim/start",
                json!({
                    "name": "Old proof",
                    "protocol": 1,
                    "identity": identity,
                })
            )
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // Claim the same physical installation anew, through the official API and real connector.
    let code: Value = request(
        Method::POST,
        "/api/installations/claim-code",
        &relay.cookie,
        csrf,
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    // Follow the recovery workflow: stop the old connector, back up its private
    // identity and claim again in the manager's permanent identity directory.
    relay.stop.cancel();
    let directory = relay
        .installation
        .config
        .data_dir
        .join("installation-relay");
    tokio::fs::rename(&directory, relay.root.path().join("forgotten-relay"))
        .await
        .unwrap();
    cairn_installation::relay::claim(
        &relay.app.url,
        &directory,
        code["code"].as_str().unwrap(),
        "Renewed installation",
    )
    .await
    .unwrap();
    let new_identity: Value = serde_json::from_slice(
        &tokio::fs::read(directory.join("identity.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(new_identity["installationId"] != identity["installationId"]);
    let stop = tokio_util::sync::CancellationToken::new();
    let connector = tokio::spawn(cairn_installation::relay::connect(
        directory,
        cairn_installation::http::router(relay.installation.clone())
            .await
            .unwrap(),
        relay.installation.clone(),
        stop.clone(),
    ));
    let url = format!(
        "{}/api/installations/{}/api/chats",
        relay.app.url,
        new_identity["installationId"].as_str().unwrap()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = relay
                .app
                .client
                .get(&url)
                .header("cookie", &relay.cookie)
                .send()
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                assert_eq!(response.json::<Value>().await.unwrap()[0]["id"], chat["id"]);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    connector.await.unwrap().unwrap();
    relay.close().await;
}
