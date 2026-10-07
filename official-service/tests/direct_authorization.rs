mod common;

use common::RelayedInstallation;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

type FrameFilter = Arc<dyn Fn(leo_relay_protocol::Frame, bool) -> Option<leo_relay_protocol::Frame> + Send + Sync>;

// Perturb only the wire between both real peers, as packet loss or an old
// service would. Authorization still crosses the official HTTP interface.
async fn filter_tunnel(relay: &mut RelayedInstallation, filter: FrameFilter) -> tokio::task::JoinHandle<()> {
    use axum::{extract::{Path, WebSocketUpgrade, ws::Message}, http::HeaderMap, routing::get};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message as UpstreamMessage, client::IntoClientRequest};

    relay.stop.cancel();
    (&mut relay.connector).await.unwrap().unwrap();
    let official = relay.app.url.replace("http:", "ws:");
    let routes = axum::Router::new().route("/api/relay/{installation}/connect", get(move |Path(id): Path<String>, headers: HeaderMap, upgrade: WebSocketUpgrade| {
        let official = official.clone();
        let filter = filter.clone();
        async move {
            upgrade.on_upgrade(move |mut installation| async move {
                let mut request = format!("{official}/api/relay/{id}/connect").into_client_request().unwrap();
                request.headers_mut().insert("authorization", headers["authorization"].clone());
                let (mut upstream, _) = tokio_tungstenite::connect_async(request).await.unwrap();
                loop {
                    tokio::select! {
                        message = installation.recv() => {
                            let Some(Ok(message)) = message else { break };
                            let message = match message {
                                Message::Text(text) => {
                                    let frame = serde_json::from_str(&text).unwrap();
                                    let Some(frame) = filter(frame, false) else { continue };
                                    UpstreamMessage::Text(serde_json::to_string(&frame).unwrap().into())
                                },
                                Message::Ping(bytes) => UpstreamMessage::Ping(bytes),
                                Message::Pong(bytes) => UpstreamMessage::Pong(bytes),
                                _ => break,
                            };
                            if upstream.send(message).await.is_err() { break }
                        },
                        message = upstream.next() => {
                            let Some(Ok(message)) = message else { break };
                            let message = match message {
                                UpstreamMessage::Text(text) => {
                                    let frame = serde_json::from_str(&text).unwrap();
                                    let Some(frame) = filter(frame, true) else { continue };
                                    Message::Text(serde_json::to_string(&frame).unwrap().into())
                                },
                                UpstreamMessage::Ping(bytes) => Message::Ping(bytes),
                                UpstreamMessage::Pong(bytes) => Message::Pong(bytes),
                                _ => break,
                            };
                            if installation.send(message).await.is_err() { break }
                        },
                    }
                }
            })
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap(); });
    let directory = relay.installation.config.data_dir.join("installation-relay");
    let identity_path = directory.join("identity.json");
    let mut identity: Value = serde_json::from_slice(&tokio::fs::read(&identity_path).await.unwrap()).unwrap();
    identity["origin"] = json!(origin);
    tokio::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).await.unwrap();
    relay.stop = tokio_util::sync::CancellationToken::new();
    relay.connector = tokio::spawn(leo_agent_manager::relay::connect_with_direct(
        directory,
        leo_agent_manager::http::router(relay.installation.clone()).await.unwrap(),
        relay.installation.clone(),
        relay.stop.clone(),
        relay.direct.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::OK {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    server
}

fn fingerprint() -> String {
    format!("sha-256 {}", vec!["AB"; 32].join(":"))
}

#[tokio::test]
async fn signaling_rejects_conversation_content_disguised_as_an_ice_candidate() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!(
                "/api/installations/{}/direct/{}/signal",
                grant.claims.installation_id, grant.claims.connection_id
            ),
        )
        .json(&json!({
            "kind": "candidate",
            "candidate": "candidate:conversation content must not enter signaling",
            "sdp_mid": "0",
            "sdp_m_line_index": 0,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        relay
            .direct
            .send_signal(
                &grant.claims.connection_id,
                leo_relay_protocol::direct::DirectSignal::Candidate {
                    candidate: "candidate:conversation content must not enter signaling".into(),
                    sdp_mid: Some("0".into()),
                    sdp_m_line_index: Some(0),
                }
            )
            .is_err()
    );
    relay.close().await;
}

#[tokio::test]
async fn owner_receives_a_session_bound_grant_verified_by_the_real_installation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/direct/authorize"),
        )
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let grant: Value = response.json().await.unwrap();
    assert_eq!(grant["available"], true);
    assert!(grant["grant"]["signature"].is_string());
    let authorization: leo_relay_protocol::direct::DirectAuthorization =
        serde_json::from_value(grant["grant"].clone()).unwrap();
    assert_eq!(authorization.claims.installation_id, id);
    assert_eq!(
        authorization.claims.account_id,
        relay.session["account"]["id"].as_str().unwrap()
    );
    assert_eq!(authorization.claims.role, leo_relay_protocol::Role::Owner);
    let lease = relay
        .direct
        .accept_peer(
            &authorization,
            &authorization.claims.session_id,
            &fingerprint(),
        )
        .unwrap();
    assert!(!lease.closed.is_cancelled());
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

async fn authorization(
    relay: &RelayedInstallation,
    cookie: &str,
    session: &Value,
) -> leo_relay_protocol::direct::DirectAuthorization {
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let response = relay
        .app
        .authenticated(
            cookie,
            session,
            Method::POST,
            &format!("/api/installations/{id}/direct/authorize"),
        )
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_value(response.json::<Value>().await.unwrap()["grant"].clone()).unwrap()
}

#[tokio::test]
async fn logout_revokes_only_that_sessions_direct_lease_and_preserves_the_relay() {
    use leo_agent_manager::direct::DirectEvent;
    use leo_relay_protocol::direct::DirectRevocation;
    use std::time::Duration;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    let mut events = relay.direct.subscribe();
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/account/logout",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(1), lease.closed.cancelled())
        .await
        .unwrap();
    match events.recv().await.unwrap() {
        DirectEvent::Revoked(DirectRevocation::Session { session_id }) => {
            assert_eq!(session_id, grant.claims.session_id)
        }
        _ => panic!("logout must revoke only its public session identifier"),
    }
    assert!(events.try_recv().is_err());
    assert!(!relay.installation.shutdown.is_cancelled());
    relay.close().await;
}

#[tokio::test]
async fn altered_replayed_other_session_and_other_fingerprint_grants_are_refused() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    assert!(
        relay
            .direct
            .accept_peer(&grant, "another-session", &fingerprint())
            .is_err()
    );
    let other_fingerprint = fingerprint().replace("AB", "CD");
    assert!(
        relay
            .direct
            .accept_peer(&grant, &grant.claims.session_id, &other_fingerprint)
            .is_err()
    );
    let mut altered = grant.clone();
    altered.claims.role = leo_relay_protocol::Role::Member;
    assert!(
        relay
            .direct
            .accept_peer(&altered, &grant.claims.session_id, &fingerprint())
            .is_err()
    );
    altered = grant.clone();
    altered.signature.push('a');
    assert!(
        relay
            .direct
            .accept_peer(&altered, &grant.claims.session_id, &fingerprint())
            .is_err()
    );
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    assert!(!lease.closed.is_cancelled());
    assert!(
        relay
            .direct
            .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
            .is_err()
    );
    relay.close().await;
}

#[tokio::test]
async fn only_current_owner_or_member_sessions_can_obtain_a_grant() {
    use common::{login, stream_accounts};
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let route = format!("/api/installations/{id}/direct/authorize");
    let members = stream_accounts(&relay).await;
    let member_cookie = &members[1];
    let member_session: Value = relay
        .app
        .client
        .get(format!("{}/api/account/session", relay.app.url))
        .header("cookie", member_cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let member_grant = authorization(&relay, member_cookie, &member_session).await;
    assert_eq!(member_grant.claims.role, leo_relay_protocol::Role::Member);
    let lease = relay
        .direct
        .accept_peer(
            &member_grant,
            &member_grant.claims.session_id,
            &fingerprint(),
        )
        .unwrap();
    assert!(matches!(
        lease.identity().role,
        leo_agent_manager::auth::InstallationRole::Member
    ));
    let (foreign_cookie, foreign_session) = login(&relay.app, "foreign@example.test").await;
    let response = relay
        .app
        .authenticated(&foreign_cookie, &foreign_session, Method::POST, &route)
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = relay
        .app
        .client
        .post(format!("{}{route}", relay.app.url))
        .header("origin", &relay.app.url)
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    for (origin, csrf) in [
        (
            "https://foreign.test",
            relay.session["csrf"].as_str().unwrap(),
        ),
        (relay.app.url.as_str(), "wrong-csrf"),
    ] {
        let response = relay
            .app
            .client
            .post(format!("{}{route}", relay.app.url))
            .header("cookie", &relay.cookie)
            .header("origin", origin)
            .header("x-csrf-token", csrf)
            .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &format!(
                "/api/installations/{id}/sharing/members/{}",
                member_grant.claims.account_id
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    tokio::time::timeout(std::time::Duration::from_secs(1), lease.closed.cancelled())
        .await
        .unwrap();
    let response = relay
        .app
        .authenticated(member_cookie, &member_session, Method::POST, &route)
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    relay.close().await;
}

#[tokio::test]
async fn session_expiration_closes_the_lease_and_sends_only_session_revocation() {
    use leo_agent_manager::direct::DirectEvent;
    use leo_relay_protocol::direct::DirectRevocation;
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    sqlx_core::query::query(
        "UPDATE web_sessions SET expires_at = clock_timestamp() + interval '3 seconds'",
    )
    .execute(&relay.app.pool)
    .await
    .unwrap();
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    let mut events = relay.direct.subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(4), lease.closed.cancelled())
        .await
        .unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        DirectEvent::Revoked(DirectRevocation::Session { session_id }) => {
            assert_eq!(session_id, grant.claims.session_id)
        }
        _ => panic!("expiry must send only the expired session scope"),
    }
    assert!(events.try_recv().is_err());
    assert!(
        relay
            .direct
            .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
            .is_err()
    );
    assert!(!relay.installation.shutdown.is_cancelled());
    relay.close().await;
}
