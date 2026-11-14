//! Hook rules through the authenticated API, the public MCP and the installation tools
//! (ADR-0075, « Règles de hook » and « Autorité des règles »).
mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    validation::text,
};
use common::AgentFixture;
use serde_json::{Value, json};

#[path = "fixtures/plugin_grants.rs"]
mod fixture;

use fixture::{Fixture, contribution, support};

const OWNER: &str = "fixture-owner";
const MEMBER: &str = "member-account";
const OTHER: &str = "other-member";

async fn call(
    f: &Fixture,
    account: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
    let role = if account == OWNER {
        InstallationRole::Owner
    } else {
        InstallationRole::Member
    };
    let request = common::request(method, path)
        .extension(InstallationIdentity::trusted(role, account))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = common::send(&app, request).await;
    (response.status(), common::read_json(response).await)
}

fn mcp_result(response: &Value) -> &Value {
    &response["result"]["structuredContent"]["result"]
}

/// A rule answering messages from chosen people in the support channel.
fn definition(f: &Fixture, agent: &str) -> Value {
    json!({
        "name": "Support replies",
        "description": "Answers the support channel.",
        "trigger": { "type": "plugin", "instanceId": f.instance, "hook": "message" },
        "executionAgentId": agent,
        "steps": [{
            "type": "if",
            "id": "known",
            "condition": {
                "type": "all",
                "conditions": [
                    {
                        "type": "authorIn",
                        "author": { "type": "formula", "expression": "payload.author" },
                        "people": [{ "resourceType": "person", "id": "alice" }],
                    },
                    {
                        "type": "compare",
                        "left": { "type": "formula", "expression": "channel" },
                        "operator": "equals",
                        "right": { "type": "literal", "value": "support" },
                    },
                ],
            },
            "then": [
                {
                    "type": "action",
                    "id": "post",
                    "label": "Acknowledge",
                    "action": {
                        "type": "callTool",
                        "instanceId": f.instance,
                        "tool": "grant_probe_post",
                        "input": {
                            "channel": { "type": "literal", "value": "support" },
                            "thread": { "type": "template", "text": "{{payload.thread}}" },
                        },
                    },
                },
                {
                    "type": "action",
                    "id": "remember",
                    "action": {
                        "type": "storeValue",
                        "scope": "rule",
                        "name": "lastAuthor",
                        "value": { "type": "formula", "expression": "truncate(payload.author, 40)" },
                    },
                },
                {
                    "type": "action",
                    "id": "pause",
                    "action": { "type": "wait", "seconds": { "type": "literal", "value": 60 } },
                },
            ],
            "otherwise": [],
        }],
        "limits": { "triggeringsPerHour": 30, "conversationsPerHour": 5 },
    })
}

async fn granted() -> Fixture {
    let f = Fixture::new().await;
    f.set(json!([
        contribution("hook", "message", support()),
        contribution("tool", "grant_probe_post", support()),
    ]))
    .await;
    f
}

async fn create(f: &Fixture, account: &str, definition: Value) -> (StatusCode, Value) {
    call(
        f,
        account,
        "POST",
        "/api/hook-rules",
        json!({ "definition": definition }),
    )
    .await
}

fn kinds(rule: &Value) -> Vec<&str> {
    rule["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["kind"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn the_owner_creates_versions_and_lists_a_rule_without_running_it() {
    let f = granted().await;
    let (status, rule) = create(&f, OWNER, definition(&f, MAIN_AGENT_ID)).await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    assert_eq!(rule["version"], 1);
    assert_eq!(rule["creatorAccountId"], OWNER);
    assert_eq!(rule["source"], "app");
    assert!(kinds(&rule).is_empty(), "{rule}");
    assert_eq!(rule["allowed"]["edit"], true);
    assert_eq!(rule["allowed"]["emergencyPause"], true);
    assert_eq!(rule["warnings"], json!([]));
    let id = text(&rule, "id").to_owned();

    let (_, listed) = call(&f, OWNER, "GET", "/api/hook-rules", Value::Null).await;
    assert_eq!(listed["rules"].as_array().unwrap().len(), 1);
    assert_eq!(listed["rules"][0]["name"], "Support replies");
    let path = format!("/api/hook-rules?instanceId={}", f.instance);
    let (_, by_instance) = call(&f, OWNER, "GET", &path, Value::Null).await;
    assert_eq!(by_instance["rules"].as_array().unwrap().len(), 1);
    let path = format!(
        "/api/hook-rules?agentId={MAIN_AGENT_ID}&instanceId={}",
        f.instance
    );
    let (_, both) = call(&f, OWNER, "GET", &path, Value::Null).await;
    assert_eq!(both["rules"].as_array().unwrap().len(), 1, "{both}");
    let other = cairn_installation::config::id();
    let path = format!("/api/hook-rules?agentId={other}");
    let (_, by_agent) = call(&f, OWNER, "GET", &path, Value::Null).await;
    assert_eq!(by_agent["rules"], json!([]));

    let mut changed = definition(&f, MAIN_AGENT_ID);
    changed["name"] = "Support replies v2".into();
    let rule_path = format!("/api/hook-rules/{id}");
    let (status, stale) = call(
        &f,
        OWNER,
        "PUT",
        &rule_path,
        json!({ "ruleId": id, "revision": "old", "definition": changed }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    let (status, replaced) = call(
        &f,
        OWNER,
        "PUT",
        &rule_path,
        json!({ "ruleId": id, "revision": rule["revision"], "definition": changed }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    assert_eq!(replaced["version"], 2);

    let (_, versions) = call(
        &f,
        OWNER,
        "GET",
        &format!("{rule_path}/versions"),
        Value::Null,
    )
    .await;
    let versions = versions["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["definition"]["name"], "Support replies v2");
    assert_eq!(versions[1]["definition"]["name"], "Support replies");
    assert_eq!(versions[1]["savedByAccountId"], OWNER);

    let (status, disabled) = call(
        &f,
        OWNER,
        "POST",
        &format!("{rule_path}/enabled"),
        json!({ "ruleId": id, "enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(kinds(&disabled), ["disabled"]);
    assert_eq!(disabled["allowed"]["enable"], true);

    let (status, deleted) = call(&f, OWNER, "DELETE", &rule_path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(
        call(&f, OWNER, "GET", &rule_path, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn saving_refuses_trees_formulas_and_rights_that_do_not_hold() {
    let f = Fixture::new().await;
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;

    // The agent does not hold the hook yet.
    let (status, refused) = create(&f, OWNER, definition(&f, MAIN_AGENT_ID)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("does not hold this hook"),
        "{refused}"
    );

    f.set(json!([
        contribution("hook", "message", support()),
        contribution("tool", "grant_probe_post", support()),
    ]))
    .await;
    let mut broken = definition(&f, MAIN_AGENT_ID);
    broken["steps"][0]["condition"]["conditions"][1]["left"]["expression"] = "sender".into();
    broken["steps"][0]["then"][0]["action"]["input"]["unknown"] =
        json!({ "type": "literal", "value": 1 });
    broken["steps"][0]["then"][1]["id"] = "post".into();
    broken["steps"][0]["then"][2]["action"]["seconds"]["value"] = 700_000.into();
    let (status, validated) = call(
        &f,
        OWNER,
        "POST",
        "/api/hook-rules/validate",
        json!({ "definition": broken }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{validated}");
    let problems = validated["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|problem| (text(problem, "path"), text(problem, "message")))
        .collect::<Vec<_>>();
    let expected = [
        "/steps/0/condition/conditions/1/left/expression",
        "/steps/0/then/0/action/input/unknown",
        "/steps/0/then/1/id",
        "/steps/0/then/2/action/seconds",
    ];
    for path in expected {
        assert!(
            problems.iter().any(|(at, _)| *at == path),
            "{path} missing from {problems:?}"
        );
    }
    assert!(
        problems
            .iter()
            .any(|(_, message)| message.contains("Unknown variable `sender`"))
    );
    let (status, _) = create(&f, OWNER, broken).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Later steps are not readable before they run, and loops cannot wait.
    let mut early = definition(&f, MAIN_AGENT_ID);
    early["steps"][0]["then"][0]["action"]["input"]["thread"] =
        json!({ "type": "formula", "expression": "steps.remember" });
    early["steps"].as_array_mut().unwrap().push(json!({
        "type": "forEach",
        "id": "each",
        "items": { "type": "formula", "expression": "payload.items" },
        "itemName": "entry",
        "body": [{
            "type": "action", "id": "later",
            "action": { "type": "wait", "seconds": { "type": "literal", "value": 5 } },
        }],
    }));
    let (_, validated) = call(
        &f,
        OWNER,
        "POST",
        "/api/hook-rules/validate",
        json!({ "definition": early }),
    )
    .await;
    let messages = validated["problems"].to_string();
    assert!(
        messages.contains("No earlier action is named `remember`"),
        "{messages}"
    );
    assert!(messages.contains("A loop cannot wait"), "{messages}");

    // Names follow the contract, authors come from the event, and a rule stays savable.
    let mut named = definition(&f, MAIN_AGENT_ID);
    named["steps"][0]["then"][1]["action"]["name"] = "_tmp".into();
    named["steps"][0]["condition"]["conditions"][0]["author"] =
        json!({ "type": "literal", "value": "alice" });
    let filler = "x".repeat(15_000);
    for index in 0..9 {
        named["steps"][0]["otherwise"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "type": "action",
                "id": format!("fill{index}"),
                "action": {
                    "type": "storeValue", "scope": "rule", "name": format!("fill{index}"),
                    "value": { "type": "literal", "value": filler },
                },
            }));
    }
    let (_, validated) = call(
        &f,
        OWNER,
        "POST",
        "/api/hook-rules/validate",
        json!({ "definition": named }),
    )
    .await;
    let problems = validated["problems"].as_array().unwrap();
    let at = |path: &str| problems.iter().any(|problem| problem["path"] == path);
    assert!(at("/steps/0/then/1/action/name"), "{validated}");
    assert!(at("/steps/0/condition/conditions/0/author"), "{validated}");
    assert!(at(""), "{validated}");
}

#[tokio::test]
async fn members_own_their_rules_and_lose_them_with_their_rights_or_membership() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    common::relay_fixture::add_member(&f.s, OTHER, "membership-other").await;

    // An agent created later is the owner's alone.
    let private =
        f.s.agent_fixture(json!({ "name": "Private" }), None)
            .await
            .unwrap();
    let private = text(&private, "id").to_owned();
    let (status, refused) = create(&f, MEMBER, definition(&f, &private)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("access to this agent")
    );

    let (status, rule) = create(&f, MEMBER, definition(&f, MAIN_AGENT_ID)).await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    let id = text(&rule, "id").to_owned();
    let path = format!("/api/hook-rules/{id}");
    assert_eq!(rule["allowed"]["edit"], true);
    assert_eq!(rule["allowed"]["emergencyPause"], false);

    // Others see it, cannot change it, and may duplicate it with its rights.
    let (_, seen) = call(&f, OTHER, "GET", &path, Value::Null).await;
    assert_eq!(seen["allowed"]["edit"], false);
    assert_eq!(seen["allowed"]["duplicate"], false);
    assert_eq!(seen["allowed"]["viewDetails"], false);
    let (status, _) = call(
        &f,
        OWNER,
        "PUT",
        &path,
        json!({ "ruleId": id, "revision": rule["revision"], "definition": definition(&f, MAIN_AGENT_ID) }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &f,
        OWNER,
        "POST",
        &format!("{path}/enabled"),
        json!({ "ruleId": id, "enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&f, OTHER, "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &f,
        OTHER,
        "POST",
        &format!("{path}/duplicate"),
        json!({ "ruleId": id }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let creation = uuid::Uuid::new_v4().to_string();
    let (status, copy) = call(
        &f,
        OWNER,
        "POST",
        &format!("{path}/duplicate"),
        json!({ "ruleId": id, "creationId": creation }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{copy}");
    assert_eq!(copy["creatorAccountId"], OWNER);
    assert_eq!(copy["source"], "duplicate");
    assert_eq!(kinds(&copy), ["disabled"]);
    let (_, again) = call(
        &f,
        OWNER,
        "POST",
        &format!("{path}/duplicate"),
        json!({ "ruleId": id, "creationId": creation }),
    )
    .await;
    assert_eq!(again["id"], copy["id"]);

    // Only the owner pauses in an emergency, and only the owner resumes.
    let pause = format!("{path}/emergency-pause");
    let (status, _) = call(
        &f,
        MEMBER,
        "POST",
        &pause,
        json!({ "ruleId": id, "paused": true }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, paused) = call(
        &f,
        OWNER,
        "POST",
        &pause,
        json!({ "ruleId": id, "paused": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{paused}");
    assert_eq!(kinds(&paused), ["emergencyPaused"]);
    let (status, _) = call(
        &f,
        MEMBER,
        "POST",
        &pause,
        json!({ "ruleId": id, "paused": false }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, resumed) = call(
        &f,
        OWNER,
        "POST",
        &pause,
        json!({ "ruleId": id, "paused": false }),
    )
    .await;
    assert!(kinds(&resumed).is_empty());

    // Losing access to the execution agent blocks the rule until it returns.
    let access = format!("/api/agents/{MAIN_AGENT_ID}/accounts/{MEMBER}");
    let (status, revoked) = call(&f, OWNER, "DELETE", &access, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    let (_, lost) = call(&f, MEMBER, "GET", &path, Value::Null).await;
    assert_eq!(kinds(&lost), ["rightsLost"]);
    let (status, _) = call(&f, OWNER, "PUT", &access, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, restored) = call(&f, MEMBER, "GET", &path, Value::Null).await;
    assert!(kinds(&restored).is_empty(), "{restored}");

    // The execution agent losing the hook blocks it as well.
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    let (_, lost) = call(&f, MEMBER, "GET", &path, Value::Null).await;
    assert_eq!(kinds(&lost), ["rightsLost"]);
    assert!(
        lost["blocks"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("message")
    );
    f.set(json!([
        contribution("hook", "message", support()),
        contribution("tool", "grant_probe_post", support()),
    ]))
    .await;

    // Leaving ends the rule for good, even when re-invited.
    common::relay_fixture::remove_member(&f.s, MEMBER).await;
    f.s.synchronize_task_authors().await.unwrap();
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    f.s.synchronize_task_authors().await.unwrap();
    let (_, ended) = call(&f, OWNER, "GET", &path, Value::Null).await;
    assert!(kinds(&ended).contains(&"membershipEnded"), "{ended}");
    let (_, current) = call(&f, MEMBER, "GET", &path, Value::Null).await;
    assert_eq!(current["allowed"]["edit"], false);
    let (status, _) = call(
        &f,
        MEMBER,
        "PUT",
        &path,
        json!({ "ruleId": id, "revision": current["revision"], "definition": definition(&f, MAIN_AGENT_ID) }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&f, OWNER, "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn agents_and_public_clients_follow_the_rule_write_limits() {
    let f = granted().await;
    let (_, owned) = create(&f, OWNER, definition(&f, MAIN_AGENT_ID)).await;
    let id = text(&owned, "id").to_owned();

    // An installation agent reads rules, never writes them outside an eligible conversation.
    let listed = f
        .mcp("/mcp-installation", &f.bearer, "list_hook_rules", json!({}))
        .await;
    assert_ne!(listed["result"]["isError"], true, "{listed}");
    assert_eq!(mcp_result(&listed)["rules"][0]["id"], id.as_str());
    for (name, input) in [
        (
            "create_hook_rule",
            json!({ "definition": definition(&f, MAIN_AGENT_ID) }),
        ),
        (
            "set_hook_rule_enabled",
            json!({ "ruleId": id, "enabled": false }),
        ),
        (
            "set_hook_rule_emergency_pause",
            json!({ "ruleId": id, "paused": true }),
        ),
        ("delete_hook_rule", json!({ "ruleId": id })),
        ("duplicate_hook_rule", json!({ "ruleId": id })),
    ] {
        let refused = f.mcp("/mcp-installation", &f.bearer, name, input).await;
        assert_eq!(refused["result"]["isError"], true, "{name}: {refused}");
    }

    // Reducing the hook a rule of a current account uses is refused to agents.
    let current = f.api("GET", json!({})).await.unwrap();
    let reduced = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            json!({
                "agentId": MAIN_AGENT_ID,
                "revision": current["revision"],
                "grants": [{
                    "instanceId": f.instance,
                    "access": {
                        "type": "selected",
                        "contributions": [contribution("tool", "grant_probe_post", support())],
                    },
                }],
                "runCredentialInstances": [],
            }),
        )
        .await;
    assert_eq!(reduced["result"]["isError"], true, "{reduced}");
    let narrowed = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "replace_agent_plugin_grants",
            json!({
                "agentId": MAIN_AGENT_ID,
                "revision": current["revision"],
                "grants": [{
                    "instanceId": f.instance,
                    "access": {
                        "type": "selected",
                        "contributions": [
                            contribution("hook", "message", json!([{ "resourceType": "thread", "id": "support-thread" }])),
                            contribution("tool", "grant_probe_post", support()),
                        ],
                    },
                }],
                "runCredentialInstances": [],
            }),
        )
        .await;
    assert_eq!(narrowed["result"]["isError"], true, "{narrowed}");

    // A public client needs the manage scope, and writes for the owner.
    let reader = common::relay_fixture::grant(&f.s, &["read"]).await;
    let refused = f
        .mcp(
            "/mcp",
            &reader,
            "create_hook_rule",
            json!({ "definition": definition(&f, MAIN_AGENT_ID) }),
        )
        .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    let manager = common::relay_fixture::grant(&f.s, &["read", "manage"]).await;
    let created = f
        .mcp(
            "/mcp",
            &manager,
            "create_hook_rule",
            json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false }),
        )
        .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    let created = mcp_result(&created);
    assert_eq!(created["creatorAccountId"], OWNER);
    assert_eq!(created["source"], "publicMcp");
    assert_eq!(kinds(created), ["disabled"]);
    let catalog = f
        .mcp("/mcp", &reader, "get_hook_rule_catalog", json!({}))
        .await;
    assert_ne!(catalog["result"]["isError"], true, "{catalog}");
}

#[tokio::test]
async fn the_catalog_offers_what_usable_agents_hold() {
    let f = granted().await;
    common::relay_fixture::add_member(&f.s, MEMBER, "membership-member").await;

    let (status, catalog) = call(&f, OWNER, "GET", "/api/hook-rules/catalog", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    let plugin_hooks = catalog["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|hook| hook["trigger"]["type"] == "plugin")
        .collect::<Vec<_>>();
    assert_eq!(plugin_hooks.len(), 1);
    assert_eq!(plugin_hooks[0]["trigger"]["hook"], "message");
    assert_eq!(plugin_hooks[0]["agentIds"], json!([MAIN_AGENT_ID]));
    assert_eq!(plugin_hooks[0]["payloadSchema"]["type"], "object");
    let cairn_hooks = catalog["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|hook| hook["trigger"]["type"] == "cairn")
        .count();
    assert_eq!(cairn_hooks, 4);
    assert_eq!(catalog["tools"][0]["tool"], "grant_probe_post");
    assert_eq!(catalog["actions"].as_array().unwrap().len(), 7);
    assert!(
        catalog["functions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|function| function["name"] == "truncate")
    );
    assert_eq!(catalog["limits"]["maxWaitSeconds"], 604_800);

    // A member without access to any agent is offered none of them.
    let (_, offered) = call(&f, MEMBER, "GET", "/api/hook-rules/catalog", Value::Null).await;
    assert_eq!(offered["agents"], json!([]));
    assert_eq!(offered["tools"], json!([]));
}

/// A conversation the person creates and writes from the app, whose turn their message
/// launched. Returns the conversation and its installation bearer.
async fn person_conversation(f: &Fixture, account: &str) -> (String, String) {
    let (status, chat) = call(
        f,
        account,
        "POST",
        "/api/chats",
        json!({ "agentId": MAIN_AGENT_ID }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{chat}");
    let chat = text(&chat, "id").to_owned();
    let (status, sent) = call(
        f,
        account,
        "POST",
        &format!("/api/chats/{chat}/messages"),
        json!({ "id": cairn_installation::config::id(), "text": "Write my support rule." }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    let bearer = fixture::running_conversation(&f.s, &chat).await;
    (chat, bearer)
}

async fn agent(f: &Fixture, bearer: &str, name: &str, input: Value) -> Value {
    f.mcp("/mcp-installation", bearer, name, input).await
}

fn refused(response: &Value) -> bool {
    response["result"]["isError"] == true
}

#[track_caller]
fn refused_because(response: &Value, reason: &str) {
    let message = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        refused(response) && message.contains(reason),
        "expected a refusal with {reason:?}: {response}"
    );
}

#[tokio::test]
async fn an_agent_writes_rules_only_for_the_person_of_its_eligible_conversation() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let (chat, bearer) = person_conversation(&f, OWNER).await;

    // The rule belongs to the person; its version records the agent and conversation.
    let created = agent(
        &f,
        &bearer,
        "create_hook_rule",
        json!({ "definition": definition(&f, MAIN_AGENT_ID) }),
    )
    .await;
    assert!(!refused(&created), "{created}");
    let rule = mcp_result(&created).clone();
    assert_eq!(rule["creatorAccountId"], OWNER);
    assert_eq!(rule["source"], "installationAgent");
    assert_eq!(rule["conversationId"], chat.as_str());
    assert_eq!(rule["nativeAuthority"], false);
    let id = text(&rule, "id").to_owned();
    let versions = agent(
        &f,
        &bearer,
        "list_hook_rule_versions",
        json!({ "ruleId": id }),
    )
    .await;
    let version = &mcp_result(&versions)["versions"][0];
    assert_eq!(version["savedByAccountId"], OWNER);
    assert_eq!(version["source"], "installationAgent");
    assert_eq!(version["conversationId"], chat.as_str());

    // It replaces, disables, enables and duplicates the person's rules.
    let replaced = agent(
        &f,
        &bearer,
        "replace_hook_rule",
        json!({ "ruleId": id, "revision": rule["revision"], "definition": definition(&f, MAIN_AGENT_ID) }),
    )
    .await;
    assert!(!refused(&replaced), "{replaced}");
    for enabled in [false, true] {
        let changed = agent(
            &f,
            &bearer,
            "set_hook_rule_enabled",
            json!({ "ruleId": id, "enabled": enabled }),
        )
        .await;
        assert!(!refused(&changed), "{changed}");
    }
    let copy = agent(&f, &bearer, "duplicate_hook_rule", json!({ "ruleId": id })).await;
    assert!(!refused(&copy), "{copy}");
    assert_eq!(mcp_result(&copy)["creatorAccountId"], OWNER);
    assert_eq!(mcp_result(&copy)["source"], "installationAgent");
    assert_eq!(mcp_result(&copy)["conversationId"], chat.as_str());

    // Never another account's rule, nor an emergency pause or a budget.
    let (_, theirs) = create(&f, MEMBER, definition(&f, MAIN_AGENT_ID)).await;
    let theirs = text(&theirs, "id").to_owned();
    let duplicated = agent(
        &f,
        &bearer,
        "duplicate_hook_rule",
        json!({ "ruleId": theirs }),
    )
    .await;
    refused_because(&duplicated, "duplicates only rules of the person");
    for (name, input) in [
        (
            "set_hook_rule_enabled",
            json!({ "ruleId": theirs, "enabled": false }),
        ),
        ("delete_hook_rule", json!({ "ruleId": theirs })),
        (
            "set_hook_rule_emergency_pause",
            json!({ "ruleId": id, "paused": true }),
        ),
        ("lift_hook_rule_budget", json!({ "ruleId": id })),
    ] {
        let response = agent(&f, &bearer, name, input).await;
        assert!(refused(&response), "{name}: {response}");
    }

    // While a rule of the person is paused, the agent only disables their rules.
    let (status, _) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/hook-rules/{id}/emergency-pause"),
        json!({ "ruleId": id, "paused": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let copy_id = text(mcp_result(&copy), "id").to_owned();
    for (name, input) in [
        (
            "create_hook_rule",
            json!({ "definition": definition(&f, MAIN_AGENT_ID) }),
        ),
        ("duplicate_hook_rule", json!({ "ruleId": id })),
        ("duplicate_hook_rule", json!({ "ruleId": copy_id })),
        (
            "set_hook_rule_enabled",
            json!({ "ruleId": copy_id, "enabled": true }),
        ),
        ("delete_hook_rule", json!({ "ruleId": copy_id })),
    ] {
        let response = agent(&f, &bearer, name, input).await;
        assert!(refused(&response), "{name}: {response}");
    }
    let disabled = agent(
        &f,
        &bearer,
        "set_hook_rule_enabled",
        json!({ "ruleId": id, "enabled": false }),
    )
    .await;
    assert!(!refused(&disabled), "{disabled}");

    // Once the owner resumes it, the agent may delete the person's rule.
    let (status, _) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/hook-rules/{id}/emergency-pause"),
        json!({ "ruleId": id, "paused": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deleted = agent(
        &f,
        &bearer,
        "delete_hook_rule",
        json!({ "ruleId": copy_id }),
    )
    .await;
    assert!(!refused(&deleted), "{deleted}");
}

#[tokio::test]
async fn a_member_loses_agent_rule_writes_with_their_access() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let (_, bearer) = person_conversation(&f, MEMBER).await;
    let created = agent(
        &f,
        &bearer,
        "create_hook_rule",
        json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false }),
    )
    .await;
    assert!(!refused(&created), "{created}");
    assert_eq!(mcp_result(&created)["creatorAccountId"], MEMBER);

    f.s.store
        .transaction(|db| {
            db.0.execute(
                "DELETE FROM agent_account_access WHERE account_id=?",
                [MEMBER],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let created = agent(
        &f,
        &bearer,
        "create_hook_rule",
        json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false }),
    )
    .await;
    refused_because(&created, "no longer has access to this agent");
}

#[tokio::test]
async fn any_other_writer_or_unrecorded_write_closes_the_conversation_for_good() {
    let f = granted().await;
    let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });

    // Tasks and conversations nobody recorded as a person's never qualify.
    refused_because(
        &agent(&f, &f.bearer, "create_hook_rule", write.clone()).await,
        "not a conversation",
    );
    let unrecorded =
        f.s.chat_create(json!({ "agentId": MAIN_AGENT_ID }))
            .await
            .unwrap();
    let unrecorded = text(&unrecorded, "id").to_owned();
    f.s.chat_send(
        &unrecorded,
        json!({ "id": cairn_installation::config::id(), "text": "No person" }),
    )
    .await
    .unwrap();
    let token = fixture::running_conversation(&f.s, &unrecorded).await;
    refused_because(
        &agent(&f, &token, "create_hook_rule", write.clone()).await,
        "did not record that a person started this conversation",
    );

    // A write path that skips the record leaves the message unattested.
    let (chat, bearer) = person_conversation(&f, OWNER).await;
    assert!(!refused(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await
    ));
    let tampered = chat.clone();
    f.s.store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE chat_messages SET text='Rewritten elsewhere' WHERE chat_id=?",
                [tampered],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
        "was not written by its person",
    );

    // An agent entry, even into its own conversation.
    let (chat, bearer) = person_conversation(&f, OWNER).await;
    let sent = agent(
        &f,
        &bearer,
        "send_chat_message",
        json!({ "chatId": chat, "message": { "id": cairn_installation::config::id(), "text": "From the agent" } }),
    )
    .await;
    assert!(!refused(&sent), "{sent}");
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
        "Another writer reached this conversation.",
    );

    // Reading another conversation reaches this one.
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let read = agent(
        &f,
        &bearer,
        "get_chat",
        json!({ "chatId": unrecorded, "limit": 10 }),
    )
    .await;
    assert!(!refused(&read), "{read}");
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
        "Another writer reached this conversation.",
    );

    // So does any operation that may return another conversation, run or task.
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let runs = agent(&f, &bearer, "list_runs", json!({})).await;
    assert!(!refused(&runs), "{runs}");
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
        "Another writer reached this conversation.",
    );

    // Withdrawing the person's queued message is an entry of whoever withdraws it.
    let (chat, bearer) = person_conversation(&f, OWNER).await;
    let queued = cairn_installation::config::id();
    let (status, _) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/chats/{chat}/messages"),
        json!({ "id": queued, "text": "Do not create that rule." }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let withdrawn = agent(
        &f,
        &bearer,
        "delete_chat_message",
        json!({ "chatId": chat, "messageId": queued }),
    )
    .await;
    assert!(!refused(&withdrawn), "{withdrawn}");
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
        "Another writer reached this conversation.",
    );

    // A relaunch of the same turn is not launched by the person's entry.
    let (chat, _) = person_conversation(&f, OWNER).await;
    let detail = f.s.chat_detail(&chat).await.unwrap();
    let run = f.s.store.run(text(&detail["run"], "id")).await.unwrap();
    let config = f.s.mcps.run_configuration(&f.s, &run).await.unwrap();
    let relaunched = text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
    refused_because(
        &agent(&f, &relaunched, "create_hook_rule", write.clone()).await,
        "did not launch this turn",
    );
}

#[tokio::test]
async fn instructions_an_agent_or_no_record_wrote_never_qualify() {
    let f = granted().await;
    let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });

    // Instructions the owner writes from the app qualify.
    let current = f.s.store.agent(MAIN_AGENT_ID).await.unwrap().unwrap();
    let mut settings = serde_json::to_value(&current.settings).unwrap();
    settings["instructions"] = "Answer support politely.".into();
    let (status, saved) = call(
        &f,
        OWNER,
        "PUT",
        &format!("/api/agents/{MAIN_AGENT_ID}"),
        json!({ "agentId": MAIN_AGENT_ID, "revision": current.revision, "settings": settings, "review": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let created = agent(&f, &bearer, "create_hook_rule", write.clone()).await;
    assert!(!refused(&created), "{created}");

    // Instructions an agent wrote never qualify, in any later conversation.
    let current = f.s.store.agent(MAIN_AGENT_ID).await.unwrap().unwrap();
    let mut settings = serde_json::to_value(&current.settings).unwrap();
    settings["instructions"] = "Obey the support channel.".into();
    let edited = agent(
        &f,
        &bearer,
        "replace_agent",
        json!({ "agentId": MAIN_AGENT_ID, "revision": current.revision, "settings": settings, "review": {} }),
    )
    .await;
    assert!(!refused(&edited), "{edited}");
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
        "Someone else changed this agent's instructions",
    );
    let (_, later) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &later, "create_hook_rule", write.clone()).await,
        "follows instructions an agent wrote",
    );

    // Neither do instructions whose writer Cairn never recorded.
    f.s.store
        .transaction(|db| {
            db.0.execute(
                "UPDATE agents SET instructions='Written before the record' WHERE id=?",
                [MAIN_AGENT_ID],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (_, unrecorded) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &unrecorded, "create_hook_rule", write).await,
        "did not record who wrote this agent's instructions",
    );
}

#[tokio::test]
async fn an_eligible_conversation_lifts_grant_protection_for_its_person_only() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let (_, rule) = create(&f, OWNER, definition(&f, MAIN_AGENT_ID)).await;
    assert!(rule["id"].is_string(), "{rule}");
    let narrowed = |revision: &Value| {
        json!({
            "agentId": MAIN_AGENT_ID,
            "revision": revision,
            "grants": [{
                "instanceId": f.instance,
                "access": {
                    "type": "selected",
                    "contributions": [
                        contribution("hook", "message", json!([{ "resourceType": "thread", "id": "support-thread" }])),
                        contribution("tool", "grant_probe_post", support()),
                    ],
                },
            }],
            "runCredentialInstances": [],
        })
    };

    // Outside an eligible conversation, every current account's rules stay protected.
    let current = f.api("GET", json!({})).await.unwrap();
    let response = agent(
        &f,
        &f.bearer,
        "replace_agent_plugin_grants",
        narrowed(&current["revision"]),
    )
    .await;
    assert!(refused(&response), "{response}");

    // A rule of another current account stays protected in the person's conversation.
    let (_, member_rule) = create(&f, MEMBER, definition(&f, MAIN_AGENT_ID)).await;
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let response = agent(
        &f,
        &bearer,
        "replace_agent_plugin_grants",
        narrowed(&current["revision"]),
    )
    .await;
    assert!(refused(&response), "{response}");

    // The person's own rules no longer protect what the agent reduces for them.
    let path = format!("/api/hook-rules/{}", text(&member_rule, "id"));
    let (status, _) = call(&f, MEMBER, "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let response = agent(
        &f,
        &bearer,
        "replace_agent_plugin_grants",
        narrowed(&current["revision"]),
    )
    .await;
    assert!(!refused(&response), "{response}");
}

#[tokio::test]
async fn the_person_is_warned_of_an_agent_rule_open_to_anyone() {
    let f = granted().await;
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let open = json!({
        "name": "Open bot",
        "trigger": { "type": "plugin", "instanceId": f.instance, "hook": "message" },
        "executionAgentId": MAIN_AGENT_ID,
        "steps": [{
            "type": "action",
            "id": "start",
            "action": {
                "type": "startAgent",
                "agentId": MAIN_AGENT_ID,
                "prompt": { "type": "formula", "expression": "truncate(payload.author, 40)" },
            },
        }],
        "limits": { "triggeringsPerHour": 30, "conversationsPerHour": 5 },
    });
    let created = agent(
        &f,
        &bearer,
        "create_hook_rule",
        json!({ "definition": open, "enabled": false }),
    )
    .await;
    assert!(!refused(&created), "{created}");
    assert_eq!(mcp_result(&created)["warnings"][0]["kind"], "openToAnyone");
    let alerts: Vec<Value> =
        f.s.store
            .read(|db| {
                let mut statement =
                    db.0.prepare("SELECT data FROM kv WHERE key LIKE 'push-outbox:%'")?;
                let rows = statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows
                    .iter()
                    .map(|row| serde_json::from_str(row).unwrap())
                    .collect())
            })
            .await
            .unwrap();
    let alert = alerts
        .iter()
        .find(|alert| alert["kind"] == "alert")
        .unwrap_or_else(|| panic!("{alerts:?}"));
    assert_eq!(alert["members"], json!([OWNER]));
}
