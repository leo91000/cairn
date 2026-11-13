mod common;

use common::{RelayedInstallation, seeded_artifact};
use reqwest::StatusCode;
use serde_json::{Value, json};

async fn change(
    relay: &RelayedInstallation,
    chat: &str,
    method: reqwest::Method,
    body: Value,
) -> reqwest::Response {
    relay
        .app
        .client
        .request(method, format!("{}/chats/{chat}/share", relay.base))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn fixture() -> (RelayedInstallation, String, String, String) {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) = seeded_artifact(&relay, b"private report bytes", "text/plain").await;
    let mut chat = relay.installation.chat_create(json!({})).await.unwrap();
    chat["runId"] = run.clone().into();
    chat["title"] = "<script>alert('title')</script>".into();
    relay
        .installation
        .store
        .save_conversation_boundary(chat.clone())
        .await
        .unwrap();
    let chat_id = chat["id"].as_str().unwrap().to_owned();
    relay
        .installation
        .store
        .event(
            &run,
            &cairn_contracts::EventKind::ChatUser,
            "<img src=x onerror=alert(1)>",
            None,
        )
        .await
        .unwrap();
    (relay, chat_id, run, artifact)
}

#[tokio::test]
async fn unlisted_conversation_links_expire_revoke_and_include_private_files() {
    let (relay, chat, run, artifact) = fixture().await;
    let started = cairn_installation::config::now();
    let response = change(&relay, &chat, reqwest::Method::PUT, json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let share: Value = response.json().await.unwrap();
    let expires = share["expiresAt"].as_i64().unwrap();
    assert!(
        (started + 86_400_000..=cairn_installation::config::now() + 86_400_000).contains(&expires)
    );
    let url = share["publicUrl"].as_str().unwrap();
    let file_url = format!("{url}/artifacts/{artifact}");
    let response = relay.app.client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["x-robots-tag"]
            .to_str()
            .unwrap()
            .contains("noindex")
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert!(!response.headers().contains_key("content-disposition"));
    assert!(!response.headers().contains_key("set-cookie"));
    let html = response.text().await.unwrap();
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains(&file_url));
    let response = relay.app.client.get(&file_url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap(), "private report bytes");
    assert_eq!(
        relay
            .installation
            .store
            .artifact(&run, &artifact)
            .await
            .unwrap()
            .unwrap()
            .visibility
            .as_deref(),
        Some("private")
    );
    let response = relay
        .app
        .client
        .get(&file_url)
        .header("range", "bytes=0-6")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.bytes().await.unwrap(), "private");
    assert_eq!(
        relay
            .app
            .client
            .head(&file_url)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let (_, other) = seeded_artifact(&relay, b"not shared", "text/plain").await;
    assert_eq!(
        relay
            .app
            .client
            .get(format!("{url}/artifacts/{other}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let replacement = change(
        &relay,
        &chat,
        reqwest::Method::PUT,
        json!({"expiresInHours": 1}),
    )
    .await;
    assert_eq!(replacement.status(), StatusCode::OK);
    let replacement: Value = replacement.json().await.unwrap();
    assert_eq!(
        relay.app.client.get(url).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        relay
            .app
            .client
            .get(&file_url)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let url = replacement["publicUrl"].as_str().unwrap();
    assert_eq!(
        change(&relay, &chat, reqwest::Method::DELETE, json!({}))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        relay.app.client.get(url).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let response = change(
        &relay,
        &chat,
        reqwest::Method::PUT,
        json!({"expiresInHours": 720}),
    )
    .await;
    let share: Value = response.json().await.unwrap();
    let token = share["token"].as_str().unwrap().to_owned();
    relay
        .installation
        .store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE kv SET expires=0 WHERE key=?",
                [format!("conversation-link:{token}")],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let expired = share["publicUrl"].as_str().unwrap();
    assert_eq!(
        relay.app.client.get(expired).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        relay
            .app
            .client
            .get(format!("{expired}/artifacts/{artifact}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        change(&relay, &chat, reqwest::Method::PUT, Value::Null)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    for hours in [0, 721, -1] {
        assert_eq!(
            change(
                &relay,
                &chat,
                reqwest::Method::PUT,
                json!({"expiresInHours": hours})
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    relay.stop.cancel();
}

#[tokio::test]
async fn deleting_a_conversation_revokes_its_link_and_files() {
    let (relay, chat, _, artifact) = fixture().await;
    let response = change(&relay, &chat, reqwest::Method::PUT, json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let share: Value = response.json().await.unwrap();
    relay.installation.chat_trash(&chat, true).await.unwrap();
    let url = share["publicUrl"].as_str().unwrap();
    for path in [url.to_owned(), format!("{url}/artifacts/{artifact}")] {
        let response = relay.app.client.get(path).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            response.headers()["x-robots-tag"]
                .to_str()
                .unwrap()
                .contains("noindex")
        );
    }
    relay.stop.cancel();
}

#[tokio::test]
async fn public_mcp_sharing_requires_manage_scope_and_matches_the_apps() {
    let (relay, chat, _, _) = fixture().await;
    let worker = std::env::var_os("CAIRN_TEST_CODE_WORKER")
        .map(std::path::PathBuf::from)
        .expect("Use the credential-isolating backend launcher");
    relay.installation.code_mode.use_worker(worker);
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    for scope in ["read", "manage"] {
        let grant = relay
            .app
            .client
            .post(format!(
                "{}/api/installations/{installation}/tokens",
                relay.app.url
            ))
            .header("cookie", &relay.cookie)
            .header("origin", &relay.app.url)
            .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
            .json(&json!({"label": "Sharing test", "scopes": [scope]}))
            .send()
            .await
            .unwrap();
        assert_eq!(grant.status(), StatusCode::CREATED);
        let grant: Value = grant.json().await.unwrap();
        let response = relay.app.client.post(format!("{}/mcp", relay.app.url))
            .bearer_auth(grant["token"].as_str().unwrap()).json(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name":"execute", "arguments": {"code":"return await cairn.set_chat_share(input)", "input":{"chatId":chat,"expiresInHours":48}}}
            })).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response: Value = response.json().await.unwrap();
        if scope == "read" {
            assert_eq!(response["result"]["isError"], true, "{response}");
            assert!(
                response["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("manage scope"),
                "{response}"
            );
            assert!(
                change(&relay, &chat, reqwest::Method::GET, json!({}))
                    .await
                    .json::<Value>()
                    .await
                    .unwrap()["publicUrl"]
                    .is_null()
            );
            continue;
        }
        assert_ne!(response["result"]["isError"], true, "{response}");
        let share = &response["result"]["structuredContent"]["result"];
        let app_share: Value = change(&relay, &chat, reqwest::Method::GET, json!({}))
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(share, &app_share);
        assert_eq!(
            relay
                .app
                .client
                .get(share["publicUrl"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    relay.stop.cancel();
}

#[tokio::test]
async fn oversized_replies_remain_readable_in_bounded_utf8_sections() {
    let (relay, chat, run, _) = fixture().await;
    let text = format!("LONG_REPLY_START{}LONG_REPLY_END", "λ<&".repeat(2_200_000));
    let payload = json!({"type": "item.completed", "item": {"id": "long-reply", "type": "agent_message", "text": &text}});
    relay
        .installation
        .store
        .event(
            &run,
            &cairn_contracts::EventKind::ItemCompleted,
            &text,
            Some(payload),
        )
        .await
        .unwrap();
    let response = change(&relay, &chat, reqwest::Method::PUT, json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let share: Value = response.json().await.unwrap();
    let url = share["publicUrl"].as_str().unwrap();
    let response = relay
        .app
        .client
        .get(url)
        .header("accept-encoding", "identity")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = response.text().await.unwrap();
    assert!(html.len() < 1_000_000);
    assert!(html.contains("LONG_REPLY_START"));
    assert!(!html.contains("LONG_REPLY_END"));
    assert!(html.contains("λ&lt;&amp;"));
    let query = html
        .split("href=\"?message=")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&");
    let next = format!("{url}?message={query}");
    let response = relay
        .app
        .client
        .get(&next)
        .header("accept-encoding", "identity")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = response.text().await.unwrap();
    assert!(html.len() < 1_000_000);
    assert!(html.contains("Continue message"));
    let mut part = reqwest::Url::parse(&next).unwrap();
    let message = part
        .query_pairs()
        .find(|(key, _)| key == "message")
        .unwrap()
        .1
        .into_owned();
    part.set_query(None);
    part.query_pairs_mut()
        .append_pair("message", &message)
        .append_pair(
            "offset",
            &text.floor_char_boundary(text.len() - 100).to_string(),
        );
    let response = relay
        .app
        .client
        .get(part.clone())
        .header("accept-encoding", "identity")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.text().await.unwrap().contains("LONG_REPLY_END"));
    part.set_query(None);
    // LONG_REPLY_START is 16 ASCII bytes, followed by a two-byte lambda.
    part.query_pairs_mut()
        .append_pair("message", &message)
        .append_pair("offset", "17");
    assert_eq!(
        relay.app.client.get(part).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    relay.stop.cancel();
}
