mod common;

use common::RelayedInstallation;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

type FrameFilter =
    Arc<dyn Fn(leo_relay_protocol::Frame, bool) -> Option<leo_relay_protocol::Frame> + Send + Sync>;

// Perturb only the wire between both real peers, as packet loss or an old
// service would. Authorization still crosses the official HTTP interface.
async fn filter_tunnel(
    relay: &mut RelayedInstallation,
    filter: FrameFilter,
) -> tokio::task::JoinHandle<()> {
    filter_tunnel_with_pause(relay, filter, tokio::sync::watch::channel(false).1).await
}

async fn filter_tunnel_with_pause(
    relay: &mut RelayedInstallation,
    filter: FrameFilter,
    paused: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    use axum::{
        extract::{Path, WebSocketUpgrade, ws::Message},
        http::HeaderMap,
        routing::get,
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message as UpstreamMessage, client::IntoClientRequest};

    relay.stop.cancel();
    (&mut relay.connector).await.unwrap().unwrap();
    let policy_origin = relay.app.url.clone();
    let official = relay.app.url.replace("http:", "ws:");
    let routes = axum::Router::new().route("/api/relay/{installation}/connect", get(move |Path(id): Path<String>, headers: HeaderMap, upgrade: WebSocketUpgrade| {
        let official = official.clone();
        let filter = filter.clone();
        let mut paused = paused.clone();
        async move {
            upgrade.on_upgrade(move |mut installation| async move {
                let mut request = format!("{official}/api/relay/{id}/connect").into_client_request().unwrap();
                request.headers_mut().insert("authorization", headers["authorization"].clone());
                let (mut upstream, _) = tokio_tungstenite::connect_async(request).await.unwrap();
                loop {
                    tokio::select! {
                        Ok(()) = paused.changed() => {},
                        message = installation.recv() => {
                            let Some(Ok(message)) = message else {
                                break;
                            };
                            let message = match message {
                                Message::Text(text) => {
                                    let frame = serde_json::from_str(&text).unwrap();
                                    let Some(frame) = filter(frame, false) else {
                                        continue;
                                    };
                                    UpstreamMessage::Text(serde_json::to_string(&frame).unwrap().into())
                                },
                                Message::Ping(bytes) => UpstreamMessage::Ping(bytes),
                                Message::Pong(bytes) => UpstreamMessage::Pong(bytes),
                                _ => break,
                            };
                            if upstream.send(message).await.is_err() {
                                break;
                            }
                        },
                        message = upstream.next(), if !*paused.borrow() => {
                            let Some(Ok(message)) = message else {
                                break;
                            };
                            let message = match message {
                                UpstreamMessage::Text(text) => {
                                    let frame = serde_json::from_str(&text).unwrap();
                                    let Some(frame) = filter(frame, true) else {
                                        continue;
                                    };
                                    Message::Text(serde_json::to_string(&frame).unwrap().into())
                                },
                                UpstreamMessage::Ping(bytes) => Message::Ping(bytes),
                                UpstreamMessage::Pong(bytes) => Message::Pong(bytes),
                                _ => break,
                            };
                            if installation.send(message).await.is_err() {
                                break;
                            }
                        },
                    }
                }
            })
        }
    }));
    let routes = routes.route(
        "/api/relay/{installation}/task-authors",
        get(move |Path(id): Path<String>, headers: HeaderMap| {
            let origin = policy_origin.clone();
            async move {
                let response = reqwest::Client::new()
                    .get(format!("{origin}/api/relay/{id}/task-authors"))
                    .header("authorization", headers["authorization"].clone())
                    .send()
                    .await
                    .unwrap();
                (response.status(), response.bytes().await.unwrap())
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let directory = relay
        .installation
        .config
        .data_dir
        .join("installation-relay");
    let identity_path = directory.join("identity.json");
    let mut identity: Value =
        serde_json::from_slice(&tokio::fs::read(&identity_path).await.unwrap()).unwrap();
    identity["origin"] = json!(origin);
    tokio::fs::write(identity_path, serde_json::to_vec(&identity).unwrap())
        .await
        .unwrap();
    relay.stop = tokio_util::sync::CancellationToken::new();
    relay.connector = tokio::spawn(leo_agent_manager::relay::connect_with_direct(
        directory,
        relay.router.clone(),
        relay.installation.clone(),
        relay.stop.clone(),
        relay.direct.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::OK {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    server
}

fn fingerprint() -> String {
    format!("sha-256 {}", vec!["AB"; 32].join(":"))
}

#[tokio::test]
async fn signaling_keeps_early_answers_and_admits_only_one_reader_per_connection() {
    use leo_relay_protocol::{Frame, direct::DirectSignal};
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let sent = Arc::new(tokio::sync::Notify::new());
    let observed = sent.clone();
    let proxy = filter_tunnel(
        &mut relay,
        Arc::new(move |frame, official| {
            if !official && matches!(&frame, Frame::DirectSignal { .. }) {
                observed.notify_one();
            }
            Some(frame)
        }),
    )
    .await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let answer = DirectSignal::Answer {
        sdp: format!(
            "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=fingerprint:{}\r\na=sctp-port:5000\r\n",
            fingerprint().replace("AB", "CD")
        ),
    };
    relay
        .direct
        .send_signal(&grant.claims.connection_id, answer.clone())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), sent.notified())
        .await
        .unwrap();
    // Its acknowledgement crosses the same ordered socket after the answer,
    // proving the official peer received the answer before an SSE reader opens.
    authorization(&relay, &relay.cookie, &relay.session).await;
    let route = format!(
        "{}/api/installations/{}/direct/{}/events",
        relay.app.url, grant.claims.installation_id, grant.claims.connection_id
    );
    let request = || relay.app.client.get(&route).header("cookie", &relay.cookie);
    let mut reader = request().send().await.unwrap();
    assert_eq!(reader.status(), StatusCode::OK);
    let chunk = tokio::time::timeout(Duration::from_secs(1), reader.chunk())
        .await
        .expect("answer sent before subscription must remain queued")
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&chunk).contains("answer"));
    assert_eq!(
        request().send().await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(reader);
    let mut reopened = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = request().send().await.unwrap();
            if response.status() == StatusCode::OK {
                break response;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    relay
        .direct
        .send_signal(&grant.claims.connection_id, answer)
        .unwrap();
    let chunk = tokio::time::timeout(Duration::from_secs(1), reopened.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&chunk).contains("answer"));
    drop(reopened);
    relay.close().await;
    proxy.abort();
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
async fn a_refused_renewal_preserves_the_peer_and_allows_a_verified_retry() {
    use leo_relay_protocol::Frame;
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let refuse_once = AtomicBool::new(true);
    let proxy = filter_tunnel(
        &mut relay,
        Arc::new(move |frame, _| {
            if let Frame::DirectRenew {
                id,
                mut authorization,
            } = frame
            {
                if refuse_once.swap(false, Ordering::SeqCst) {
                    authorization.signature.push('a');
                }
                Some(Frame::DirectRenew { id, authorization })
            } else {
                Some(frame)
            }
        }),
    )
    .await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    let route = format!(
        "/api/installations/{}/direct/{}/renew",
        grant.claims.installation_id, grant.claims.connection_id
    );
    let request = || {
        relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &route)
            .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
    };
    assert_eq!(
        request().send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert!(!lease.closed.is_cancelled());
    let response = request().send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let renewed: leo_relay_protocol::direct::DirectAuthorization =
        serde_json::from_value(response.json::<Value>().await.unwrap()["grant"].clone()).unwrap();
    assert_ne!(renewed.claims.nonce, grant.claims.nonce);
    assert_eq!(renewed.claims.connection_id, grant.claims.connection_id);
    assert_eq!(renewed.claims.role, grant.claims.role);
    assert!(!lease.closed.is_cancelled());
    assert!(
        relay
            .direct
            .accept_peer(&renewed, &renewed.claims.session_id, &fingerprint())
            .is_err(),
        "renewal must not admit a second peer"
    );
    relay.close().await;
    proxy.abort();
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
    let mut client_events = relay
        .app
        .client
        .get(format!(
            "{}/api/installations/{}/direct/{}/events",
            relay.app.url, grant.claims.installation_id, grant.claims.connection_id
        ))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(client_events.status(), StatusCode::OK);
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
    assert!(
        tokio::time::timeout(Duration::from_secs(1), client_events.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    drop(client_events);
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
    let owner_grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let owner_lease = relay
        .direct
        .accept_peer(&owner_grant, &owner_grant.claims.session_id, &fingerprint())
        .unwrap();
    let mut events = relay.direct.subscribe();
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
    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        leo_agent_manager::direct::DirectEvent::Revoked(
            leo_relay_protocol::direct::DirectRevocation::Account {
                account_id,
                generation,
            },
        ) => {
            assert_eq!(account_id, member_grant.claims.account_id);
            assert_eq!(generation, 1);
        }
        _ => panic!("member removal must revoke only that account"),
    }
    assert!(events.try_recv().is_err());
    assert!(!owner_lease.closed.is_cancelled());
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

#[tokio::test]
async fn session_revocation_preserves_other_devices_and_denies_further_grants_or_renewal() {
    use common::login;
    use leo_agent_manager::direct::DirectEvent;
    use leo_relay_protocol::direct::DirectRevocation;
    for revoke_others in [false, true] {
        let relay = RelayedInstallation::new(axum::Router::new()).await;
        sqlx_core::query::query("UPDATE account_rate_limits SET resets_at = now() - interval '1 second' WHERE key LIKE 'email:%'")
            .execute(&relay.app.pool).await.unwrap();
        let (other_cookie, other_session) = login(&relay.app, "relay-owner@example.test").await;
        let grant = authorization(&relay, &other_cookie, &other_session).await;
        let lease = relay
            .direct
            .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
            .unwrap();
        let owner_grant = authorization(&relay, &relay.cookie, &relay.session).await;
        let owner_lease = relay
            .direct
            .accept_peer(&owner_grant, &owner_grant.claims.session_id, &fingerprint())
            .unwrap();
        let renew_route = format!(
            "/api/installations/{}/direct/{}/renew",
            grant.claims.installation_id, grant.claims.connection_id
        );
        let foreign_renewal = relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &renew_route)
            .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            foreign_renewal.status(),
            StatusCode::NOT_FOUND,
            "even another session of the same account cannot renew this peer"
        );
        let mut events = relay.direct.subscribe();
        let (method, route) = if revoke_others {
            (
                Method::POST,
                "/api/account/sessions/revoke-others".to_owned(),
            )
        } else {
            (
                Method::DELETE,
                format!("/api/account/sessions/{}", grant.claims.session_id),
            )
        };
        let response = relay
            .app
            .authenticated(&relay.cookie, &relay.session, method, &route)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        tokio::time::timeout(Duration::from_secs(1), lease.closed.cancelled())
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            DirectEvent::Revoked(DirectRevocation::Session { session_id }) => {
                assert_eq!(session_id, grant.claims.session_id)
            }
            _ => panic!("session revocation must send only the targeted public session"),
        }
        assert!(events.try_recv().is_err());
        assert!(!owner_lease.closed.is_cancelled());
        for route in [
            renew_route,
            format!(
                "/api/installations/{}/direct/authorize",
                grant.claims.installation_id
            ),
        ] {
            let response = relay
                .app
                .authenticated(&other_cookie, &other_session, Method::POST, &route)
                .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        assert_eq!(
            relay.get("/chats").send().await.unwrap().status(),
            StatusCode::OK
        );
        relay.close().await;
    }
}

#[tokio::test]
async fn installation_removal_rotation_and_owner_deletion_send_only_installation_revocation() {
    use leo_agent_manager::direct::DirectEvent;
    use leo_relay_protocol::direct::DirectRevocation;
    for action in ["detach", "rotate", "forget", "delete-owner"] {
        let relay = RelayedInstallation::new(axum::Router::new()).await;
        let grant = authorization(&relay, &relay.cookie, &relay.session).await;
        let lease = relay
            .direct
            .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
            .unwrap();
        let mut events = relay.direct.subscribe();
        let id = &grant.claims.installation_id;
        let response = match action {
            "rotate" => {
                let directory = relay
                    .installation
                    .config
                    .data_dir
                    .join("installation-relay");
                leo_agent_manager::relay::rotate_token(&directory)
                    .await
                    .unwrap();
                None
            }
            "detach" => Some(
                relay
                    .app
                    .authenticated(
                        &relay.cookie,
                        &relay.session,
                        Method::POST,
                        &format!("/api/installations/{id}/detach"),
                    )
                    .send()
                    .await
                    .unwrap(),
            ),
            "forget" => Some(
                relay
                    .app
                    .authenticated(
                        &relay.cookie,
                        &relay.session,
                        Method::DELETE,
                        &format!("/api/installations/{id}"),
                    )
                    .send()
                    .await
                    .unwrap(),
            ),
            "delete-owner" => Some(
                relay
                    .app
                    .authenticated(
                        &relay.cookie,
                        &relay.session,
                        Method::POST,
                        "/api/account/delete",
                    )
                    .json(&json!({ "email": "relay-owner@example.test" }))
                    .send()
                    .await
                    .unwrap(),
            ),
            _ => unreachable!(),
        };
        if let Some(response) = response {
            assert_eq!(response.status(), StatusCode::NO_CONTENT, "{action}");
        }
        tokio::time::timeout(Duration::from_secs(1), lease.closed.cancelled())
            .await
            .expect(action);
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event, DirectEvent::Revoked(DirectRevocation::Installation)),
            "{action} must revoke the entire installation"
        );
        assert!(events.try_recv().is_err());
        assert!(!relay.installation.shutdown.is_cancelled());
        assert!(
            relay
                .direct
                .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
                .is_err()
        );
        if action == "detach" {
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
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        relay.close().await;
    }
}

#[tokio::test]
async fn authorization_and_signaling_limits_preserve_the_fallback_relay() {
    use leo_relay_protocol::direct::DirectSignal;
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let mut installation_events = relay.direct.subscribe();
    let route = format!(
        "/api/installations/{}/direct/{}/signal",
        grant.claims.installation_id, grant.claims.connection_id
    );
    let candidate = DirectSignal::Candidate {
        candidate: "candidate:1 1 udp 2122260223 192.0.2.1 50000 typ host generation 0".into(),
        sdp_mid: Some("0".into()),
        sdp_m_line_index: Some(0),
    };
    for _ in 0..120 {
        let response = relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &route)
            .json(&candidate)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        tokio::time::timeout(Duration::from_secs(1), installation_events.recv())
            .await
            .unwrap()
            .unwrap();
    }
    let response = relay
        .app
        .authenticated(&relay.cookie, &relay.session, Method::POST, &route)
        .json(&candidate)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(installation_events.try_recv().is_err());
    let route = format!(
        "/api/installations/{}/direct/authorize",
        grant.claims.installation_id
    );
    for _ in 1..30 {
        let response = relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &route)
            .json(&json!({ "fingerprint": fingerprint(), "versions": [3] }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({ "available": false })
        );
    }
    let response = relay
        .app
        .authenticated(&relay.cookie, &relay.session, Method::POST, &route)
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn older_tunnel_versions_keep_api_and_stream_contracts_without_direct_frames() {
    use leo_relay_protocol::Frame;
    for version in [1, 2, 3] {
        let mut relay = RelayedInstallation::new(axum::Router::new()).await;
        let proxy = filter_tunnel(
            &mut relay,
            Arc::new(move |frame, official| {
                if !official && matches!(&frame, Frame::Hello { .. }) {
                    return Some(Frame::Hello {
                        versions: vec![version],
                    });
                }
                assert!(
                    !matches!(
                        &frame,
                        Frame::DirectKey { .. }
                            | Frame::DirectAuthorize { .. }
                            | Frame::DirectRenew { .. }
                            | Frame::DirectAuthorized { .. }
                            | Frame::DirectSignal { .. }
                            | Frame::DirectRevoke { .. }
                    ),
                    "v{version} must receive no direct frames"
                );
                Some(frame)
            }),
        )
        .await;
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
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({ "available": false })
        );
        assert_eq!(
            relay.get("/chats").send().await.unwrap().status(),
            StatusCode::OK
        );
        let mut stream = relay.get("/chats/stream").send().await.unwrap();
        if version == 1 {
            assert_eq!(stream.status(), StatusCode::NOT_IMPLEMENTED);
        } else {
            assert_eq!(stream.status(), StatusCode::OK);
            assert!(
                tokio::time::timeout(Duration::from_secs(1), stream.chunk())
                    .await
                    .unwrap()
                    .unwrap()
                    .is_some()
            );
        }
        drop(stream);
        relay.close().await;
        proxy.abort();
    }
}

#[tokio::test]
async fn installation_verification_rejects_wire_tampering_and_replayed_nonces() {
    use leo_relay_protocol::{Frame, Role, direct::DirectAuthorization};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let scenario = Arc::new(AtomicUsize::new(0));
    let selected = scenario.clone();
    let original = Mutex::new(None::<DirectAuthorization>);
    let proxy = filter_tunnel(
        &mut relay,
        Arc::new(move |frame, _| {
            if let Frame::DirectAuthorize {
                id,
                mut authorization,
            } = frame
            {
                match selected.load(Ordering::SeqCst) {
                    0 => *original.lock().unwrap() = Some(authorization.clone()),
                    1 => authorization.signature.push('a'),
                    2 => authorization.claims.role = Role::Member,
                    3 => authorization.claims.session_id = "another-session".into(),
                    4 => authorization.claims.fingerprint = fingerprint().replace("AB", "CD"),
                    5 => authorization.claims.generation += 1,
                    6 => authorization.claims.installation_id = "another-installation".into(),
                    7 => authorization = original.lock().unwrap().clone().unwrap(),
                    _ => unreachable!(),
                }
                Some(Frame::DirectAuthorize { id, authorization })
            } else {
                Some(frame)
            }
        }),
    )
    .await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let route = format!(
        "/api/installations/{}/direct/authorize",
        grant.claims.installation_id
    );
    for case in 1..=7 {
        scenario.store(case, Ordering::SeqCst);
        let response = relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &route)
            .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "tampering case {case}"
        );
        assert_eq!(
            relay.get("/chats").send().await.unwrap().status(),
            StatusCode::OK
        );
    }
    relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    relay.close().await;
    proxy.abort();
}

#[tokio::test]
async fn tunnel_loss_denies_new_peers_and_existing_leases_expire_without_renewal() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    sqlx_core::query::query(
        "UPDATE web_sessions SET expires_at = clock_timestamp() + interval '4 seconds'",
    )
    .execute(&relay.app.pool)
    .await
    .unwrap();
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let waiting = authorization(&relay, &relay.cookie, &relay.session).await;
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    let mut events = relay.direct.subscribe();
    relay.stop.cancel();
    tokio::time::timeout(Duration::from_secs(1), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::SERVICE_UNAVAILABLE
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        relay
            .direct
            .accept_peer(&waiting, &waiting.claims.session_id, &fingerprint())
            .is_err()
    );
    assert!(
        !lease.closed.is_cancelled(),
        "a network loss leaves only the finite existing lease"
    );
    tokio::time::timeout(Duration::from_secs(5), lease.closed.cancelled())
        .await
        .unwrap();
    assert!(
        relay
            .direct
            .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
            .is_err()
    );
    assert!(
        events.try_recv().is_err(),
        "no revocation frame can arrive on a lost tunnel"
    );
    // The signed expiry is rounded down to whole seconds; the official session
    // can have a remaining fraction of a second after its last direct lease ends.
    tokio::time::sleep(leo_relay_protocol::direct::until_expiry(
        grant.claims.expires_at + 1,
    ))
    .await;
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!(
                "/api/installations/{}/direct/authorize",
                grant.claims.installation_id
            ),
        )
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(!relay.installation.shutdown.is_cancelled());
    relay.close().await;
}

#[tokio::test]
async fn direct_capacity_is_bounded_without_using_fallback_request_or_stream_slots() {
    use common::{login, stream_accounts};
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let mut cookies = stream_accounts(&relay).await;
    cookies.remove(0);
    let (cookie, session) = login(&relay.app, "capacity-member@example.test").await;
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/sharing/invitations"),
        )
        .json(&json!({ "email": "capacity-member@example.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let invitation: Value = response.json().await.unwrap();
    let response = relay
        .app
        .authenticated(
            &cookie,
            &session,
            Method::POST,
            &format!(
                "/api/account/invitations/{}/accept",
                invitation["id"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    cookies.push(cookie);
    for cookie in &cookies {
        let session: Value = relay
            .app
            .client
            .get(format!("{}/api/account/session", relay.app.url))
            .header("cookie", cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        for _ in 0..8 {
            authorization(&relay, cookie, &session).await;
        }
        let response = relay
            .app
            .authenticated(
                cookie,
                &session,
                Method::POST,
                &format!("/api/installations/{id}/direct/authorize"),
            )
            .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    let (foreign_cookie, foreign_session) =
        login(&relay.app, "additional-member@example.test").await;
    let invitation: Value = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/sharing/invitations"),
        )
        .json(&json!({ "email": "additional-member@example.test" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let response = relay
        .app
        .authenticated(
            &foreign_cookie,
            &foreign_session,
            Method::POST,
            &format!(
                "/api/account/invitations/{}/accept",
                invitation["id"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = relay
        .app
        .authenticated(
            &foreign_cookie,
            &foreign_session,
            Method::POST,
            &format!("/api/installations/{id}/direct/authorize"),
        )
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "member leases leave eight reserved slots for the owner"
    );
    for _ in 0..8 {
        authorization(&relay, &relay.cookie, &relay.session).await;
    }
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let mut stream = relay.get("/chats/stream").send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), stream.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_some()
    );
    drop(stream);
    relay.close().await;
}

#[tokio::test]
async fn offers_answers_and_ice_cross_both_peers_as_metadata_only() {
    use leo_agent_manager::direct::DirectEvent;
    use leo_relay_protocol::direct::DirectSignal;
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let mut installation_events = relay.direct.subscribe();
    let prefix = format!(
        "/api/installations/{}/direct/{}",
        grant.claims.installation_id, grant.claims.connection_id
    );
    let sdp = format!(
        "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=fingerprint:{}\r\na=sctp-port:5000\r\n",
        fingerprint()
    );
    let signal_route = format!("{prefix}/signal");
    for invalid in [
        json!({ "kind": "offer", "sdp": sdp.replace("AB", "CD") }),
        json!({ "kind": "offer", "sdp": format!("{sdp}a=conversation:private content\r\n") }),
        json!({ "kind": "offer", "sdp": format!("{sdp}m=audio 9 RTP/AVP 0\r\n") }),
        json!({ "kind": "candidate", "candidate": "", "body": "private content" }),
        json!({ "kind": "candidate", "candidate": "x".repeat(33_000) }),
    ] {
        let response = relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &signal_route)
            .json(&invalid)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        assert!(installation_events.try_recv().is_err());
    }
    let offer = DirectSignal::Offer { sdp: sdp.clone() };
    let response = relay
        .app
        .authenticated(&relay.cookie, &relay.session, Method::POST, &signal_route)
        .json(&offer)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let event = tokio::time::timeout(Duration::from_secs(1), installation_events.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        DirectEvent::Signal { id, signal } => {
            assert_eq!(id, grant.claims.connection_id);
            assert_eq!(
                serde_json::to_value(signal).unwrap(),
                serde_json::to_value(offer).unwrap()
            );
        }
        _ => panic!("offer must reach only the authorized connection"),
    }
    let mut client_events = relay
        .app
        .client
        .get(format!("{}{prefix}/events", relay.app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(client_events.status(), StatusCode::OK);
    for signal in [
        DirectSignal::Answer {
            sdp: sdp.replace("AB", "CD"),
        },
        DirectSignal::Candidate {
            candidate: "candidate:1 1 udp 2122260223 192.0.2.2 50001 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_m_line_index: Some(0),
        },
        DirectSignal::Candidate {
            candidate: String::new(),
            sdp_mid: None,
            sdp_m_line_index: None,
        },
    ] {
        let expected = serde_json::to_string(&signal).unwrap();
        relay
            .direct
            .send_signal(&grant.claims.connection_id, signal)
            .unwrap();
        let mut received = String::new();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !received.contains("\n\n") {
                received.push_str(&String::from_utf8_lossy(
                    &client_events.chunk().await.unwrap().unwrap(),
                ));
            }
        })
        .await
        .unwrap();
        assert!(received.contains("event: signal"));
        assert!(received.contains(&expected));
    }
    drop(client_events);
    relay.close().await;
}

#[tokio::test]
async fn access_changes_advance_generation_before_a_new_grant_is_accepted() {
    use leo_agent_manager::direct::DirectEvent;
    use leo_relay_protocol::direct::DirectRevocation;
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    let mut events = relay.direct.subscribe();
    // Existing access-change hook, also used for member removal. Any future role
    // mutation must commit its change then use this same per-account revocation.
    relay.app.relay.revoke_access(
        &grant.claims.installation_id,
        Some(&grant.claims.account_id),
    );
    tokio::time::timeout(Duration::from_secs(1), lease.closed.cancelled())
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        DirectEvent::Revoked(DirectRevocation::Account {
            account_id,
            generation,
        }) => {
            assert_eq!(account_id, grant.claims.account_id);
            assert_eq!(generation, 1);
        }
        _ => panic!("access change must use only the account scope"),
    }
    assert!(events.try_recv().is_err());
    let fresh = authorization(&relay, &relay.cookie, &relay.session).await;
    assert_eq!(fresh.claims.generation, 1);
    relay
        .direct
        .accept_peer(&fresh, &fresh.claims.session_id, &fingerprint())
        .unwrap();
    assert!(
        relay
            .direct
            .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
            .is_err()
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn a_lost_renewal_acknowledgement_does_not_hide_the_peer_from_logout() {
    use leo_agent_manager::direct::DirectEvent;
    use leo_relay_protocol::Frame;
    use leo_relay_protocol::direct::DirectRevocation;
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let lose_ack = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let losing = lose_ack.clone();
    let proxy = filter_tunnel(
        &mut relay,
        Arc::new(move |frame, official| {
            if !official
                && losing.load(std::sync::atomic::Ordering::SeqCst)
                && matches!(&frame, Frame::DirectAuthorized { .. })
            {
                None
            } else {
                Some(frame)
            }
        }),
    )
    .await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    lose_ack.store(true, std::sync::atomic::Ordering::SeqCst);
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!(
                "/api/installations/{}/direct/{}/renew",
                grant.claims.installation_id, grant.claims.connection_id
            ),
        )
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!lease.closed.is_cancelled());
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
    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        DirectEvent::Revoked(DirectRevocation::Session { session_id }) => {
            assert_eq!(session_id, grant.claims.session_id)
        }
        _ => panic!("lost renewal acknowledgement must preserve session-scoped revocation"),
    }
    assert!(events.try_recv().is_err());
    relay.close().await;
    proxy.abort();
}

#[tokio::test]
async fn installation_signaling_has_a_separate_bounded_budget() {
    use leo_relay_protocol::direct::DirectSignal;
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let candidate = || DirectSignal::Candidate {
        candidate: "candidate:1 1 udp 2122260223 192.0.2.2 50001 typ host".into(),
        sdp_mid: Some("0".into()),
        sdp_m_line_index: Some(0),
    };
    let mut events = relay
        .app
        .client
        .get(format!(
            "{}/api/installations/{}/direct/{}/events",
            relay.app.url, grant.claims.installation_id, grant.claims.connection_id
        ))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    for _ in 0..120 {
        relay
            .direct
            .send_signal(&grant.claims.connection_id, candidate())
            .unwrap();
        let chunk = tokio::time::timeout(Duration::from_secs(1), events.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&chunk).contains("candidate:"));
    }
    assert!(
        relay
            .direct
            .send_signal(&grant.claims.connection_id, candidate())
            .is_err()
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    drop(events);
    relay.close().await;
}

#[tokio::test]
async fn tunnel_loss_without_reconnection_keeps_established_peers_until_grant_expiry() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    sqlx_core::query::query(
        "UPDATE web_sessions SET expires_at = clock_timestamp() + interval '4 seconds'",
    )
    .execute(&relay.app.pool)
    .await
    .unwrap();
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let waiting = authorization(&relay, &relay.cookie, &relay.session).await;
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    let mut events = relay.direct.subscribe();

    relay.app.relay.shutdown();
    relay.app.server.abort();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !lease.closed.is_cancelled(),
        "shutdown is tunnel loss, not access revocation"
    );
    assert!(
        events.try_recv().is_err(),
        "shutdown must send no revocation frame"
    );
    assert!(
        relay
            .direct
            .accept_peer(&waiting, &waiting.claims.session_id, &fingerprint())
            .is_err()
    );
    let before_expiry = leo_relay_protocol::direct::until_expiry(grant.claims.expires_at)
        .saturating_sub(Duration::from_millis(100));
    assert!(
        tokio::time::timeout(before_expiry, lease.closed.cancelled())
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(1), lease.closed.cancelled())
        .await
        .unwrap();
    relay.close().await;
}

#[tokio::test]
async fn official_restart_closes_old_leases_and_new_grants_remain_revocable() {
    use leo_agent_manager::direct::DirectEvent;
    use leo_relay_protocol::direct::DirectRevocation;

    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let waiting = authorization(&relay, &relay.cookie, &relay.session).await;
    let lease = relay
        .direct
        .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
        .unwrap();
    let mut events = relay.direct.subscribe();

    relay.app.relay.shutdown();
    relay.app.server.abort();
    assert!((&mut relay.app.server).await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!lease.closed.is_cancelled());
    assert!(events.try_recv().is_err());
    assert!(
        relay
            .direct
            .accept_peer(&waiting, &waiting.claims.session_id, &fingerprint())
            .is_err()
    );

    // Keep the persisted accounts, sessions and machine credential, but recreate
    // the official process's relay state at the same origin. The real connector
    // must reconnect without restarting the installation or replacing its leases.
    let port = reqwest::Url::parse(&relay.app.url).unwrap().port().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    relay.app.relay = leo_official_service::Relay::default();
    let app = leo_official_service::router_with_relay(
        relay.app.pool.clone(),
        relay.app.mail.clone(),
        relay.app.url.clone(),
        leo_official_service::OAuthProviders::default(),
        relay.app.relay.clone(),
    )
    .await
    .unwrap();
    relay.app.server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    tokio::time::timeout(Duration::from_secs(5), lease.closed.cancelled())
        .await
        .expect("the new signing key must close peers authorized by the old process");
    assert!(grant.claims.expires_at > leo_relay_protocol::direct::unix_time());
    assert!(events.try_recv().is_err(), "key replacement is not logout");
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::OK {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the fallback relay must recover after the official restart");
    assert!(
        relay
            .direct
            .accept_peer(&waiting, &waiting.claims.session_id, &fingerprint())
            .is_err(),
        "the old pending grant must not survive key replacement"
    );

    let renewed = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!(
                "/api/installations/{}/direct/{}/renew",
                grant.claims.installation_id, grant.claims.connection_id
            ),
        )
        .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
        .send()
        .await
        .unwrap();
    assert_eq!(renewed.status(), StatusCode::NOT_FOUND);

    let fresh = authorization(&relay, &relay.cookie, &relay.session).await;
    assert_eq!(fresh.claims.session_id, grant.claims.session_id);
    let fresh_lease = relay
        .direct
        .accept_peer(&fresh, &fresh.claims.session_id, &fingerprint())
        .expect("a fresh grant must be verified under the new tunnel key");
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
    tokio::time::timeout(Duration::from_secs(1), fresh_lease.closed.cancelled())
        .await
        .expect("logout after restart must still revoke the new peer immediately");
    match tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap()
    {
        DirectEvent::Revoked(DirectRevocation::Session { session_id }) => {
            assert_eq!(session_id, fresh.claims.session_id);
        }
        _ => panic!("logout must revoke only its public session identifier"),
    }
    assert!(events.try_recv().is_err());
    assert!(!relay.installation.shutdown.is_cancelled());
    relay.close().await;
}

#[tokio::test]
async fn installation_accepts_bounded_signing_clock_skew_but_rejects_long_lived_grants() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use leo_relay_protocol::{Frame, direct::signing_bytes};
    use ring::signature::KeyPair;
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    };

    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let key = Arc::new(ring::signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap());
    let offset = Arc::new(AtomicU64::new(1));
    let signed = Arc::new(Mutex::new(None));
    let proxy = filter_tunnel(&mut relay, {
        let key = key.clone();
        let offset = offset.clone();
        let signed = signed.clone();
        Arc::new(move |frame, official| {
            if !official {
                return Some(frame);
            }
            match frame {
                Frame::DirectKey { .. } => Some(Frame::DirectKey {
                    public_key: URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
                    stun_url: None,
                }),
                Frame::DirectAuthorize {
                    id,
                    mut authorization,
                } => {
                    // A signer whose clock is ahead, with the same three-minute TTL.
                    authorization.claims.expires_at += offset.load(Ordering::SeqCst);
                    authorization.signature = URL_SAFE_NO_PAD
                        .encode(key.sign(&signing_bytes(&authorization.claims)).as_ref());
                    *signed.lock().unwrap() = Some(authorization.clone());
                    Some(Frame::DirectAuthorize { id, authorization })
                }
                _ => Some(frame),
            }
        })
    })
    .await;
    let route = format!(
        "/api/installations/{}/direct/authorize",
        relay.session["installations"][0]["id"].as_str().unwrap()
    );
    for skew in [1, 30, 90] {
        offset.store(skew, Ordering::SeqCst);
        let response = relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &route)
            .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }))
            .send()
            .await
            .unwrap();
        if skew <= 30 {
            assert_eq!(response.status(), StatusCode::OK, "clock skew {skew}");
            let grant = signed.lock().unwrap().clone().unwrap();
            relay
                .direct
                .accept_peer(&grant, &grant.claims.session_id, &fingerprint())
                .unwrap();
        } else {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    relay.close().await;
    proxy.abort();
}

#[tokio::test]
async fn one_members_signaling_budget_does_not_block_the_owner_and_refusals_are_visible() {
    use common::stream_accounts;
    use leo_relay_protocol::direct::DirectSignal;
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let cookies = stream_accounts(&relay).await;
    let member_cookie = &cookies[1];
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
    let member = authorization(&relay, member_cookie, &member_session).await;
    let owner = authorization(&relay, &relay.cookie, &relay.session).await;
    let candidate = || DirectSignal::Candidate {
        candidate: "candidate:1 1 udp 2122260223 192.0.2.2 50001 typ host".into(),
        sdp_mid: Some("0".into()),
        sdp_m_line_index: Some(0),
    };
    let prefix = format!(
        "/api/installations/{}/direct",
        member.claims.installation_id
    );
    let mut member_events = relay
        .app
        .client
        .get(format!(
            "{}{prefix}/{}/events",
            relay.app.url, member.claims.connection_id
        ))
        .header("cookie", member_cookie)
        .send()
        .await
        .unwrap();
    for _ in 0..120 {
        relay
            .direct
            .send_signal(&member.claims.connection_id, candidate())
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), member_events.chunk())
                .await
                .unwrap()
                .unwrap()
                .is_some()
        );
    }
    let mut installation_events = relay.direct.subscribe();
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("{prefix}/{}/signal", owner.claims.connection_id),
        )
        .json(&candidate())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let event = tokio::time::timeout(Duration::from_secs(1), installation_events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, leo_agent_manager::direct::DirectEvent::Signal { id, .. } if id == owner.claims.connection_id)
    );

    let response = relay
        .app
        .authenticated(
            member_cookie,
            &member_session,
            Method::POST,
            &format!("{prefix}/{}/signal", member.claims.connection_id),
        )
        .json(&candidate())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "an installation refusal cannot be reported as delivered"
    );
    assert!(installation_events.try_recv().is_err());
    drop(member_events);
    relay.close().await;
}

#[tokio::test]
async fn saturated_direct_signaling_preserves_credited_fallback_streams() {
    use axum::{
        body::{Body, Bytes},
        routing::get,
    };
    let routes = axum::Router::new().route(
        "/api/fixture/burst/stream",
        get(|| async {
            let bytes = Bytes::from(vec![b'x'; leo_relay_protocol::MAX_STREAM_CHUNK]);
            let stream = futures_util::stream::unfold(bytes, |bytes| async {
                Some((Ok::<_, std::io::Error>(bytes.clone()), bytes))
            });
            (
                [("content-type", "text/event-stream")],
                Body::from_stream(stream),
            )
        }),
    );
    let mut relay = RelayedInstallation::with_small_socket_buffer(routes).await;
    let (pause, paused) = tokio::sync::watch::channel(false);
    let proxy =
        filter_tunnel_with_pause(&mut relay, Arc::new(|frame, _| Some(frame)), paused).await;
    let mut grants = Vec::new();
    for _ in 0..8 {
        grants.push(authorization(&relay, &relay.cookie, &relay.session).await);
    }
    let mut stream = relay.get("/fixture/burst/stream").send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert!(stream.chunk().await.unwrap().is_some());
    pause.send_replace(true);

    let sdp = format!(
        "v=0\r\ns=-\r\nt=0 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=fingerprint:{}\r\n{}",
        fingerprint(),
        "a=candidate:1 1 udp 2122260223 192.0.2.1 50000 typ host\r\n".repeat(270)
    );
    assert!(sdp.len() < leo_relay_protocol::direct::MAX_SIGNAL);
    let prefix = format!(
        "/api/installations/{}/direct",
        grants[0].claims.installation_id
    );
    let mut signals = tokio::task::JoinSet::new();
    for index in 0..32 {
        let request = relay
            .app
            .authenticated(
                &relay.cookie,
                &relay.session,
                Method::POST,
                &format!("{prefix}/{}/signal", grants[index % 8].claims.connection_id),
            )
            .json(&json!({ "kind": "offer", "sdp": sdp }));
        signals.spawn(async move { request.send().await.unwrap().status() });
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    let mut renewals = tokio::task::JoinSet::new();
    for grant in &grants {
        let request = relay
            .app
            .authenticated(
                &relay.cookie,
                &relay.session,
                Method::POST,
                &format!("{prefix}/{}/renew", grant.claims.connection_id),
            )
            .json(&json!({ "fingerprint": fingerprint(), "versions": [4] }));
        renewals.spawn(async move { request.send().await.unwrap().status() });
    }
    let busy = tokio::time::timeout(Duration::from_secs(2), renewals.join_next()).await;
    // Read while the signaling socket is stalled: HTTP body polling grants stream
    // credit independently, and must never fail because the direct queue is full.
    let _ = tokio::time::timeout(Duration::from_millis(300), stream.chunk()).await;
    pause.send_replace(false);
    assert_eq!(
        busy.unwrap().unwrap().unwrap(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a saturated direct queue must refuse control work without borrowing credit capacity"
    );
    while signals.join_next().await.is_some() {}
    while renewals.join_next().await.is_some() {}
    let mut received = 0;
    tokio::time::timeout(Duration::from_secs(5), async {
        while received < 8 * leo_relay_protocol::MAX_STREAM_CHUNK {
            let chunk = stream
                .chunk()
                .await
                .unwrap()
                .expect("fallback stream was cut by signaling");
            received += chunk.len();
        }
    })
    .await
    .expect("credited fallback stream must keep making progress");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    drop(stream);
    relay.close().await;
    proxy.abort();
}

#[tokio::test]
async fn candidate_policy_drops_local_special_addresses_but_preserves_lan_and_vpn() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let grant = authorization(&relay, &relay.cookie, &relay.session).await;
    let path = format!(
        "/api/installations/{}/direct/{}/signal",
        grant.claims.installation_id, grant.claims.connection_id
    );
    let mut signals = relay.direct.subscribe();
    for address in [
        "127.0.0.1",
        "169.254.1.1",
        "::1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.1.1",
        "fe80::1",
        "0.0.0.0",
        "::",
        "224.0.0.251",
        "ff02::fb",
        "255.255.255.255",
        "hidden-host.local",
    ] {
        let candidate = format!("candidate:1 1 udp 2122260223 {address} 50000 typ host");
        let response = relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &path)
            .json(&json!({
                "kind": "candidate",
                "candidate": candidate,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "ignored candidate {address}"
        );
        assert!(
            signals.try_recv().is_err(),
            "ignored candidates must never reach ICE"
        );
    }
    for address in [
        "192.168.1.2",
        "10.8.0.2",
        "172.16.2.3",
        "fd00::2",
        "192.0.2.1",
    ] {
        let candidate = format!("candidate:1 1 udp 2122260223 {address} 50000 typ host");
        let response = relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &path)
            .json(&json!({
                "kind": "candidate",
                "candidate": candidate,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            matches!(signals.try_recv().unwrap(), leo_agent_manager::direct::DirectEvent::Signal { signal: leo_relay_protocol::direct::DirectSignal::Candidate { candidate: received, .. }, .. } if received == candidate)
        );
    }
    let base = format!(
        "v=0\r\ns=-\r\nt=0 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=fingerprint:{}\r\na=sctp-port:5000\r\n",
        grant.claims.fingerprint,
    );
    let usable = "a=candidate:1 1 udp 2122260223 192.168.1.2 50000 typ host\r\n";
    let special = "a=candidate:2 1 udp 2122260223 fe80::1 50001 typ host\r\n";
    let mdns = "a=candidate:3 1 udp 2122260223 hidden-host.local 50002 typ host\r\n";
    let response = relay
        .app
        .authenticated(&relay.cookie, &relay.session, Method::POST, &path)
        .json(&json!({
            "kind": "offer",
            "sdp": format!("{base}{usable}{special}{mdns}"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        matches!(signals.try_recv().unwrap(), leo_agent_manager::direct::DirectEvent::Signal {
        signal: leo_relay_protocol::direct::DirectSignal::Offer { sdp }, ..
    } if sdp == format!("{base}{usable}"))
    );
    for candidates in [special, mdns, "a=candidate:invalid syntax\r\n"] {
        let response = relay
            .app
            .authenticated(&relay.cookie, &relay.session, Method::POST, &path)
            .json(&json!({
                "kind": "offer",
                "sdp": format!("{base}{candidates}"),
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(signals.try_recv().is_err());
    }
    relay.close().await;
}
