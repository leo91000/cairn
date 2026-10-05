mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};

async fn seeded_artifact(relay: &RelayedInstallation, bytes: &[u8]) -> (String, String) {
    let installation = &relay.installation;
    let task = installation.task(json!({ "name": "Report", "prompt": "Make a report", "agentId": leo_agent_manager::config::MAIN_AGENT_ID }), None).await.unwrap();
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
                "mediaType": "text/html",
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
    let (run, artifact) = seeded_artifact(&relay, b"<h1>Report</h1>").await;
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
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("content-security-policy", "default-src 'none'; sandbox"),
            ("referrer-policy", "no-referrer"),
            ("x-robots-tag", "noindex, nofollow"),
        ] {
            assert_eq!(response.headers()[name], value);
        }
        assert!(!response.headers().contains_key("set-cookie"));
        assert_eq!(response.headers()["content-range"], "bytes 4-9/15");
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
            .put(public)
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
    let body = response.text().await.unwrap();
    assert!(
        body.contains("offline"),
        "A recipient must understand why the file is unavailable"
    );
    assert!(!body.contains("relay-owner") && !body.contains("accountId"));
    relay.close().await;
}
