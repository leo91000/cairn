mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};

async fn seeded_artifact(
    relay: &RelayedInstallation,
    bytes: &[u8],
    media_type: &str,
) -> (String, String) {
    let installation = &relay.installation;
    let task = installation
        .task(
            json!({
                "name": "Report",
                "prompt": "Make a report",
                "agentId": cairn_installation::config::MAIN_AGENT_ID,
            }),
            None,
        )
        .await
        .unwrap();
    let run = installation
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let run = run["id"].as_str().unwrap().to_owned();
    let artifact = uuid::Uuid::new_v4().to_string();
    let directory = installation.config.data_dir.join("artifacts");
    tokio::fs::create_dir_all(&directory).await.unwrap();
    tokio::fs::write(directory.join(&artifact), bytes)
        .await
        .unwrap();
    installation
        .store
        .set(
            &format!("artifact:{run}:{artifact}"),
            json!({
                "id": artifact,
                "runId": run,
                "name": "report.html",
                "title": "Report",
                "mediaType": media_type,
                "visibility": "private",
                "url": format!("/api/runs/{run}/artifacts/{artifact}"),
            }),
            None,
        )
        .await
        .unwrap();
    (run, artifact)
}

async fn visibility(relay: &RelayedInstallation, run: &str, artifact: &str, value: &str) -> Value {
    let response = relay
        .app
        .client
        .put(format!(
            "{}/runs/{run}/artifacts/{artifact}/visibility",
            relay.base
        ))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&json!({ "visibility": value }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

#[tokio::test]
async fn public_file_is_an_official_read_only_link_with_security_headers_revocation_and_offline_message()
 {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) = seeded_artifact(&relay, b"<h1>Report</h1>", "text/html").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let public = shared["publicUrl"].as_str().unwrap();
    assert!(
        public.starts_with(&format!(
            "{}/api/public/installations/{id}/artifacts/",
            relay.app.url
        )),
        "The share URL must use the official service"
    );
    for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
        let response = relay
            .app
            .client
            .request(method.clone(), public)
            .header("origin", "https://recipient.example")
            .header("range", "bytes=4-9")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        for (name, value) in [
            ("access-control-allow-origin", "*"),
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("content-security-policy", "default-src 'none'; sandbox"),
            ("referrer-policy", "no-referrer"),
            ("x-robots-tag", "noindex, nofollow"),
        ] {
            assert_eq!(response.headers()[name], value);
        }
        assert!(!response.headers().contains_key("set-cookie"));
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-credentials")
        );
        assert_eq!(response.headers()["content-range"], "bytes 4-9/15");
        assert_eq!(response.headers()["content-length"], "6");
        assert_eq!(response.headers()["content-disposition"], "attachment");
        assert_eq!(
            response.text().await.unwrap(),
            if method == reqwest::Method::HEAD {
                ""
            } else {
                "Report"
            }
        );
    }

    assert_eq!(
        relay
            .app
            .client
            .get(format!("{}/runs/{run}/artifacts", relay.base))
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
            .post(public)
            .body("changed")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );

    visibility(&relay, &run, &artifact, "private").await;
    assert_eq!(
        relay.app.client.get(public).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );

    let next = visibility(&relay, &run, &artifact, "public").await;
    assert_ne!(next["publicUrl"], shared["publicUrl"]);

    relay.stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let installations: Value = relay
                .app
                .client
                .get(format!("{}/api/installations", relay.app.url))
                .header("cookie", &relay.cookie)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if installations[0]["online"] == false {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let response = relay
        .app
        .client
        .get(next["publicUrl"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    assert!(!response.headers().contains_key("content-disposition"));
    assert_eq!(
        response.headers()["content-security-policy"],
        "default-src 'none'; sandbox"
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("set-cookie"));

    let body = response.text().await.unwrap();
    assert!(
        body.contains("<h1>Installation offline</h1>"),
        "A recipient must understand why the file is unavailable"
    );
    assert!(!body.contains("relay-owner") && !body.contains("accountId"));
    relay.close().await;
}

#[tokio::test]
async fn public_files_larger_than_a_relay_frame_stream_without_truncation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let bytes = vec![b'x'; cairn_protocol::MAX_BODY + 100_000];
    let (run, artifact) = seeded_artifact(&relay, &bytes, "application/octet-stream").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let response = relay
        .app
        .client
        .get(shared["publicUrl"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), bytes.as_slice());
    relay.close().await;
}

#[tokio::test]
async fn public_svg_is_downloaded_as_an_attachment() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) = seeded_artifact(&relay, b"<svg></svg>", "image/svg+xml").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let response = relay
        .app
        .client
        .get(shared["publicUrl"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-disposition"], "attachment");
    assert_eq!(response.headers()["content-length"], "11");
    assert_eq!(response.text().await.unwrap(), "<svg></svg>");
    relay.close().await;
}

#[tokio::test]
async fn stalled_public_downloads_cannot_exhaust_live_streams_and_expire_without_another_read() {
    use std::time::Duration;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) =
        seeded_artifact(&relay, &vec![b'x'; 12_000_000], "application/octet-stream").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let public = shared["publicUrl"].as_str().unwrap();
    let mut downloads = Vec::new();
    for _ in 0..4 {
        let response = relay.app.client.get(public).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        downloads.push(response);
    }

    assert_eq!(
        relay.app.client.get(public).send().await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "Public downloads must have their own small capacity pool"
    );

    let cookies = common::stream_accounts(&relay).await;
    let mut live = Vec::new();
    for index in 0..24 {
        let response = relay
            .app
            .client
            .get(format!("{}/chats/stream", relay.base))
            .header("cookie", &cookies[index / 8])
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "All live-stream slots must remain available"
        );
        live.push(response);
    }

    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    // Keep the stalled responses alive and never poll their bodies. Expiry must
    // be driven by the tunnel, not by a downstream HTTP body read.
    tokio::time::sleep(Duration::from_secs(30)).await;

    let replacement = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let response = relay.app.client.get(public).send().await.unwrap();
            if response.status() == StatusCode::OK {
                break response;
            }
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    })
    .await
    .expect("Idle public downloads must release their slots");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    drop(replacement);
    drop(downloads);
    drop(live);
    relay.close().await;
}

#[tokio::test]
async fn public_download_rate_limit_uses_the_peer_and_preserves_account_access() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) = seeded_artifact(&relay, b"Report", "text/plain").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let public = shared["publicUrl"].as_str().unwrap();
    for _ in 0..30 {
        let response = relay.app.client.get(public).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "Report");
    }
    let limited = relay
        .app
        .client
        .get(public)
        .header("x-forwarded-for", "198.51.100.1")
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        limited.headers()["content-security-policy"],
        "default-src 'none'; sandbox"
    );
    assert!(!limited.headers().contains_key("set-cookie"));
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn only_owner_controls_public_links_and_member_removal_preserves_them() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let cookies = common::stream_accounts(&relay).await;
    let cookie = &cookies[1];
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
    let (run, artifact) = seeded_artifact(&relay, b"Report", "text/plain").await;
    let path = format!("{}/runs/{run}/artifacts/{artifact}/visibility", relay.base);
    for value in ["public", "private"] {
        let response = relay
            .app
            .client
            .put(&path)
            .header("cookie", cookie)
            .header("origin", &relay.app.url)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({ "visibility": value }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let public = shared["publicUrl"].as_str().unwrap();
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    let removed = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            reqwest::Method::DELETE,
            &format!(
                "/api/installations/{installation}/sharing/members/{}",
                session["account"]["id"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        relay
            .app
            .client
            .get(public)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "Report"
    );
    visibility(&relay, &run, &artifact, "private").await;
    assert_eq!(
        relay.app.client.get(public).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    relay.close().await;
}
