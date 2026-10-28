use super::*;
use axum::response::IntoResponse;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn an_authorized_resource_tool_finishes_after_its_first_provider_read() {
    Box::pin(resource_approval(Surface::Tool, Barrier::Read, false)).await;
}

#[tokio::test]
async fn a_resource_tool_cannot_reacquire_target_approval_during_provider_io() {
    Box::pin(resource_approval(Surface::Tool, Barrier::Read, true)).await;
}

#[tokio::test]
async fn an_authorized_resource_command_finishes_after_its_first_provider_read() {
    Box::pin(resource_approval(Surface::Command, Barrier::Read, false)).await;
}

#[tokio::test]
async fn a_resource_command_cannot_reacquire_target_approval_during_provider_io() {
    Box::pin(resource_approval(Surface::Command, Barrier::Read, true)).await;
}

#[tokio::test]
async fn an_authorized_resource_tool_confirms_its_write_receipt() {
    Box::pin(resource_approval(Surface::Tool, Barrier::Write, false)).await;
}

#[tokio::test]
async fn an_authorized_resource_command_confirms_its_write_receipt() {
    Box::pin(resource_approval(Surface::Command, Barrier::Write, false)).await;
}

#[tokio::test]
async fn a_resource_tool_cannot_confirm_or_repeat_a_write_after_target_reapproval() {
    Box::pin(resource_approval(Surface::Tool, Barrier::Write, true)).await;
}

#[tokio::test]
async fn a_resource_command_cannot_confirm_or_repeat_a_write_after_target_reapproval() {
    Box::pin(resource_approval(Surface::Command, Barrier::Write, true)).await;
}

#[derive(Clone, Copy)]
enum Surface {
    Tool,
    Command,
}

#[derive(Clone, Copy, PartialEq)]
enum Barrier {
    Read,
    Write,
}

impl Barrier {
    fn phase(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

async fn invoke(
    s: &Service,
    bearer: &str,
    resource: &str,
    surface: Surface,
    barrier: Barrier,
) -> Result<serde_json::Value, cairn_installation::error::Error> {
    match surface {
        Surface::Tool => agents::call_tool(
            s,
            bearer,
            "approval_bound_write",
            &json!({
                "resource": resource, "phase": barrier.phase()
            }),
        )
        .await
        .map(|result| result.expect("Granted resource tool must be available")),
        Surface::Command => s
            .plugins
            .command(
                s,
                bearer,
                &json!({
                    "plugin": "approval-bound", "command": "write",
                    "args": ["--resource", resource, "--phase", barrier.phase()]
                }),
            )
            .await
            .map(|result| result["value"].clone()),
    }
}

async fn resource_approval(surface: Surface, barrier: Barrier, reapprove: bool) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    let mut config = common::config(root.path());
    config.runner_url = "http://runner:4311".into();
    let s = Service::new(config).await.unwrap();
    common::relay_fixture::claimed(&s).await.unwrap();
    s.plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let package = registry::install(&s, &json!({
        "manifest": {
            "id": "approval-bound", "name": "Approval bound", "version": "1", "apiVersion": 2,
            "capabilities": ["resources", "http"], "hosts": ["api.example.test"], "agentAccess": {},
            "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace", "settings": [
                {"key": "token", "title": "Token", "type": "secret", "required": true}
            ]}],
            "operations": [{"name": "write", "collection": "workspace", "profile": "contribute", "requests": [
                {"method": "GET", "host": "api.example.test", "path": "/barrier"},
                {"method": "POST", "host": "api.example.test", "path": "/write"}
            ]}],
            "tools": [{"name": "approval_bound_write", "description": "Write within the granted workspace", "operation": "write",
                "inputSchema": {"type": "object", "required": ["resource", "phase"], "properties": {
                    "resource": {"type": "string"}, "phase": {"type": "string"}
                }}}],
            "commands": [{"name": "write", "operation": "write", "arguments": [
                {"name": "resource", "flag": "--resource", "required": true},
                {"name": "phase", "flag": "--phase", "required": true}
            ]}]
        },
        "source": r#"local function invoke(event)
            local input = event.input or event.arguments
            if input.phase == 'read' then
                cairn.http.request({url = 'https://api.example.test/barrier',
                    auth = {resource = input.resource, secret = 'token', prefix = 'Bearer '}})
            end
            return cairn.http.request({url = 'https://api.example.test/write', method = 'POST',
                auth = {resource = input.resource, secret = 'token', prefix = 'Bearer '},
                idempotencyKey = 'approval-write:' .. input.resource, json = {text = 'Approved write'}}).json
        end
        return {tool = invoke, command = invoke}"#
    })).await.unwrap();
    registry::approve(&s, "approval-bound", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &s,
        "approval-bound",
        &json!({"name": "Workspace app", "enabled": true}),
        None,
    )
    .await
    .unwrap();
    let instance = text(&instance, "id");
    let resource = resources::save(&s, "approval-bound", instance, "workspace",
        &json!({"name": "Engineering", "settings": {}, "secrets": {"token": "target-approval-fixture"}}), None).await.unwrap();
    let resource = text(&resource, "id");
    let app = cairn_installation::http::router(s.clone()).await.unwrap();
    let mut request = common::request("POST", &format!("/api/plugins/approval-bound/connections/{instance}/resources/workspace/{resource}/grants"))
        .header("content-type", "application/json")
        .body(axum::Json(json!({"agentId": MAIN_AGENT_ID, "profile": "contribute"})).into_response().into_body()).unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(
            InstallationRole::Owner,
            "fixture-owner",
        ));
    let response = common::send(&app, request).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let task = s
        .task(
            json!({"name": "Write resource", "prompt": "Write resource", "agentId": MAIN_AGENT_ID}),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    let run = s
        .store
        .patch_run(
            text(&run, "id"),
            json!({"status": "running", "isolated": true}),
        )
        .await
        .unwrap();
    let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
    let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let writes = Arc::new(AtomicUsize::new(0));
    let (entering, releasing, writing) = (entered.clone(), release.clone(), writes.clone());
    let (write_entering, write_releasing) = (entered.clone(), release.clone());
    let provider = axum::Router::new()
        .route(
            "/barrier",
            axum::routing::get(move || {
                let (entered, release) = (entering.clone(), releasing.clone());
                async move {
                    entered.notify_one();
                    release.notified().await;
                    axum::Json(json!({"ok": true}))
                }
            }),
        )
        .route(
            "/write",
            axum::routing::post(move |headers: axum::http::HeaderMap| {
                let writes = writing.clone();
                let (entered, release) = (write_entering.clone(), write_releasing.clone());
                async move {
                    assert_eq!(headers["authorization"], "Bearer target-approval-fixture");
                    writes.fetch_add(1, Ordering::SeqCst);
                    if barrier == Barrier::Write {
                        entered.notify_one();
                        release.notified().await;
                    }
                    axum::Json(json!({"ok": true}))
                }
            }),
        );
    let (origin, server) = common::serve_locally(provider).await;
    s.plugins.route_http_to(Some(origin));
    let running = s.clone();
    let resource = resource.to_owned();
    let (running_bearer, running_resource) = (bearer.clone(), resource.clone());
    let mut pending = tokio::spawn(async move {
        invoke(
            &running,
            &running_bearer,
            &running_resource,
            surface,
            barrier,
        )
        .await
    });
    tokio::select! {
        () = entered.notified() => {},
        result = &mut pending => panic!("Invocation ended before provider barrier: {result:?}"),
        () = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("Invocation never reached provider barrier"),
    }
    if reapprove {
        registry::approve(&s, "approval-bound", "1", text(&package, "digest"))
            .await
            .unwrap();
    }
    release.notify_one();
    let result = pending.await.unwrap();
    let receipts = s.store.list("plugin-http-receipts").await.unwrap();
    if reapprove {
        assert_eq!(
            writes.load(Ordering::SeqCst),
            usize::from(barrier == Barrier::Write),
            "Old handler sent the resource secret under a new target approval"
        );
        assert!(
            result.is_err(),
            "Old handler returned a result under a new target approval: {result:?}"
        );
        assert!(
            receipts.iter().all(|r| r["state"] != "completed"),
            "Old handler published a confirmed receipt after its target approval changed"
        );
        if barrier == Barrier::Write {
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0]["state"], "pending");
            let retry = invoke(&s, &bearer, &resource, surface, barrier).await;
            assert!(
                retry.is_err(),
                "An uncertain write must not be retried: {retry:?}"
            );
            assert_eq!(writes.load(Ordering::SeqCst), 1);
            assert_eq!(
                s.store.list("plugin-http-receipts").await.unwrap(),
                receipts
            );
        }
    } else {
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        assert_eq!(result.unwrap(), json!({"ok": true}));
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0]["state"], "completed");
    }
    server.abort();
}
