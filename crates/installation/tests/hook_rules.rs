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
use common::account_fixture::AccountFixtures;
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
async fn clock_rules_run_now_through_public_and_eligible_installation_tools() {
    let f = granted().await;
    let manager = common::relay_fixture::grant(&f.s, &["read", "manage"]).await;
    let runner = common::relay_fixture::grant(&f.s, &["read", "run"]).await;
    let definition = json!({
        "name": "Check weekly notes",
        "trigger": { "type": "clock", "cron": "0 9 * * 1", "timezone": "UTC" },
        "executionAgentId": MAIN_AGENT_ID,
        "limits": cairn_contracts::HookRuleLimits::default(),
        "steps": [{
            "type": "action",
            "id": "start",
            "action": {
                "type": "startAgent",
                "agentId": MAIN_AGENT_ID,
                "prompt": { "type": "literal", "value": "Review the notes." },
            },
        }],
    });
    let created = f
        .mcp(
            "/mcp",
            &manager,
            "create_hook_rule",
            json!({ "definition": definition }),
        )
        .await;
    assert!(!refused(&created), "{created}");
    let rule = mcp_result(&created);
    assert_eq!(rule["creatorAccountId"], OWNER);
    let rule_id = text(rule, "id");
    let command = json!({ "ruleId": rule_id });

    let missing_scope = f
        .mcp("/mcp", &manager, "run_hook_rule", command.clone())
        .await;
    assert!(refused(&missing_scope), "{missing_scope}");
    assert!(refused(
        &agent(&f, &f.bearer, "run_hook_rule", command.clone()).await
    ));

    let public = f
        .mcp("/mcp", &runner, "run_hook_rule", command.clone())
        .await;
    assert!(!refused(&public), "{public}");
    assert_eq!(mcp_result(&public)["ruleId"], rule_id);
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let installation = agent(&f, &bearer, "run_hook_rule", command).await;
    assert!(!refused(&installation), "{installation}");
    assert_eq!(mcp_result(&installation)["ruleId"], rule_id);

    cairn_installation::rules::engine::tick(&f.s).await.unwrap();
    let (_, chats) = call(&f, OWNER, "GET", "/api/chats", Value::Null).await;
    assert_eq!(
        chats
            .as_array()
            .unwrap()
            .iter()
            .filter(|chat| {
                chat["originRule"]["ruleId"] == rule_id && chat["originRule"]["schedule"] == true
            })
            .count(),
        2
    );
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

async fn person_instructions(f: &Fixture, instructions: &str) {
    let current = f.s.store.agent(MAIN_AGENT_ID).await.unwrap().unwrap();
    let mut settings = serde_json::to_value(&current.settings).unwrap();
    settings["instructions"] = instructions.into();
    let (status, saved) = call(f, OWNER, "PUT", &format!("/api/agents/{MAIN_AGENT_ID}"),
        json!({ "agentId": MAIN_AGENT_ID, "revision": current.revision, "settings": settings, "review": {} }),
    ).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

async fn native_configuration(
    f: &Fixture,
    provider: &str,
    agent_id: Option<&str>,
    document: &str,
    bearer: Option<&str>,
) -> Value {
    let (status, current) = call(
        f,
        OWNER,
        "POST",
        "/api/settings/coding-agents/read",
        json!({
            "provider": provider,
            "agentId": agent_id,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{current}");
    let document = if provider == "opencode" && agent_id.is_some() {
        let mut document: Value = serde_json::from_str(document).unwrap();
        let inherited: Value = serde_json::from_str(text(&current, "document")).unwrap();
        if document.get("model").is_none() {
            document["model"] = inherited["model"].clone();
        }
        document.to_string()
    } else {
        document.to_owned()
    };
    let input = json!({
        "provider": provider,
        "agentId": agent_id,
        "revision": current["revision"],
        "document": document,
        "changes": [],
        "reset": false,
    });
    if let Some(bearer) = bearer {
        let saved = agent(f, bearer, "save_coding_agent_configuration", input).await;
        assert!(!refused(&saved), "{saved}");
        mcp_result(&saved).clone()
    } else {
        let (status, saved) = call(f, OWNER, "PUT", "/api/settings/coding-agents", input).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        saved
    }
}

async fn native_instructions(f: &Fixture, instructions: &str, bearer: Option<&str>) {
    native_configuration(
        f,
        "codex",
        None,
        &format!(
            "developer_instructions = {}\n",
            serde_json::to_string(instructions).unwrap()
        ),
        bearer,
    )
    .await;
}

async fn person_provider(f: &Fixture, provider: &str, instructions: &str) {
    let current = f.s.store.agent(MAIN_AGENT_ID).await.unwrap().unwrap();
    let mut settings = serde_json::to_value(&current.settings).unwrap();
    settings["provider"] = provider.into();
    settings["model"] = if provider == "opencode" {
        "opencode/ling-3.1-flash-free"
    } else {
        ""
    }
    .into();
    settings["reasoning"] = "".into();
    settings["speed"] = "".into();
    settings["instructions"] = instructions.into();
    let (status, saved) = call(
        f,
        OWNER,
        "PUT",
        &format!("/api/agents/{MAIN_AGENT_ID}"),
        json!({
            "agentId": MAIN_AGENT_ID,
            "revision": current.revision,
            "settings": settings,
            "review": {},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

async fn next_person_turn(f: &Fixture, chat: &str) -> String {
    let detail = f.s.chat_detail(chat).await.unwrap();
    f.s.store
        .change_run(
            text(&detail["run"], "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Succeeded),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (status, sent) = call(
        f,
        OWNER,
        "POST",
        &format!("/api/chats/{chat}/messages"),
        json!({
            "id": cairn_installation::config::id(),
            "text": "Write my next rule.",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    fixture::running_conversation(&f.s, chat).await
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

async fn replacing_a_queued_message_preserves_its_unknown_writer(withdraw: bool) {
    for prior in ["attested", "changed", "missing"] {
        let f = granted().await;
        let (chat, bearer) = person_conversation(&f, OWNER).await;
        let queued = cairn_installation::config::id();
        let path = format!("/api/chats/{chat}/messages");
        let (status, sent) = call(
            &f,
            OWNER,
            "POST",
            &path,
            json!({ "id": queued, "text": "Original human request" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{sent}");
        let message = queued.clone();
        f.s.store
            .transaction(move |db| {
                match prior {
                    "changed" => {
                        db.0.execute(
                            "UPDATE chat_messages SET text='Unrecorded rewrite' WHERE id=?",
                            [&message],
                        )?;
                    }
                    "missing" => {
                        db.0.execute(
                            "DELETE FROM conversation_entry_writers WHERE message_id=?",
                            [&message],
                        )?;
                    }
                    _ => {}
                }
                Ok(())
            })
            .await
            .unwrap();
        let path = format!("{path}/{queued}");
        let (status, edited) = call(
            &f,
            OWNER,
            if withdraw { "DELETE" } else { "PUT" },
            &path,
            if withdraw {
                Value::Null
            } else {
                json!({ "text": "Person's correction" })
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        if !withdraw {
            assert_eq!(edited["text"], "Person's correction");
        }
        let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
        let created = agent(&f, &bearer, "create_hook_rule", write.clone()).await;
        if prior == "attested" {
            assert!(!refused(&created), "{created}");
            continue;
        }
        refused_because(&created, "Another writer reached this conversation");
        if !withdraw {
            let (status, removed) = call(&f, OWNER, "DELETE", &path, Value::Null).await;
            assert_eq!(status, StatusCode::OK, "{removed}");
        }
        finish_turn(&f, &chat).await;
        let later = next_person_turn(&f, &chat).await;
        refused_because(
            &agent(&f, &later, "create_hook_rule", write).await,
            "Another writer reached this conversation",
        );
    }
}

#[tokio::test]
async fn editing_a_message_never_attests_its_unknown_previous_revision() {
    replacing_a_queued_message_preserves_its_unknown_writer(false).await;
}

#[tokio::test]
async fn withdrawing_a_message_never_erases_its_unknown_previous_revision() {
    replacing_a_queued_message_preserves_its_unknown_writer(true).await;
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
        "Another writer reached this conversation",
    );
    let (_, later) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &later, "create_hook_rule", write.clone()).await,
        "follows instructions an agent wrote",
    );

    // A later human rewrite never erases earlier agent authorship of this text.
    person_instructions(&f, "Obey the support channel!").await;
    let (_, edited) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &edited, "create_hook_rule", write.clone()).await,
        "follows instructions an agent wrote",
    );
    person_instructions(&f, "Obey the support channel.").await;
    let (_, rewritten) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &rewritten, "create_hook_rule", write.clone()).await,
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
async fn inherited_instructions_keep_their_own_writer_and_history() {
    let f = granted().await;
    native_instructions(&f, "Installation instructions", None).await;
    let (chat, bearer) = person_conversation(&f, OWNER).await;
    let detail = f.s.chat_detail(&chat).await.unwrap();
    let run = f.s.store.run(text(&detail["run"], "id")).await.unwrap();
    assert_eq!(
        run["snapshot"]["agent"]["instructions"],
        "Installation instructions"
    );
    let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
    let created = agent(&f, &bearer, "create_hook_rule", write.clone()).await;
    assert!(!refused(&created), "{created}");

    native_instructions(&f, "Agent changed the defaults", Some(&f.bearer)).await;
    native_instructions(&f, "Installation instructions", Some(&f.bearer)).await;
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
        "Another writer reached this conversation",
    );
    person_instructions(&f, "A human replacement").await;
    person_instructions(&f, "").await;
    native_instructions(&f, "A human default", None).await;
    native_instructions(&f, "Installation instructions", None).await;
    let (_, later) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &later, "create_hook_rule", write).await,
        "follows instructions an agent wrote",
    );
}

#[tokio::test]
async fn unrecorded_defaults_cannot_borrow_a_profiles_instruction_writer() {
    let f = granted().await;
    person_instructions(&f, "The same instruction text").await;
    person_instructions(&f, "").await;
    native_instructions(&f, "The same instruction text", None).await;
    // Installation defaults written before provenance have no native writer record.
    f.s.store
        .transaction(|db| {
            db.0.execute("DELETE FROM native_instruction_writers", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let (_, bearer) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(
            &f,
            &bearer,
            "create_hook_rule",
            json!({ "definition": definition(&f, MAIN_AGENT_ID) }),
        )
        .await,
        "did not record who wrote this agent's instructions",
    );
}

#[tokio::test]
async fn removing_inherited_instructions_permanently_closes_existing_conversations() {
    let f = granted().await;
    native_instructions(&f, "Human defaults", None).await;
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let target = json!({
        "provider": "codex",
        "agentId": MAIN_AGENT_ID,
    });
    let (_, current) = call(
        &f,
        OWNER,
        "POST",
        "/api/settings/coding-agents/read",
        target.clone(),
    )
    .await;
    let changed = agent(
        &f,
        &f.bearer,
        "save_coding_agent_configuration",
        json!({
            "provider": "codex",
            "agentId": MAIN_AGENT_ID,
            "revision": current["revision"],
            "document": "",
            "changes": [],
            "reset": false,
        }),
    )
    .await;
    assert!(!refused(&changed), "{changed}");
    assert!(
        !mcp_result(&changed)["document"]
            .as_str()
            .unwrap()
            .contains("Human defaults")
    );
    let (_, current) = call(
        &f,
        OWNER,
        "POST",
        "/api/settings/coding-agents/read",
        target,
    )
    .await;
    let (status, restored) = call(
        &f,
        OWNER,
        "PUT",
        "/api/settings/coding-agents",
        json!({
            "provider": "codex",
            "agentId": MAIN_AGENT_ID,
            "revision": current["revision"],
            "document": current["document"],
            "changes": [],
            "reset": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    refused_because(
        &agent(
            &f,
            &bearer,
            "create_hook_rule",
            json!({
                "definition": definition(&f, MAIN_AGENT_ID),
            }),
        )
        .await,
        "Another writer reached this conversation",
    );
}

fn skill_content(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: A support skill\n---\n{body}\n")
}

#[tokio::test]
async fn unrecorded_skill_replacements_during_save_are_never_attested() {
    for replace in [false, true] {
        let f = granted().await;
        let root = f.s.skills.root(None).await.unwrap();
        let file = root.join("raced/SKILL.md");
        let replacement = skill_content("raced", "Unrecorded replacement instructions");
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if replace {
            let observed = observed.clone();
            let replacement = replacement.clone();
            f.s.store
                .transaction(move |db| {
                    db.0.create_scalar_function(
                        "replace_published_skill",
                        0,
                        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
                        move |_| -> rusqlite::Result<i64> {
                            // The audit occurs after publication and before the
                            // old provenance handler rereads the released bundle.
                            std::fs::write(&file, &replacement).unwrap();
                            observed.store(true, std::sync::atomic::Ordering::SeqCst);
                            Ok(1)
                        },
                    )?;
                    db.0.execute_batch(
                        "CREATE TRIGGER replace_published_skill AFTER INSERT ON audit
                         WHEN NEW.action='skill.saved' BEGIN SELECT replace_published_skill(); END;",
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        let (status, saved) = call(
            &f,
            OWNER,
            "PUT",
            "/api/skills/global/raced",
            json!({ "content": skill_content("raced", "Human instructions") }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(observed.load(std::sync::atomic::Ordering::SeqCst), replace);
        let (chat, bearer) = person_conversation(&f, OWNER).await;
        let run = f.s.chat_detail(&chat).await.unwrap()["run"].clone();
        assert_eq!(run["snapshot"]["skills"][0]["name"], "raced");
        if replace {
            assert_eq!(
                tokio::fs::read_to_string(root.join("raced/SKILL.md"))
                    .await
                    .unwrap(),
                replacement
            );
        }
        let created = agent(
            &f,
            &bearer,
            "create_hook_rule",
            json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false }),
        )
        .await;
        if replace {
            refused_because(&created, "did not record who wrote the skill raced");
        } else {
            assert!(!refused(&created), "{created}");
        }
    }
}

#[tokio::test]
async fn deleting_a_changed_skill_never_restores_conversation_eligibility() {
    let f = granted().await;
    let (status, saved) = call(
        &f,
        OWNER,
        "PUT",
        "/api/skills/global/support",
        json!({
            "content": skill_content("support", "Human instructions"),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (chat, bearer) = person_conversation(&f, OWNER).await;
    let write = json!({
        "definition": definition(&f, MAIN_AGENT_ID),
        "enabled": false,
    });
    let created = agent(&f, &bearer, "create_hook_rule", write.clone()).await;
    assert!(!refused(&created), "{created}");
    let detail = f.s.chat_detail(&chat).await.unwrap();
    let run_id = text(&detail["run"], "id").to_owned();
    let before = f.s.store.run(&run_id).await.unwrap();
    assert_eq!(before["snapshot"]["skills"][0]["name"], "support");
    let edited = agent(
        &f,
        &f.bearer,
        "save_skill",
        json!({
            "scope": "global",
            "name": "support",
            "content": skill_content("support","Agent instructions"),
        }),
    )
    .await;
    assert!(!refused(&edited), "{edited}");
    let (status, deleted) = call(
        &f,
        OWNER,
        "DELETE",
        "/api/skills/global/support",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    f.s.store
        .change_run(
            &run_id,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Succeeded),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (status, sent) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/chats/{chat}/messages"),
        json!({
            "id": cairn_installation::config::id(),
            "text": "Write my next rule.",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    let bearer = fixture::running_conversation(&f.s, &chat).await;
    let after = f.s.store.run(&run_id).await.unwrap();
    assert_eq!(after["snapshot"]["skills"], json!([]));
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write).await,
        "Someone else changed the skill support",
    );
}

#[tokio::test]
async fn skill_reads_track_unselected_content_and_allow_recorded_human_skills() {
    for (index, (operation, extra)) in [
        (
            "read_skill",
            json!({
                "path": "SKILL.md",
            }),
        ),
        ("read_skill", json!({})),
        ("export_skill_archive", json!({})),
        ("list_skills", json!({})),
    ]
    .into_iter()
    .enumerate()
    {
        let f = granted().await;
        let (_, bearer) = person_conversation(&f, OWNER).await;
        let name = format!("unsafe-{index}");
        let saved = agent(
            &f,
            &bearer,
            "save_skill",
            json!({
                "scope": "global",
                "name": name,
                "content": skill_content(&name,"Agent instructions"),
            }),
        )
        .await;
        assert!(!refused(&saved), "{saved}");
        let write = json!({
            "definition": definition(&f, MAIN_AGENT_ID),
            "enabled": false,
        });
        let before = agent(&f, &bearer, "create_hook_rule", write.clone()).await;
        assert!(!refused(&before), "{before}");
        let mut input = json!({
            "scope": "global",
            "name": name,
        });
        if operation == "list_skills" {
            input = json!({
                "scope": "global",
            });
        }
        for (key, value) in extra.as_object().unwrap() {
            input[key] = value.clone();
        }
        let read = agent(&f, &bearer, operation, input).await;
        assert!(!refused(&read), "{operation}: {read}");
        refused_because(
            &agent(&f, &bearer, "create_hook_rule", write).await,
            "Another writer reached this conversation",
        );
    }
    let f = granted().await;
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let (status, saved) = call(
        &f,
        OWNER,
        "PUT",
        "/api/skills/global/human",
        json!({
            "content": skill_content("human","Human instructions"),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let read = agent(
        &f,
        &bearer,
        "read_skill",
        json!({
            "scope": "global",
            "name": "human",
            "path": "SKILL.md",
        }),
    )
    .await;
    assert!(!refused(&read), "{read}");
    let written = agent(
        &f,
        &bearer,
        "create_hook_rule",
        json!({
            "definition": definition(&f, MAIN_AGENT_ID),
            "enabled": false,
        }),
    )
    .await;
    assert!(!refused(&written), "{written}");
    f.s.skills
        .save(
            "unrecorded",
            &skill_content("unrecorded", "Unknown instructions"),
            None,
        )
        .await
        .unwrap();
    let read = agent(
        &f,
        &bearer,
        "read_skill",
        json!({
            "scope": "global",
            "name": "unrecorded",
            "path": "SKILL.md",
        }),
    )
    .await;
    assert!(!refused(&read), "{read}");
    refused_because(
        &agent(
            &f,
            &bearer,
            "create_hook_rule",
            json!({
                "definition": definition(&f, MAIN_AGENT_ID),
            }),
        )
        .await,
        "Another writer reached this conversation",
    );
}

#[tokio::test]
async fn later_human_instruction_writes_never_attest_an_earlier_unknown_turn() {
    for native in [false, true] {
        let f = granted().await;
        if native {
            native_instructions(&f, "Earlier instructions", None).await;
            f.s.store
                .transaction(|db| {
                    db.0.execute("DELETE FROM native_instruction_writers", [])?;
                    Ok(())
                })
                .await
                .unwrap();
        } else {
            f.s.store
                .transaction(|db| {
                    db.0.execute(
                        "UPDATE agents SET instructions='Earlier instructions' WHERE id=?",
                        [MAIN_AGENT_ID],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        let (_, bearer) = person_conversation(&f, OWNER).await;
        for instructions in ["Human replacement", "Earlier instructions"] {
            if native {
                native_instructions(&f, instructions, None).await;
            } else {
                person_instructions(&f, instructions).await;
            }
        }
        let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
        refused_because(
            &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
            "Another writer reached this conversation",
        );
        let (_, fresh) = person_conversation(&f, OWNER).await;
        let created = agent(&f, &fresh, "create_hook_rule", write).await;
        refused_because(
            &created,
            "did not record who wrote this agent's instructions",
        );
    }
}

#[tokio::test]
async fn later_human_skill_writes_never_attest_an_earlier_unknown_turn() {
    let f = granted().await;
    f.s.skills
        .save(
            "earlier",
            &skill_content("earlier", "Earlier instructions"),
            None,
        )
        .await
        .unwrap();
    let (_, bearer) = person_conversation(&f, OWNER).await;
    for instructions in ["Human replacement", "Earlier instructions"] {
        let (status, saved) = call(
            &f,
            OWNER,
            "PUT",
            "/api/skills/global/earlier",
            json!({
                "content": skill_content("earlier", instructions),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
    }
    let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
        "Someone else changed the skill earlier",
    );
    let (_, fresh) = person_conversation(&f, OWNER).await;
    let created = agent(&f, &fresh, "create_hook_rule", write).await;
    refused_because(&created, "did not record who wrote the skill earlier");
}

#[tokio::test]
async fn native_opencode_prompts_have_their_own_permanent_writer_history() {
    for (scope, document) in [
        (
            None,
            json!({ "agent": { "build": { "prompt": "Agent prompt" } } }),
        ),
        (
            Some(MAIN_AGENT_ID),
            json!({ "mode": { "build": { "prompt": "Agent prompt" } } }),
        ),
        (
            None,
            json!({ "agents": { "build": { "system": "Agent prompt" } } }),
        ),
    ] {
        let f = granted().await;
        person_provider(&f, "opencode", "Human profile instructions").await;
        native_configuration(
            &f,
            "opencode",
            scope,
            &document.to_string(),
            Some(&f.bearer),
        )
        .await;
        let (_, bearer) = person_conversation(&f, OWNER).await;
        refused_because(
            &agent(
                &f,
                &bearer,
                "create_hook_rule",
                json!({
                    "definition": definition(&f, MAIN_AGENT_ID),
                }),
            )
            .await,
            "follows instructions an agent wrote",
        );
        let human =
            json!({ "agent": { "build": { "prompt": "Agent prompt with a human typo fix" } } });
        native_configuration(&f, "opencode", scope, &human.to_string(), None).await;
        let (_, fresh) = person_conversation(&f, OWNER).await;
        refused_because(
            &agent(
                &f,
                &fresh,
                "create_hook_rule",
                json!({
                    "definition": definition(&f, MAIN_AGENT_ID),
                }),
            )
            .await,
            "follows instructions an agent wrote",
        );
    }
    let f = granted().await;
    person_provider(&f, "opencode", "Human profile instructions").await;
    let human = json!({ "agent": { "build": { "prompt": "Human native prompt" } } });
    native_configuration(&f, "opencode", None, &human.to_string(), None).await;
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
    let created = agent(&f, &bearer, "create_hook_rule", write.clone()).await;
    assert!(!refused(&created), "{created}");
    f.s.store
        .transaction(|db| {
            db.0.execute(
                "DELETE FROM native_instruction_writers WHERE provider='opencode'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (_, legacy) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &legacy, "create_hook_rule", write.clone()).await,
        "did not record who wrote this agent's instructions",
    );
    native_configuration(
        &f,
        "opencode",
        None,
        "{\"agent\":{\"build\":{\"prompt\":\"Human replacement\"}}}",
        None,
    )
    .await;
    let (_, rewritten) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &rewritten, "create_hook_rule", write).await,
        "did not record who wrote this agent's instructions",
    );
}

#[tokio::test]
async fn unsafe_instructions_from_an_earlier_turn_never_stop_blocking() {
    let f = granted().await;
    person_provider(&f, "opencode", "Human profile instructions").await;
    native_configuration(
        &f,
        "opencode",
        None,
        "{\"instructions\":[\"unrecorded.md\"]}",
        None,
    )
    .await;
    let (chat, bearer) = person_conversation(&f, OWNER).await;
    let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
        "did not record the source",
    );
    native_configuration(&f, "opencode", None, "{}", None).await;
    person_instructions(&f, "New human instructions").await;
    let old = f.s.chat_detail(&chat).await.unwrap();
    let old_run = text(&old["run"], "id").to_owned();
    f.s.store
        .transaction(move |db| {
            db.0.execute(
                "DELETE FROM conversation_turn_entries WHERE run_id=?",
                [old_run],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let later = next_person_turn(&f, &chat).await;
    let detail = f.s.chat_detail(&chat).await.unwrap();
    let run = f.s.store.run(text(&detail["run"], "id")).await.unwrap();
    assert_eq!(
        run["snapshot"]["agent"]["instructions"],
        "New human instructions"
    );
    refused_because(
        &agent(&f, &later, "create_hook_rule", write.clone()).await,
        "An earlier turn followed instructions",
    );
    let (_, fresh) = person_conversation(&f, OWNER).await;
    let created = agent(&f, &fresh, "create_hook_rule", write).await;
    assert!(!refused(&created), "{created}");
}

#[tokio::test]
async fn public_mcp_members_have_entry_provenance_without_native_authority() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let token = common::relay_fixture::grant(&f.s, &["read", "write", "run", "manage"]).await;
    f.s.store
        .set(
            &format!("test-relay:{}", cairn_installation::auth::digest(&token)),
            json!({
                "account": MEMBER,
                "scopes": ["read", "write", "run", "manage"],
            }),
            None,
        )
        .await
        .unwrap();
    let created = f
        .mcp(
            "/mcp",
            &token,
            "create_chat",
            json!({ "agentId": MAIN_AGENT_ID }),
        )
        .await;
    assert!(!refused(&created), "{created}");
    let chat = text(mcp_result(&created), "id").to_owned();
    let message = cairn_installation::config::id();
    let sent = f
        .mcp(
            "/mcp",
            &token,
            "send_chat_message",
            json!({
                "chatId": chat,
                "message": { "id": message, "text": "Write my rule." },
            }),
        )
        .await;
    assert!(!refused(&sent), "{sent}");
    let edited = f
        .mcp(
            "/mcp",
            &token,
            "edit_chat_message",
            json!({
                "chatId": chat,
                "messageId": message,
                "message": { "text": "Write my rule carefully." },
            }),
        )
        .await;
    assert!(!refused(&edited), "{edited}");
    let checked = chat.clone();
    f.s.store
        .read(move |db| {
            let creator: String = db.0.query_row(
                "SELECT account_id FROM conversation_writers WHERE chat_id=?",
                [&checked],
                |row| row.get(0),
            )?;
            assert_eq!(creator, MEMBER);
            let writer: String = db.0.query_row(
                "SELECT account_id FROM conversation_entry_writers WHERE chat_id=?",
                [&checked],
                |row| row.get(0),
            )?;
            assert_eq!(writer, MEMBER);
            let native: bool = db.0.query_row(
                "SELECT native_provenance_unknown FROM conversations WHERE id=?",
                [&checked],
                |row| row.get(0),
            )?;
            assert!(native);
            Ok(())
        })
        .await
        .unwrap();
    let bearer = fixture::running_conversation(&f.s, &chat).await;
    let rule = agent(
        &f,
        &bearer,
        "create_hook_rule",
        json!({
            "definition": definition(&f, MAIN_AGENT_ID),
            "enabled": false,
        }),
    )
    .await;
    assert!(!refused(&rule), "{rule}");
    assert_eq!(mcp_result(&rule)["creatorAccountId"], MEMBER);
    assert_eq!(mcp_result(&rule)["nativeAuthority"], false);
}

#[tokio::test]
async fn codex_compaction_prompt_files_cannot_be_configured() {
    let f = granted().await;
    person_provider(&f, "codex", "Human profile instructions").await;
    let (_, current) = call(
        &f,
        OWNER,
        "POST",
        "/api/settings/coding-agents/read",
        json!({ "provider": "codex", "agentId": null }),
    )
    .await;
    let input = json!({
        "provider": "codex",
        "agentId": null,
        "revision": current["revision"],
        "document": "experimental_compact_prompt_file = \"/workspace/project/COMPACT.md\"\n",
        "changes": [],
        "reset": false,
    });
    let (status, saved) = call(
        &f,
        OWNER,
        "PUT",
        "/api/settings/coding-agents",
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{saved}");
    let (_, bearer) = person_conversation(&f, OWNER).await;
    assert!(refused(
        &agent(&f, &bearer, "save_coding_agent_configuration", input).await
    ));
}

#[tokio::test]
async fn engine_changes_record_who_rewrote_the_native_document() {
    let write =
        |f: &Fixture| json!({ "definition": definition(f, MAIN_AGENT_ID), "enabled": false });
    let codex = "developer_instructions = \"Human Codex agent text\"\n";
    let f = granted().await;
    person_provider(&f, "claude", "Human profile instructions").await;
    native_configuration(&f, "codex", Some(MAIN_AGENT_ID), codex, None).await;
    person_provider(&f, "codex", "Human profile instructions").await;
    let (_, bearer) = person_conversation(&f, OWNER).await;
    let created = agent(&f, &bearer, "create_hook_rule", write(&f)).await;
    assert!(!refused(&created), "{created}");
    native_configuration(
        &f,
        "codex",
        Some(MAIN_AGENT_ID),
        "show_raw_agent_reasoning = false\n",
        None,
    )
    .await;
    let (_, later) = person_conversation(&f, OWNER).await;
    let created = agent(&f, &later, "create_hook_rule", write(&f)).await;
    assert!(!refused(&created), "{created}");

    // The person's engine change never launders what an agent or an unrecorded
    // writer left in the document: the remaining compaction prompt keeps its history.
    let kept = "developer_instructions = \"Codex text\"\ncompact_prompt = \"Compaction text\"\n";
    let f = granted().await;
    person_provider(&f, "claude", "Human profile instructions").await;
    native_configuration(&f, "codex", Some(MAIN_AGENT_ID), kept, Some(&f.bearer)).await;
    person_provider(&f, "codex", "Human profile instructions").await;
    let (_, bearer) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write(&f)).await,
        "follows instructions an agent wrote",
    );
    let f = granted().await;
    person_provider(&f, "claude", "Human profile instructions").await;
    native_configuration(&f, "codex", Some(MAIN_AGENT_ID), kept, None).await;
    f.s.store
        .transaction(|db| {
            db.0.execute("DELETE FROM native_instruction_writers", [])?;
            Ok(())
        })
        .await
        .unwrap();
    person_provider(&f, "codex", "Human profile instructions").await;
    let (_, bearer) = person_conversation(&f, OWNER).await;
    refused_because(
        &agent(&f, &bearer, "create_hook_rule", write(&f)).await,
        "did not record who wrote",
    );
}

async fn finish_turn(f: &Fixture, chat: &str) -> String {
    let detail = f.s.chat_detail(chat).await.unwrap();
    let run = text(&detail["run"], "id").to_owned();
    f.s.store
        .change_run(
            &run,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Succeeded),
                recovery_pending: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    run
}

async fn side_question_content_closes_rule_authority_for_good(operation: &str) {
    for account in [MEMBER, OWNER] {
        let f = granted().await;
        common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
        person_provider(&f, "claude", "Human instructions").await;
        let (chat, bearer) = person_conversation(&f, OWNER).await;
        let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
        assert!(!refused(
            &agent(&f, &bearer, "create_hook_rule", write.clone()).await
        ));
        let detail = f.s.chat_detail(&chat).await.unwrap();
        let run = text(&detail["run"], "id").to_owned();
        f.s.side_questions_available(&run, true).await.unwrap();
        f.s.store
            .event(
                &run,
                &cairn_contracts::EventKind::ItemCompleted,
                "A reply",
                Some(json!({
                    "item": {
                        "id": "reply",
                        "type": "agent_message",
                        "text": "A reply",
                    },
                })),
            )
            .await
            .unwrap();
        let question = cairn_installation::config::id();
        let input = json!({
            "id": question,
            "text": "Foreign instructions beside the reply",
            "quote": "Foreign quoted instructions",
            "itemId": "reply",
        });
        let (status, asked) = call(
            &f,
            account,
            "POST",
            &format!("/api/chats/{chat}/side-questions"),
            input.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{asked}");
        let args = match operation {
            "list_chat_side_questions" => json!({ "chatId": chat }),
            "cancel_chat_side_question" => json!({
                "chatId": chat,
                "sideQuestionId": question,
            }),
            "ask_chat_side_question" => {
                f.s.side_question_settle(
                    &run,
                    &json!({
                        "type": "chat.side.answered",
                        "sideQuestionId": question,
                        "text": "Unattested side answer",
                    }),
                )
                .await
                .unwrap();
                assert_eq!(
                    f.s.chat_detail(&chat).await.unwrap()["sideQuestions"][0]["answer"],
                    "Unattested side answer"
                );
                json!({ "chatId": chat, "question": input })
            }
            _ => unreachable!("unexpected side-question operation"),
        };
        let read = agent(&f, &bearer, operation, args).await;
        assert!(!refused(&read), "{read}");
        let returned = if operation == "list_chat_side_questions" {
            &mcp_result(&read)["sideQuestions"][0]
        } else {
            mcp_result(&read)
        };
        assert_eq!(returned["text"], "Foreign instructions beside the reply");
        assert_eq!(returned["quote"], "Foreign quoted instructions");
        if operation == "ask_chat_side_question" {
            assert_eq!(returned["status"], "answered");
            assert_eq!(returned["answer"], "Unattested side answer");
        } else if operation == "cancel_chat_side_question" {
            assert_eq!(returned["status"], "cancelled");
        }
        refused_because(
            &agent(&f, &bearer, "create_hook_rule", write.clone()).await,
            "Another writer reached this conversation",
        );

        // Removing the untrusted stored content and starting a human turn cannot
        // restore authority. This policy also covers questions without any author.
        f.s.store
            .delete(&format!("chat-side-question:{chat}:{question}"))
            .await
            .unwrap();
        assert_eq!(
            f.s.chat_detail(&chat).await.unwrap()["sideQuestions"],
            json!([])
        );
        finish_turn(&f, &chat).await;
        let later = next_person_turn(&f, &chat).await;
        refused_because(
            &agent(&f, &later, "create_hook_rule", write).await,
            "Another writer reached this conversation",
        );
    }
}

#[tokio::test]
async fn reading_own_side_questions_closes_rule_authority_for_good() {
    side_question_content_closes_rule_authority_for_good("list_chat_side_questions").await;
}

#[tokio::test]
async fn cancelling_own_side_questions_closes_rule_authority_for_good() {
    side_question_content_closes_rule_authority_for_good("cancel_chat_side_question").await;
}

#[tokio::test]
async fn retrying_answered_side_questions_closes_rule_authority_for_good() {
    side_question_content_closes_rule_authority_for_good("ask_chat_side_question").await;
}

#[tokio::test]
async fn wakeups_record_the_skills_they_freeze_for_good() {
    let f = granted().await;
    let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
    let (chat, bearer) = person_conversation(&f, OWNER).await;
    cairn_installation::waits::schedule(
        &f.s,
        &bearer,
        &json!({ "conditions": [{ "type": "delay", "seconds": 420 }], "reason": "Wait for CI" }),
    )
    .await
    .unwrap();
    // Another agent writes a skill while the conversation sleeps.
    let saved = agent(
        &f,
        &f.bearer,
        "save_skill",
        json!({
            "scope": "global",
            "name": "helper",
            "content": skill_content("helper", "Agent instructions"),
        }),
    )
    .await;
    assert!(!refused(&saved), "{saved}");
    let run = finish_turn(&f, &chat).await;
    let mut due = f.s.conversation_boundary(&chat).await.unwrap();
    due["wakeup"]["wakeAt"] = (cairn_installation::config::now() - 1000).into();
    f.s.store.save_conversation_boundary(due).await.unwrap();
    f.s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let woken = f.s.store.run(&run).await.unwrap();
    assert_eq!(woken["status"], "queued", "{woken}");
    assert!(
        woken["snapshot"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["name"] == "helper"),
        "{woken}"
    );
    // Deleting the skill before the person's next turn does not erase the wakeup.
    let (status, deleted) = call(
        &f,
        OWNER,
        "DELETE",
        "/api/skills/global/helper",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    finish_turn(&f, &chat).await;
    let later = next_person_turn(&f, &chat).await;
    let after = f.s.store.run(&run).await.unwrap();
    assert!(
        !after["snapshot"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["name"] == "helper")
    );
    let refusal = agent(&f, &later, "create_hook_rule", write).await;
    assert!(refused(&refusal), "{refusal}");
    assert!(refusal.to_string().contains("helper"), "{refusal}");
}

#[tokio::test]
async fn engine_switches_record_the_instructions_they_freeze_for_good() {
    let f = granted().await;
    let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
    person_provider(&f, "codex", "Human profile instructions").await;
    // Claude turns would follow native defaults that an agent wrote.
    native_configuration(
        &f,
        "claude",
        None,
        r#"{"alwaysThinkingEnabled":true}"#,
        Some(&f.bearer),
    )
    .await;
    let (chat, bearer) = person_conversation(&f, OWNER).await;
    let created = agent(&f, &bearer, "create_hook_rule", write.clone()).await;
    assert!(!refused(&created), "{created}");
    let detail = f.s.chat_detail(&chat).await.unwrap();
    let run = text(&detail["run"], "id").to_owned();
    let turn = f.s.store.run(&run).await.unwrap()["chatExecution"]["messageId"].clone();
    f.s.store
        .change_run(
            &run,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Queued),
                account_wait_reason: Some(Some(
                    "Waiting for a Codex account with available usage.".into(),
                )),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut account = serde_json::to_value(
        f.s.accounts
            .create(
                &f.s,
                cairn_installation::provider::Provider::Claude,
                "Alternative",
            )
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    f.s.store.save_account_fixture(account).await.unwrap();
    let (status, accepted) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/chats/{chat}/coding-agent"),
        json!({
            "requestId": cairn_installation::config::id(),
            "runId": run,
            "turnId": turn,
            "provider": "claude",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    f.s.worker.tick(&f.s).await.unwrap();
    common::eventually(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(20),
        async || {
            let switched = f.s.store.run(&run).await.unwrap();
            (switched["snapshot"]["agent"]["provider"] == "claude").then_some(())
        },
    )
    .await;
    // The person's next turn returns to Codex, which follows human instructions only.
    finish_turn(&f, &chat).await;
    let (status, sent) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/chats/{chat}/messages"),
        json!({
            "id": cairn_installation::config::id(),
            "text": "Back to Codex.",
            "provider": "codex",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    let later = fixture::running_conversation(&f.s, &chat).await;
    let back = f.s.store.run(&run).await.unwrap();
    assert_eq!(back["snapshot"]["agent"]["provider"], "codex");
    refused_because(
        &agent(&f, &later, "create_hook_rule", write).await,
        "An earlier turn followed instructions without a recorded human writer",
    );
}

#[tokio::test]
async fn engine_switch_requesters_reach_the_conversation_for_good() {
    for surface in [
        "http-member",
        "public-member",
        "installation",
        "unknown-public",
        "http-owner",
        "public-owner",
    ] {
        let f = granted().await;
        common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
        let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
        person_provider(&f, "codex", "Human profile instructions").await;
        // Both providers follow human-written instructions; the switch author matters.
        native_configuration(
            &f,
            "claude",
            None,
            r#"{"alwaysThinkingEnabled":true}"#,
            None,
        )
        .await;
        let (chat, bearer) = person_conversation(&f, OWNER).await;
        let created = agent(&f, &bearer, "create_hook_rule", write.clone()).await;
        assert!(!refused(&created), "{created}");
        let detail = f.s.chat_detail(&chat).await.unwrap();
        let run = text(&detail["run"], "id").to_owned();
        let turn = f.s.store.run(&run).await.unwrap()["chatExecution"]["messageId"].clone();
        f.s.store
            .change_run(
                &run,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Queued),
                    account_wait_reason: Some(Some(
                        "Waiting for a Codex account with available usage.".into(),
                    )),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut account = serde_json::to_value(
            f.s.accounts
                .create(
                    &f.s,
                    cairn_installation::provider::Provider::Claude,
                    "Alternative",
                )
                .await
                .unwrap(),
        )
        .unwrap();
        account["state"] = "ready".into();
        f.s.store.save_account_fixture(account).await.unwrap();
        let input = json!({
            "chatId": chat,
            "requestId": cairn_installation::config::id(),
            "runId": run,
            "turnId": turn,
            "provider": "claude",
        });
        let foreign = matches!(
            surface,
            "http-member" | "public-member" | "installation" | "unknown-public"
        );
        let account = if surface.ends_with("member") {
            MEMBER
        } else {
            OWNER
        };
        if surface.starts_with("http") {
            let (status, accepted) = call(
                &f,
                account,
                "POST",
                &format!("/api/chats/{chat}/coding-agent"),
                input.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{surface}: {accepted}");
        } else if surface == "unknown-public" {
            // Exercise the shared public dispatcher with scopes but no recorded
            // account. Scopes cannot attest who chose the instruction source.
            let mut identity = InstallationIdentity::trusted(InstallationRole::Member, "");
            identity.mcp_scopes = Some(vec!["run".into()]);
            let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
            let request = common::request("POST", "/api/mcp")
                .extension(identity)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": common::mcp_arguments("switch_chat_coding_agent", &input),
                    })
                    .to_string(),
                ))
                .unwrap();
            let response = common::send(&app, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            let accepted = common::read_json(response).await;
            assert!(!refused(&accepted), "{surface}: {accepted}");
        } else {
            let (endpoint, token) = if surface == "installation" {
                ("/mcp-installation", f.bearer.clone())
            } else {
                let token = common::relay_fixture::grant(&f.s, &["run"]).await;
                f.s.store
                    .set(
                        &format!("test-relay:{}", cairn_installation::auth::digest(&token)),
                        json!({ "account": account, "scopes": ["run"] }),
                        None,
                    )
                    .await
                    .unwrap();
                ("/mcp", token)
            };
            let accepted = f
                .mcp(endpoint, &token, "switch_chat_coding_agent", input.clone())
                .await;
            assert!(!refused(&accepted), "{surface}: {accepted}");
        }
        // The entry is retained with the acceptance, before asynchronous recovery, and
        // retrying the same request as the owner cannot replace the original author.
        let checked = chat.clone();
        let reached = f.s.store
            .read(move |db| {
                Ok(db.0.query_row(
                    "SELECT reached_by,reached_account_id FROM conversation_writers WHERE chat_id=?",
                    [&checked],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?)),
                )?)
            })
            .await
            .unwrap();
        let (status, retried) = call(
            &f,
            OWNER,
            "POST",
            &format!("/api/chats/{chat}/coding-agent"),
            input,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{retried}");
        f.s.worker.tick(&f.s).await.unwrap();
        common::eventually(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(20),
            async || {
                let switched = f.s.store.run(&run).await.unwrap();
                (switched["snapshot"]["agent"]["provider"] == "claude").then_some(())
            },
        )
        .await;
        // The person's next turn returns to Codex, which follows human instructions only.
        finish_turn(&f, &chat).await;
        let (status, sent) = call(
            &f,
            OWNER,
            "POST",
            &format!("/api/chats/{chat}/messages"),
            json!({
                "id": cairn_installation::config::id(),
                "text": "Back to Codex.",
                "provider": "codex",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{sent}");
        let later = fixture::running_conversation(&f.s, &chat).await;
        let back = f.s.store.run(&run).await.unwrap();
        assert_eq!(back["snapshot"]["agent"]["provider"], "codex");
        let result = agent(&f, &later, "create_hook_rule", write).await;
        if foreign {
            refused_because(&result, "Another writer reached this conversation");
        } else {
            assert!(!refused(&result), "{surface}: {result}");
        }
        if foreign {
            assert_eq!(
                reached.0.as_deref(),
                Some(match surface {
                    "installation" => "agent",
                    "unknown-public" => "unknown",
                    _ => "person",
                }),
                "{surface}"
            );
            assert_eq!(
                reached.1.as_deref(),
                surface.ends_with("member").then_some(MEMBER),
                "{surface}"
            );
        } else {
            assert_eq!(reached, (None, None), "{surface}");
        }
    }
}

#[tokio::test]
async fn profile_engine_changes_reach_conversations_whose_source_moves() {
    for by_agent in [false, true] {
        let f = granted().await;
        common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
        let write = json!({ "definition": definition(&f, MAIN_AGENT_ID), "enabled": false });
        person_provider(&f, "codex", "Human profile instructions").await;
        native_instructions(&f, "Owner native text", None).await;
        let (_, bearer) = person_conversation(&f, MEMBER).await;
        let created = agent(&f, &bearer, "create_hook_rule", write.clone()).await;
        assert!(!refused(&created), "{created}");
        // Codex conversations now follow the owner's native document instead of the
        // profile, although no text changed.
        let current = f.s.store.agent(MAIN_AGENT_ID).await.unwrap().unwrap();
        let mut settings = serde_json::to_value(&current.settings).unwrap();
        settings["provider"] = "claude".into();
        settings["model"] = "".into();
        let input = json!({
            "agentId": MAIN_AGENT_ID,
            "revision": current.revision,
            "settings": settings,
            "review": {},
        });
        if by_agent {
            let saved = agent(&f, &f.bearer, "replace_agent", input).await;
            assert!(!refused(&saved), "{saved}");
        } else {
            let (status, saved) = call(
                &f,
                OWNER,
                "PUT",
                &format!("/api/agents/{MAIN_AGENT_ID}"),
                input,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{saved}");
        }
        refused_because(
            &agent(&f, &bearer, "create_hook_rule", write).await,
            "Another writer reached this conversation",
        );
    }
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

#[tokio::test]
async fn listing_rules_remains_consistent_during_concurrent_deletions() {
    let f = granted().await;
    let mut ids = Vec::new();
    for index in 0..32 {
        let mut data = definition(&f, MAIN_AGENT_ID);
        data["name"] = format!("Concurrent rule {index}").into();
        let (status, saved) = create(&f, OWNER, data).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        ids.push(text(&saved, "id").to_owned());
    }
    let read = || async {
        for _ in 0..32 {
            let (status, listed) = call(&f, OWNER, "GET", "/api/hook-rules", Value::Null).await;
            assert_eq!(status, StatusCode::OK, "{listed}");
            assert!(listed["rules"].is_array());
            tokio::task::yield_now().await;
        }
    };
    let remove = async {
        for id in ids {
            let (status, deleted) = call(
                &f,
                OWNER,
                "DELETE",
                &format!("/api/hook-rules/{id}"),
                Value::Null,
            )
            .await;
            assert!(status.is_success(), "{deleted}");
            tokio::task::yield_now().await;
        }
    };
    tokio::join!(read(), read(), read(), read(), remove);
    let (status, listed) = call(&f, OWNER, "GET", "/api/hook-rules", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["rules"], json!([]));
}
