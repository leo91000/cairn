mod common;

use axum::{Json, Router, body::Bytes};
use cairn_installation::{plugins::oauth, service::Service};
use common::{RelayedInstallation, login};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

fn post(relay: &RelayedInstallation, value: &Value) -> reqwest::RequestBuilder {
    relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/plugins/oauth/callback",
        )
        .json(value)
}

async fn start(service: &Service, binding: &str) -> Value {
    oauth::start(
        service,
        "gmail",
        &json!({
            "settings": { "clientId": "fixture-client" },
            "secrets": { "clientSecret": "fixture-secret" },
        }),
        binding,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn one_beacon_callback_routes_two_installations_and_rejects_altered_state() {
    let relay = RelayedInstallation::new(Router::new()).await;
    let root = tempfile::tempdir().unwrap();
    let mut config = relay.installation.config.clone();
    config.data_dir = root.path().join("data");
    config.home = root.path().join("home");
    config.workspace_roots = vec![root.path().to_owned()];
    std::fs::create_dir_all(config.home.join(".codex")).unwrap();
    std::fs::write(config.home.join(".codex/cairn-managed-auth"), "1").unwrap();
    let second = Service::new(config).await.unwrap();
    let claim: Value = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            "/api/installations/claim-code",
        )
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let identity = second.config.data_dir.join("installation-relay");
    cairn_installation::relay::claim(
        &relay.beacon_origin,
        &identity,
        claim["code"].as_str().unwrap(),
        "Second installation",
    )
    .await
    .unwrap();
    let second_id = cairn_installation::relay::beacon_address(&second.config.data_dir)
        .await
        .unwrap()
        .unwrap()
        .1;
    let stop = tokio_util::sync::CancellationToken::new();
    let connector = tokio::spawn(cairn_installation::relay::connect_with_direct(
        identity,
        cairn_installation::http::router(second.clone())
            .await
            .unwrap(),
        second.clone(),
        stop.clone(),
        Default::default(),
    ));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = relay
                .app
                .client
                .get(format!(
                    "{}/api/installations/{second_id}/api/chats",
                    relay.app.url,
                ))
                .header("cookie", &relay.cookie)
                .send()
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let exchanged = Arc::new(Mutex::new(Vec::<HashMap<String, String>>::new()));
    let calls = exchanged.clone();
    let google = Router::new().route(
        "/token",
        axum::routing::post(move |bytes: Bytes| {
            let calls = calls.clone();
            async move {
                calls
                    .lock()
                    .unwrap()
                    .push(url::form_urlencoded::parse(&bytes).into_owned().collect());
                Json(json!({
                    "access_token": "synthetic-access", "refresh_token": "synthetic-refresh",
                    "token_type": "Bearer", "expires_in": 3600,
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let google_origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, google).await.unwrap() });
    relay
        .installation
        .plugins
        .route_http_to(Some(google_origin.clone()));
    second.plugins.route_http_to(Some(google_origin));

    let binding = format!(
        "cairn-account:{}",
        relay.session["account"]["id"].as_str().unwrap()
    );
    let first_flow = start(&relay.installation, &binding).await;
    let second_flow = start(&second, &binding).await;
    let expected = format!("{}/plugins/oauth/callback", relay.beacon_origin);
    for (service, flow) in [(&relay.installation, &first_flow), (&second, &second_flow)] {
        assert_eq!(oauth::callback_url(service).await.unwrap(), expected);
        let url = url::Url::parse(flow["verificationUri"].as_str().unwrap()).unwrap();
        let parameters: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(parameters["redirect_uri"], expected);
        assert_eq!(parameters["state"], flow["id"]);
        // Android URL-encodes path segments before polling and cancellation.
        // Exercise the actual generated ID through the encoded HTTP relay path.
        let encoded: String =
            url::form_urlencoded::byte_serialize(flow["id"].as_str().unwrap().as_bytes()).collect();
        assert_eq!(encoded, flow["id"].as_str().unwrap());
        let (_, id) = cairn_installation::relay::beacon_address(&service.config.data_dir)
            .await
            .unwrap()
            .unwrap();
        let response = relay
            .app
            .authenticated(
                &relay.cookie,
                &relay.session,
                Method::GET,
                &format!("/api/installations/{id}/api/plugins/gmail/oauth/{encoded}"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["state"], "pending");
    }

    let first_id = relay.session["installations"][0]["id"].as_str().unwrap();
    let altered = second_flow["id"]
        .as_str()
        .unwrap()
        .replace(&second_id, first_id);
    assert_eq!(
        post(
            &relay,
            &json!({ "state": altered, "code": "wrong-destination" })
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        second
            .plugins
            .oauth_flows
            .status("gmail", second_flow["id"].as_str().unwrap(), &binding)
            .unwrap()["state"],
        "pending"
    );
    assert!(exchanged.lock().unwrap().is_empty());

    for (service, flow) in [(&second, &second_flow), (&relay.installation, &first_flow)] {
        let response = post(&relay, &json!({ "state": flow["id"], "code": "verified" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap()["flowId"],
            flow["id"]
        );
        assert_eq!(
            service
                .plugins
                .oauth_flows
                .status("gmail", flow["id"].as_str().unwrap(), &binding)
                .unwrap()["state"],
            "completed"
        );
        assert_eq!(
            post(&relay, &json!({ "state": flow["id"], "code": "replay" }))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let calls = exchanged.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|call| call["redirect_uri"] == expected && call["code_verifier"].len() == 43)
    );

    let cancelled = start(&second, &binding).await;
    let encoded: String =
        url::form_urlencoded::byte_serialize(cancelled["id"].as_str().unwrap().as_bytes())
            .collect();
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::DELETE,
            &format!("/api/installations/{second_id}/api/plugins/gmail/oauth/{encoded}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap()["cancelled"], true);

    stop.cancel();
    connector.await.unwrap().unwrap();
    second.shutdown.cancel();
    second.avatars.close().await;
    server.abort();
    relay.close().await;
}

#[tokio::test]
async fn central_callback_requires_current_owner_session_csrf_and_valid_routing_state() {
    let relay = RelayedInstallation::new(Router::new()).await;
    let binding = format!(
        "cairn-account:{}",
        relay.session["account"]["id"].as_str().unwrap()
    );
    let flow = start(&relay.installation, &binding).await;
    let parameters = json!({ "state": flow["id"], "error": "access_denied" });
    let endpoint = format!("{}/api/plugins/oauth/callback", relay.app.url);
    assert_eq!(
        relay
            .app
            .client
            .post(&endpoint)
            .header("origin", &relay.app.url)
            .json(&parameters)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        relay
            .app
            .client
            .post(&endpoint)
            .header("origin", &relay.app.url)
            .header("cookie", &relay.cookie)
            .json(&parameters)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let (outsider_cookie, outsider_session) = login(&relay.app, "outsider@example.test").await;
    assert_eq!(
        relay
            .app
            .authenticated(
                &outsider_cookie,
                &outsider_session,
                Method::POST,
                "/api/plugins/oauth/callback"
            )
            .json(&parameters)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    // A member has installation access but cannot complete owner-only OAuth.
    let cookies = common::stream_accounts_with_members(&relay, 1).await;
    let member_session: Value = relay
        .app
        .client
        .get(format!("{}/api/account/session", relay.app.url))
        .header("cookie", &cookies[1])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        relay
            .app
            .authenticated(
                &cookies[1],
                &member_session,
                Method::POST,
                "/api/plugins/oauth/callback"
            )
            .json(&parameters)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    for state in [
        "",
        "plugin:../outside:abc",
        "plugin:https://outside.example:abc",
        "plugin:00000000-0000-4000-8000-000000000001:short",
        "arbitrary-state",
    ] {
        assert_eq!(
            post(&relay, &json!({ "state": state, "code": "ignored" }))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        post(
            &relay,
            &json!({ "state": flow["id"], "code": "x".repeat(20_000) })
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        relay
            .installation
            .plugins
            .oauth_flows
            .status("gmail", flow["id"].as_str().unwrap(), &binding)
            .unwrap()["state"],
        "pending"
    );
    assert_eq!(
        post(&relay, &parameters).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        relay
            .installation
            .plugins
            .oauth_flows
            .status("gmail", flow["id"].as_str().unwrap(), &binding)
            .unwrap()["state"],
        "failed"
    );
    relay.close().await;
}
