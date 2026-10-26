//! Entry authentication survives slow transport input, before tool dispatch.
use super::*;
use std::convert::Infallible;

pub(super) async fn call(
    app: &axum::Router,
    bearer: &str,
    path: &str,
    body: Body,
) -> (StatusCode, Value) {
    let response = common::send(
        app,
        common::request("POST", path)
            .header("authorization", format!("Bearer {bearer}"))
            .header(
                "mcp-protocol-version",
                cairn_installation::mcp_client::MODERN,
            )
            .header("mcp-method", "tools/call")
            .body(body)
            .unwrap(),
    )
    .await;
    (response.status(), common::read_json(response).await)
}

async fn retained(path: &'static str) {
    let f = Started::with_access(None, "http://runner:4311", path == "/mcp-installation").await;
    if path == "/plugin-command" {
        let package = registry::install(
            &f.s,
            &json!({
                "manifest": {
                    "id": "entry-reader", "name": "Entry reader", "version": "1", "apiVersion": 2,
                    "capabilities": [], "agentAccess": {},
                    "commands": [{"name": "read", "output": "text"}],
                },
                "source": "return {command = function(event) return 'entry-private-profile' end}",
            }),
        )
        .await
        .unwrap();
        registry::approve(&f.s, "entry-reader", "1", text(&package, "digest"))
            .await
            .unwrap();
        connections::save(&f.s, "entry-reader", &json!({
            "name": "Explicit CLI grant", "enabled": true, "agentIds": [f.run["snapshot"]["agent"]["id"]],
        }), None).await.unwrap();
    }
    let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
    let frame = match path {
        "/mcp-workspace" => json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "report_outcome", "arguments": {"status": "needs_input", "reason": "Original outcome", "evidence": []},
        }}),
        "/mcp-installation" => {
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": common::mcp_arguments("list_agents", &json!({})),
            })
        }
        "/plugin-command" => json!({"plugin": "entry-reader", "command": "read", "args": []}),
        _ => unreachable!(),
    };
    let first = call(&app, &f.bearer, path, Body::from(frame.to_string())).await;
    assert_eq!(first.0, StatusCode::OK, "{first:?}");
    assert!(first.1.get("error").is_none(), "{first:?}");
    if path == "/mcp-installation" {
        assert!(
            first.1.to_string().contains("Dedicated helper"),
            "{first:?}"
        );
    } else if path == "/plugin-command" {
        assert!(
            first.1.to_string().contains("entry-private-profile"),
            "{first:?}"
        );
    } else {
        assert_eq!(
            f.s.store.run(text(&f.run, "id")).await.unwrap()["outcome"]["reason"],
            "Original outcome"
        );
    }
    let mut frame = frame;
    if path == "/mcp-workspace" {
        frame["params"]["arguments"]["reason"] = json!("Retired operation");
    }
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (ready, resume, packet) = (entered.clone(), release.clone(), frame.to_string());
    let body = Body::from_stream(futures_util::stream::once(async move {
        ready.notify_one();
        resume.notified().await;
        Ok::<_, Infallible>(axum::body::Bytes::from(packet))
    }));
    let (pending_app, token) = (app.clone(), f.bearer.clone());
    let pending = tokio::spawn(async move { call(&pending_app, &token, path, body).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let package = registry::active(&f.s, "router").await.unwrap();
    registry::approve(&f.s, "router", "1", &package.digest)
        .await
        .unwrap();
    release.notify_one();
    let retired = pending.await.unwrap();
    if path == "/mcp-workspace" {
        assert_eq!(
            f.s.store.run(text(&f.run, "id")).await.unwrap()["outcome"]["reason"],
            "Original outcome",
            "An old HTTP entry borrowed the replacement source approval: {retired:?}"
        );
    } else {
        let marker = if path == "/mcp-installation" {
            "Dedicated helper"
        } else {
            "entry-private-profile"
        };
        assert!(
            !retired.1.to_string().contains(marker),
            "An old HTTP entry exported private output under a replacement approval: {retired:?}"
        );
    }
    let fresh = call(&app, &f.bearer, path, Body::from(frame.to_string())).await;
    assert_eq!(fresh.0, StatusCode::OK, "{fresh:?}");
    assert!(fresh.1.get("error").is_none(), "{fresh:?}");
    if path == "/mcp-workspace" {
        assert_eq!(
            f.s.store.run(text(&f.run, "id")).await.unwrap()["outcome"]["reason"],
            "Retired operation"
        );
    } else {
        let marker = if path == "/mcp-installation" {
            "Dedicated helper"
        } else {
            "entry-private-profile"
        };
        assert!(fresh.1.to_string().contains(marker), "{fresh:?}");
    }
}

#[tokio::test]
async fn workspace_entry_authentication_survives_body_read() {
    retained("/mcp-workspace").await;
}

#[tokio::test]
async fn installation_entry_authentication_survives_body_read() {
    retained("/mcp-installation").await;
}

#[tokio::test]
async fn cli_entry_authentication_survives_body_read() {
    retained("/plugin-command").await;
}
