use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

#[tokio::test]
async fn an_unchanged_ingress_can_admit_after_its_provider_lookup() {
    approval_during_lookup(false).await;
}

#[tokio::test]
async fn a_reapproved_ingress_cannot_admit_after_its_provider_lookup() {
    approval_during_lookup(true).await;
}

async fn approval_during_lookup(reapprove: bool) {
    let source = r#"return { webhook = function(event)
        cairn.http.request({url = 'https://api.example.test/lookup'})
        local result = cairn.inbound.deliver(cairn.json.decode(event.body))
        return {status = 200, body = cairn.json.encode(result)}
    end }"#;
    let (_root, app, service, path) =
        fixture_with(source, &["http", "inbound"], json!({"userIds": ["alice"]})).await;
    let package = registry::active(&service, "inbox").await.unwrap();

    let hold = Arc::new(AtomicBool::new(true));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (held, entering, releasing) = (hold, entered.clone(), release.clone());
    let provider = Router::new().route(
        "/lookup",
        axum::routing::get(move || {
            let (held, entered, release) = (held.clone(), entering.clone(), releasing.clone());
            async move {
                if held.swap(false, Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                axum::Json(json!({"ok": true}))
            }
        }),
    );
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let body = json!({
        "id": "original-dispatch",
        "author": {"id": "alice"},
        "body": {"text": "Hello"},
    })
    .to_string();
    let request = signed(&path, now() / 1000, &body);
    let routed = app.clone();
    let pending = tokio::spawn(async move { common::send(&routed, request).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("The actual ingress worker must reach its provider lookup");
    if reapprove {
        registry::approve(&service, "inbox", "1", &package.digest)
            .await
            .unwrap();
    }
    release.notify_one();
    let response = pending.await.unwrap();
    assert_eq!(
        response.status(),
        if reapprove {
            StatusCode::CONFLICT
        } else {
            StatusCode::OK
        },
        "{}",
        common::read_text(response).await
    );
    // A late refusal alone cannot prove the retired handler had no effect.
    // Read admission through a fresh signed invocation of the same event ID.
    let response = common::send(&app, signed(&path, now() / 1000, &body)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        common::read_json(response).await["status"],
        if reapprove { "accepted" } else { "duplicate" }
    );
    server.abort();
}
