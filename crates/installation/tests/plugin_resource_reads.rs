//! Resource discovery through an execution agent, at the real HTTP, MCP and agent tool
//! boundaries (ADR-0075, « Accès des comptes aux agents » and « Règles de hook »).
mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    validation::text,
};
use common::{AgentFixture, relay_fixture};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

#[path = "fixtures/plugin_grants.rs"]
mod fixture;

use fixture::{Fixture, contribution, support};

const MEMBER: &str = "member-account";

const DENIED: &str =
    "You don't have access to this agent. Ask the installation owner to give you access.";

async fn call(f: &Fixture, account: Option<&str>, method: &str, path: &str) -> (StatusCode, Value) {
    let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
    let identity = match account {
        Some(account) => InstallationIdentity::trusted(InstallationRole::Member, account),
        None => InstallationIdentity::trusted(InstallationRole::Owner, "fixture-owner"),
    };
    let request = common::request(method, path)
        .extension(identity)
        .header("content-type", "application/json")
        .body(Body::from(if method == "PUT" { "{}" } else { "" }))
        .unwrap();
    let response = common::send(&app, request).await;
    (response.status(), common::read_json(response).await)
}

async fn get(f: &Fixture, account: Option<&str>, path: &str) -> (StatusCode, Value) {
    call(f, account, "GET", path).await
}

fn list_path(f: &Fixture, agent: Option<&str>) -> String {
    let through = agent
        .map(|agent| format!("&agentId={agent}"))
        .unwrap_or_default();
    format!(
        "/api/plugins/grant-probe/connections/{}/resources?resourceType=channel&query=sup{through}",
        f.instance
    )
}

fn resolve_path(f: &Fixture, agent: Option<&str>) -> String {
    let through = agent
        .map(|agent| format!("&agentId={agent}"))
        .unwrap_or_default();
    format!(
        "/api/plugins/grant-probe/connections/{}/resources/ancestors?resourceType=thread&id=support-thread{through}",
        f.instance
    )
}

/// Starts a read and returns its operation.
async fn start(f: &Fixture, account: Option<&str>, path: &str) -> String {
    let (status, started) = get(f, account, path).await;
    assert_eq!(status, StatusCode::OK, "{started}");
    text(&started, "operation").to_owned()
}

/// The operation once its task settles, observed directly rather than through a caller.
async fn finished(f: &Fixture, operation: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let state = f.s.plugins.operations.get(operation).unwrap();
            if state["state"] != "running" {
                break state;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the resource read settles")
}

/// Polls through the general preparation route the apps use, as the given caller.
async fn poll(f: &Fixture, account: Option<&str>, operation: &str) -> (StatusCode, Value) {
    finished(f, operation).await;
    get(
        f,
        account,
        &format!("/api/plugins/packages/operations/{operation}"),
    )
    .await
}

/// An agent holding only the `message` hook on the probe instance, usable by the member.
async fn reviewer(f: &Fixture) -> String {
    let agent =
        f.s.agent_fixture(json!({ "name": "Reviewer" }), None)
            .await
            .unwrap();
    let agent = text(&agent, "id").to_owned();
    f.set_for(&agent, json!([contribution("hook", "message", support())]))
        .await;
    relay_fixture::allow_member(&f.s, MEMBER, &agent).await;
    agent
}

fn resolution(operation: &Value) -> Value {
    assert_eq!(operation["state"], "completed", "{operation}");
    let resolution = &operation["result"]["resolution"];
    let ancestors = resolution["ancestors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ancestor| text(ancestor, "id"))
        .collect::<Vec<_>>();
    json!({ "resource": text(&resolution["resource"], "name"), "ancestors": ancestors })
}

fn expected_resolution() -> Value {
    json!({ "resource": "support-thread", "ancestors": ["support", "guild"] })
}

#[tokio::test]
async fn the_owner_keeps_reading_with_the_instance_and_may_choose_an_agent() {
    let f = Fixture::new().await;
    let agent = reviewer(&f).await;

    let operation = start(&f, None, &resolve_path(&f, None)).await;
    let (status, read) = poll(&f, None, &operation).await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(resolution(&read), expected_resolution());
    let operation = start(&f, None, &list_path(&f, None)).await;
    assert_eq!(poll(&f, None, &operation).await.1["state"], "completed");

    // Through an agent, the owner reads only what that agent is granted.
    let operation = start(&f, None, &resolve_path(&f, Some(&agent))).await;
    assert_eq!(
        resolution(&poll(&f, None, &operation).await.1),
        expected_resolution()
    );
    let (status, refused) = get(&f, None, &resolve_path(&f, Some(MAIN_AGENT_ID))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(
        refused["error"],
        "This agent is not granted that plugin instance."
    );
}

#[tokio::test]
async fn a_member_reads_through_an_agent_they_may_use_and_polls_only_their_reads() {
    let f = Fixture::new().await;
    let agent = reviewer(&f).await;

    let operation = start(&f, Some(MEMBER), &resolve_path(&f, Some(&agent))).await;
    let (status, read) = poll(&f, Some(MEMBER), &operation).await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(resolution(&read), expected_resolution());
    // The resource-only route returns the same read.
    let (status, again) = get(
        &f,
        Some(MEMBER),
        &format!("/api/plugins/resources/operations/{operation}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again, read);

    let operation = start(&f, Some(MEMBER), &list_path(&f, Some(&agent))).await;
    let (status, page) = poll(&f, Some(MEMBER), &operation).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["state"], "completed", "{page}");
    assert_eq!(page["result"]["page"]["items"], json!([]));

    // Another member cannot poll it, even one with access to the same agent.
    relay_fixture::allow_member(&f.s, "other-member", &agent).await;
    let (status, refused) = poll(&f, Some("other-member"), &operation).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");

    // Members still cannot read the owner's preparations or anything else of plugins.
    let owned = start(&f, None, &resolve_path(&f, None)).await;
    let (status, refused) = poll(&f, Some(MEMBER), &owned).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    for path in [
        "/api/plugins".to_owned(),
        "/api/plugins/grant-probe/programs".to_owned(),
        format!("/api/plugins/grant-probe/connections/{}/view", f.instance),
    ] {
        let (status, refused) = get(&f, Some(MEMBER), &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {refused}");
    }
    let (status, _) = call(
        &f,
        Some(MEMBER),
        "DELETE",
        &format!("/api/plugins/packages/operations/{operation}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_member_needs_an_agent_they_may_use_with_a_grant_on_the_instance() {
    let f = Fixture::new().await;
    reviewer(&f).await;
    let posts = f.posts.load(Ordering::SeqCst);

    // Without an agent, reads stay the owner's.
    for path in [list_path(&f, None), resolve_path(&f, None)] {
        let (status, refused) = get(&f, Some(MEMBER), &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    }

    // The main agent holds the instance, but the member may not use it.
    f.set(json!([contribution("hook", "message", support())]))
        .await;
    for path in [
        list_path(&f, Some(MAIN_AGENT_ID)),
        resolve_path(&f, Some(MAIN_AGENT_ID)),
    ] {
        let (status, refused) = get(&f, Some(MEMBER), &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        assert_eq!(refused["error"], DENIED);
    }

    // An agent the member may use, without a grant on this instance.
    let ungranted =
        f.s.agent_fixture(json!({ "name": "Ungranted" }), None)
            .await
            .unwrap();
    let ungranted = text(&ungranted, "id");
    relay_fixture::allow_member(&f.s, MEMBER, ungranted).await;
    for path in [
        list_path(&f, Some(ungranted)),
        resolve_path(&f, Some(ungranted)),
    ] {
        let (status, refused) = get(&f, Some(MEMBER), &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        assert_eq!(
            refused["error"],
            "This agent is not granted that plugin instance."
        );
    }

    // A former member's entries never admit a read.
    let agent = reviewer(&f).await;
    relay_fixture::remove_member(&f.s, MEMBER).await;
    let (status, refused) = get(&f, Some(MEMBER), &resolve_path(&f, Some(&agent))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(f.posts.load(Ordering::SeqCst), posts);
}

#[tokio::test]
async fn revoking_access_during_a_read_stops_its_plugin_calls_and_result() {
    let f = Fixture::new().await;
    let agent = reviewer(&f).await;

    // Listing calls the service twice; the first call waits while access is revoked.
    f.pause.store(true, Ordering::SeqCst);
    let operation = start(&f, Some(MEMBER), &list_path(&f, Some(&agent))).await;
    f.entered.notified().await;
    let (status, revoked) = call(
        &f,
        None,
        "DELETE",
        &format!("/api/agents/{agent}/accounts/{MEMBER}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    f.release.notify_one();
    let read = finished(&f, &operation).await;
    assert_eq!(read["state"], "failed", "{read}");
    assert!(read["result"].is_null());
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    let (status, refused) = poll(&f, Some(MEMBER), &operation).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");

    // A completed read is not delivered once its plugin grant is removed.
    relay_fixture::allow_member(&f.s, MEMBER, &agent).await;
    let operation = start(&f, Some(MEMBER), &resolve_path(&f, Some(&agent))).await;
    assert_eq!(finished(&f, &operation).await["state"], "completed");
    f.remove_for(&agent).await;
    for path in [
        format!("/api/plugins/packages/operations/{operation}"),
        format!("/api/plugins/resources/operations/{operation}"),
    ] {
        let (status, refused) = get(&f, Some(MEMBER), &path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        assert!(refused.get("result").is_none(), "{refused}");
    }

    // Nor once the member loses access to the agent.
    f.set_for(&agent, json!([contribution("hook", "message", support())]))
        .await;
    let operation = start(&f, Some(MEMBER), &resolve_path(&f, Some(&agent))).await;
    assert_eq!(finished(&f, &operation).await["state"], "completed");
    call(
        &f,
        None,
        "DELETE",
        &format!("/api/agents/{agent}/accounts/{MEMBER}"),
    )
    .await;
    let (status, refused) = poll(&f, Some(MEMBER), &operation).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"], DENIED);
}

#[tokio::test]
async fn a_new_membership_of_the_same_account_never_inherits_a_read() {
    let f = Fixture::new().await;
    let agent = reviewer(&f).await;

    // Re-invited while the plugin call is in flight.
    f.pause.store(true, Ordering::SeqCst);
    let operation = start(&f, Some(MEMBER), &list_path(&f, Some(&agent))).await;
    f.entered.notified().await;
    relay_fixture::add_member(&f.s, MEMBER, "membership-2").await;
    f.release.notify_one();
    assert_eq!(finished(&f, &operation).await["state"], "failed");
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);

    // Re-invited after a read completed, before it is polled.
    relay_fixture::allow_member(&f.s, MEMBER, &agent).await;
    let operation = start(&f, Some(MEMBER), &resolve_path(&f, Some(&agent))).await;
    assert_eq!(finished(&f, &operation).await["state"], "completed");
    relay_fixture::add_member(&f.s, MEMBER, "membership-3").await;
    let (status, refused) = poll(&f, Some(MEMBER), &operation).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert!(refused.get("result").is_none(), "{refused}");

    // The new membership holds no access to the agent until the owner gives it again.
    let (status, refused) = get(&f, Some(MEMBER), &resolve_path(&f, Some(&agent))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["error"], DENIED);
    let (status, _) = call(
        &f,
        None,
        "PUT",
        &format!("/api/agents/{agent}/accounts/{MEMBER}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let operation = start(&f, Some(MEMBER), &resolve_path(&f, Some(&agent))).await;
    assert_eq!(
        resolution(&poll(&f, Some(MEMBER), &operation).await.1),
        expected_resolution()
    );
}

async fn settle(f: &Fixture, endpoint: &str, token: &str, started: &Value) -> Value {
    assert_ne!(started["result"]["isError"], true, "{started}");
    let operation = text(
        &started["result"]["structuredContent"]["result"],
        "operation",
    )
    .to_owned();
    finished(f, &operation).await;
    f.mcp(
        endpoint,
        token,
        "get_plugin_resource_operation",
        json!({ "operationId": operation }),
    )
    .await
}

fn resolve_input(f: &Fixture, agent: Option<&str>) -> Value {
    let mut input = json!({
        "pluginId": "grant-probe",
        "instanceId": f.instance,
        "resourceType": "thread",
        "id": "support-thread",
    });
    if let Some(agent) = agent {
        input["agentId"] = agent.into();
    }
    input
}

#[tokio::test]
async fn the_public_mcp_reads_through_an_agent_within_its_scope_and_grants() {
    let f = Fixture::new().await;
    let agent = reviewer(&f).await;
    let read = relay_fixture::grant(&f.s, &["read"]).await;

    let started = f
        .mcp(
            "/mcp",
            &read,
            "resolve_plugin_resource",
            resolve_input(&f, Some(&agent)),
        )
        .await;
    let polled = settle(&f, "/mcp", &read, &started).await;
    assert_ne!(polled["result"]["isError"], true, "{polled}");
    assert_eq!(
        resolution(&polled["result"]["structuredContent"]["result"]),
        expected_resolution()
    );

    // The agent's grants still bound the read.
    let refused = f
        .mcp(
            "/mcp",
            &read,
            "resolve_plugin_resource",
            resolve_input(&f, Some(MAIN_AGENT_ID)),
        )
        .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(
        refused
            .to_string()
            .contains("This agent is not granted that plugin instance."),
        "{refused}"
    );

    // Without the read scope, nothing is read through any agent.
    let run = relay_fixture::grant(&f.s, &["run"]).await;
    let refused = f
        .mcp(
            "/mcp",
            &run,
            "resolve_plugin_resource",
            resolve_input(&f, Some(&agent)),
        )
        .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(
        refused.to_string().contains("The read scope is required."),
        "{refused}"
    );
}

#[tokio::test]
async fn an_installation_agent_reads_only_through_its_own_grants() {
    let f = Fixture::new().await;
    let agent = reviewer(&f).await;
    let posts = f.posts.load(Ordering::SeqCst);

    // The calling agent holds no grant: naming an agent that does widens nothing.
    for input in [resolve_input(&f, Some(&agent)), resolve_input(&f, None)] {
        let refused = f
            .mcp(
                "/mcp-installation",
                &f.bearer,
                "resolve_plugin_resource",
                input,
            )
            .await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");
    }

    // Once granted, it still reads only as itself.
    f.set(json!([contribution("hook", "message", support())]))
        .await;
    let refused = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "resolve_plugin_resource",
            resolve_input(&f, Some(&agent)),
        )
        .await;
    assert!(
        refused
            .to_string()
            .contains("An installation agent reads resources only through its own grants."),
        "{refused}"
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), posts);
    for input in [
        resolve_input(&f, Some(MAIN_AGENT_ID)),
        resolve_input(&f, None),
    ] {
        let started = f
            .mcp(
                "/mcp-installation",
                &f.bearer,
                "resolve_plugin_resource",
                input,
            )
            .await;
        let polled = settle(&f, "/mcp-installation", &f.bearer, &started).await;
        assert_ne!(polled["result"]["isError"], true, "{polled}");
        assert_eq!(
            resolution(&polled["result"]["structuredContent"]["result"]),
            expected_resolution()
        );
    }
}

const NOT_GRANTED: &str = "This agent is not granted that plugin instance.";

/// A list and a resolve of one resource type, through an agent when one is named.
fn reads(plugin: &str, instance: &str, kind: &str, agent: Option<&str>) -> [String; 2] {
    let through = agent
        .map(|agent| format!("&agentId={agent}"))
        .unwrap_or_default();
    let base = format!("/api/plugins/{plugin}/connections/{instance}/resources");
    [
        format!("{base}?resourceType={kind}{through}"),
        format!("{base}/ancestors?resourceType={kind}&id=support{through}"),
    ]
}

/// Every read refuses with the same status and message.
async fn refused_alike(
    f: &Fixture,
    account: Option<&str>,
    paths: &[String],
    expected: (StatusCode, &str),
) {
    for path in paths {
        let (status, refused) = get(f, account, path).await;
        assert_eq!(
            (status, refused["error"].as_str().unwrap_or_default()),
            expected,
            "{path}: {refused}"
        );
    }
}

async fn set_enabled(f: &Fixture, enabled: bool) {
    let mut instance =
        f.s.store
            .plugin_instance(&f.instance)
            .await
            .unwrap()
            .unwrap();
    instance.enabled = enabled;
    instance.revision = cairn_installation::config::id();
    f.s.store
        .transaction(move |db| db.save_plugin_instance(&instance))
        .await
        .unwrap();
}

async fn set_approval(f: &Fixture, digest: &str) {
    let digest = digest.to_owned();
    f.s.store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE plugin_approvals SET digest=?1 WHERE plugin_id='grant-probe'",
                [digest],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn an_ungranted_agent_reveals_nothing_of_the_instance_or_its_package() {
    let f = Fixture::new().await;
    let granted = reviewer(&f).await;
    let ungranted =
        f.s.agent_fixture(json!({ "name": "Ungranted" }), None)
            .await
            .unwrap();
    let ungranted = text(&ungranted, "id").to_owned();
    relay_fixture::allow_member(&f.s, MEMBER, &ungranted).await;
    let unknown = "00000000-0000-4000-8000-0000000000ff";
    let probes = |agent: &str| {
        [
            reads("grant-probe", &f.instance, "channel", Some(agent)),
            reads("grant-probe", unknown, "channel", Some(agent)),
            reads("missing-plugin", &f.instance, "channel", Some(agent)),
            reads("grant-probe", &f.instance, "unknown-type", Some(agent)),
        ]
        .concat()
    };
    let not_granted = (StatusCode::FORBIDDEN, NOT_GRANTED);
    let posts = f.posts.load(Ordering::SeqCst);

    // Known, unknown, foreign and untyped instances look the same without a grant.
    refused_alike(&f, Some(MEMBER), &probes(&ungranted), not_granted).await;
    refused_alike(&f, None, &probes(&ungranted), not_granted).await;
    refused_alike(
        &f,
        Some(MEMBER),
        &[
            reads("grant-probe", unknown, "channel", Some(&granted)),
            reads("missing-plugin", &f.instance, "channel", Some(&granted)),
        ]
        .concat(),
        not_granted,
    )
    .await;
    let untyped = reads("grant-probe", &f.instance, "unknown-type", Some(&granted));
    refused_alike(
        &f,
        Some(MEMBER),
        &untyped,
        (
            StatusCode::NOT_FOUND,
            "This plugin has no such resource type.",
        ),
    )
    .await;

    // So do a disabled instance and a package changed since its approval.
    set_enabled(&f, false).await;
    refused_alike(&f, Some(MEMBER), &probes(&ungranted), not_granted).await;
    let disabled = (
        StatusCode::BAD_REQUEST,
        "Enable this instance before reading its resources.",
    );
    let granted_reads = reads("grant-probe", &f.instance, "channel", Some(&granted));
    refused_alike(&f, Some(MEMBER), &granted_reads, disabled).await;
    let owned = reads("grant-probe", &f.instance, "channel", None);
    refused_alike(&f, None, &owned, disabled).await;
    set_enabled(&f, true).await;

    let approved =
        f.s.store
            .transaction(|db| {
                Ok(db
                    .plugin_package("grant-probe")?
                    .approvals
                    .get("1")
                    .cloned()
                    .unwrap())
            })
            .await
            .unwrap();
    set_approval(&f, &"0".repeat(64)).await;
    refused_alike(&f, Some(MEMBER), &probes(&ungranted), not_granted).await;
    let changed = (
        StatusCode::FORBIDDEN,
        "This plugin changed since it was approved. Review it in Plugins.",
    );
    refused_alike(&f, Some(MEMBER), &granted_reads, changed).await;
    refused_alike(&f, None, &owned, changed).await;
    set_approval(&f, &approved).await;
    assert_eq!(f.posts.load(Ordering::SeqCst), posts);

    // With the grant, the same reads go through.
    for path in &granted_reads {
        let operation = start(&f, Some(MEMBER), path).await;
        let (status, read) = poll(&f, Some(MEMBER), &operation).await;
        assert_eq!(status, StatusCode::OK, "{read}");
        assert_eq!(read["state"], "completed", "{read}");
    }
}
