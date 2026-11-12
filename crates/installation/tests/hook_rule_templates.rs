//! Rule templates declared by plugins (ADR-0075, « Modèles de règles »): installing one
//! creates ordinary rules owned by the installer, with no link to the template.
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

/// « Support »: acknowledge chosen people in a channel, and start an agent for anyone.
fn template(name: &str) -> Value {
    json!({
        "name": "support",
        "title": "Support",
        "description": "Answers a support channel.",
        "parameters": [
            { "name": "agent", "title": "Agent", "kind": "agent", "required": true },
            { "name": "channel", "title": "Channel", "kind": "resource", "resourceType": "channel", "required": true },
            { "name": "people", "title": "People", "kind": "resources", "resourceType": "person", "required": true },
            { "name": "greeting", "title": "Greeting", "kind": "text", "default": "Hello" },
            { "name": "hourly", "title": "Triggerings per hour", "kind": "number", "default": 30 },
        ],
        "rules": [
            {
                "name": name,
                "trigger": { "type": "plugin", "hook": "message" },
                "executionAgentId": { "$parameter": "agent" },
                "steps": [{
                    "type": "if",
                    "id": "known",
                    "condition": {
                        "type": "authorIn",
                        "author": { "type": "formula", "expression": "payload.author" },
                        "people": { "$parameter": "people" },
                    },
                    "then": [{
                        "type": "action",
                        "id": "post",
                        "action": {
                            "type": "callTool",
                            "tool": "grant_probe_post",
                            "input": {
                                "channel": { "type": "literal", "value": { "$parameter": "channel", "field": "id" } },
                                "thread": { "type": "template", "text": "{{payload.thread}}" },
                            },
                        },
                    }],
                }],
            },
            {
                "name": "Start for anyone",
                "trigger": { "type": "plugin", "hook": "message" },
                "executionAgentId": { "$parameter": "agent" },
                "steps": [{
                    "type": "action",
                    "id": "start",
                    "action": {
                        "type": "startAgent",
                        "agentId": { "$parameter": "agent" },
                        "prompt": { "type": "literal", "value": { "$parameter": "greeting" } },
                    },
                }],
                "limits": { "triggeringsPerHour": { "$parameter": "hourly" }, "conversationsPerHour": 5 },
            },
        ],
    })
}

/// The probe package, at another version, declaring the support template.
async fn with_template(f: &Fixture, version: &str, rule_name: &str) -> Value {
    let mut manifest = f.manifest.clone();
    manifest["version"] = version.into();
    manifest["ruleTemplates"] = json!([template(rule_name)]);
    f.publish(&manifest).await.unwrap()
}

async fn granted() -> Fixture {
    let f = Fixture::new().await;
    f.set(json!([
        contribution("hook", "message", support()),
        contribution("tool", "grant_probe_post", support()),
    ]))
    .await;
    with_template(&f, "2", "Support replies").await;
    f
}

fn parameters(agent: &str) -> Value {
    json!({
        "agent": agent,
        "channel": { "resourceType": "channel", "id": "support" },
        "people": [{ "resourceType": "person", "id": "alice" }],
    })
}

fn install(f: &Fixture, parameters: &Value) -> Value {
    json!({
        "instanceId": f.instance,
        "template": "support",
        "parameters": parameters,
    })
}

async fn rule_count(f: &Fixture) -> usize {
    let (_, listed) = call(f, OWNER, "GET", "/api/hook-rules", Value::Null).await;
    listed["rules"].as_array().unwrap().len()
}

const INSTALL: &str = "/api/hook-rules/templates/install";

#[tokio::test]
async fn installing_a_template_creates_ordinary_rules_that_updates_never_change() {
    let f = granted().await;

    let (status, listed) = call(&f, OWNER, "GET", "/api/hook-rules/templates", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let templates = listed["templates"].as_array().unwrap();
    assert_eq!(templates.len(), 1, "{listed}");
    assert_eq!(templates[0]["instanceId"], f.instance.as_str());
    assert_eq!(templates[0]["pluginVersion"], "2");
    assert_eq!(templates[0]["template"]["name"], "support");
    assert_eq!(templates[0]["template"]["parameters"][0]["kind"], "agent");
    let path = format!("/api/hook-rules/templates?instanceId={}", f.instance);
    let (_, filtered) = call(&f, OWNER, "GET", &path, Value::Null).await;
    assert_eq!(filtered["templates"].as_array().unwrap().len(), 1);
    let other = uuid::Uuid::new_v4();
    let path = format!("/api/hook-rules/templates?instanceId={other}");
    let (_, none) = call(&f, OWNER, "GET", &path, Value::Null).await;
    assert_eq!(none["templates"], json!([]));

    // Parameters are checked before anything is written.
    for (given, expected) in [
        (json!({ "agent": MAIN_AGENT_ID }), "Enter Channel."),
        (
            {
                let mut given = parameters(MAIN_AGENT_ID);
                given["greeting"] = json!([]);
                given
            },
            "takes a text",
        ),
        (
            {
                let mut given = parameters(MAIN_AGENT_ID);
                given["hourly"] = json!([]);
                given
            },
            "takes a number",
        ),
        (
            json!({ "agent": MAIN_AGENT_ID, "channel": { "resourceType": "thread", "id": "x" }, "people": [] }),
            "type channel",
        ),
        (
            {
                let mut given = parameters(MAIN_AGENT_ID);
                given["unknown"] = true.into();
                given
            },
            "no parameter unknown",
        ),
    ] {
        let (status, refused) = call(&f, OWNER, "POST", INSTALL, install(&f, &given)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
        assert!(
            refused["error"].as_str().unwrap().contains(expected),
            "{refused}"
        );
    }

    // A rule failing validation keeps every rule of the template from being created.
    let mut excessive = parameters(MAIN_AGENT_ID);
    excessive["hourly"] = 10_000.into();
    let (status, refused) = call(&f, OWNER, "POST", INSTALL, install(&f, &excessive)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().starts_with("Rule 2:"),
        "{refused}"
    );
    assert_eq!(rule_count(&f).await, 0);

    let creation = uuid::Uuid::new_v4().to_string();
    let mut body = install(&f, &parameters(MAIN_AGENT_ID));
    body["creationId"] = creation.clone().into();
    let (status, installed) = call(&f, OWNER, "POST", INSTALL, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{installed}");
    let rules = installed["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2);
    for rule in rules {
        assert_eq!(rule["creatorAccountId"], OWNER);
        assert_eq!(rule["source"], "template");
        assert_eq!(rule["blocks"], json!([]), "{rule}");
        assert_eq!(
            rule["definition"]["trigger"]["instanceId"],
            f.instance.as_str()
        );
        assert_eq!(rule["definition"]["executionAgentId"], MAIN_AGENT_ID);
    }
    let support = &rules[0]["definition"];
    assert_eq!(support["name"], "Support replies");
    assert_eq!(support["limits"]["triggeringsPerHour"], 60);
    assert_eq!(
        support["steps"][0]["condition"]["people"],
        json!([{ "resourceType": "person", "id": "alice" }])
    );
    let post = &support["steps"][0]["then"][0]["action"];
    assert_eq!(post["instanceId"], f.instance.as_str());
    assert_eq!(post["input"]["channel"]["value"], "support");
    assert_eq!(rules[0]["warnings"], json!([]));
    // Starting an agent with broad rights for anyone warns its creator.
    assert_eq!(
        rules[1]["warnings"][0]["kind"], "openToAnyone",
        "{}",
        rules[1]
    );
    assert_eq!(rules[1]["definition"]["limits"]["triggeringsPerHour"], 30);
    assert_eq!(
        rules[1]["definition"]["steps"][0]["action"]["prompt"]["value"],
        "Hello"
    );

    // Retrying returns the same rules, by their creator only.
    let (status, again) = call(&f, OWNER, "POST", INSTALL, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["rules"][0]["id"], rules[0]["id"]);
    assert_eq!(again["rules"][1]["id"], rules[1]["id"]);
    assert_eq!(rule_count(&f).await, 2);
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let (status, taken) = call(&f, MEMBER, "POST", INSTALL, body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{taken}");

    // Disabled on request, and an ordinary rule its creator edits.
    let mut disabled = install(&f, &parameters(MAIN_AGENT_ID));
    disabled["enabled"] = false.into();
    let (status, disabled) = call(&f, OWNER, "POST", INSTALL, disabled).await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(disabled["rules"][0]["blocks"][0]["kind"], "disabled");

    // A plugin update changes the template, never the rules it created.
    let id = text(&rules[0], "id").to_owned();
    let second = text(&rules[1], "id").to_owned();
    with_template(&f, "3", "Renamed by the update").await;
    let (_, kept) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(kept["version"], 1);
    assert_eq!(kept["definition"], rules[0]["definition"]);
    let (_, listed) = call(&f, OWNER, "GET", "/api/hook-rules/templates", Value::Null).await;
    assert_eq!(listed["templates"][0]["pluginVersion"], "3");

    // Retrying recovers the original set even when the template shrinks, changes its
    // parameters, or disappears. It never depends on the current manifest.
    let mut changed = template("One rule now");
    changed["rules"].as_array_mut().unwrap().pop();
    changed["parameters"]
        .as_array_mut()
        .unwrap()
        .retain(|parameter| !matches!(parameter["name"].as_str(), Some("greeting" | "hourly")));
    for (version, templates) in [("4", json!([changed])), ("5", json!([]))] {
        let mut manifest = f.manifest.clone();
        manifest["version"] = version.into();
        manifest["ruleTemplates"] = templates;
        f.publish(&manifest).await.unwrap();
        let (status, retried) = call(&f, OWNER, "POST", INSTALL, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{retried}");
        let retried = retried["rules"].as_array().unwrap();
        assert_eq!(retried.len(), 2);
        assert_eq!(retried[0]["id"], rules[0]["id"]);
        assert_eq!(retried[1]["id"], rules[1]["id"]);
    }

    // Once one of its rules is deleted, the creation identity no longer installs.
    let (status, _) = call(
        &f,
        OWNER,
        "DELETE",
        &format!("/api/hook-rules/{second}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, partial) = call(&f, OWNER, "POST", INSTALL, body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{partial}");
    assert_eq!(
        listed["templates"][0]["template"]["rules"][0]["name"],
        "Renamed by the update"
    );
}

#[tokio::test]
async fn template_installation_follows_the_rule_authority() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let private =
        f.s.agent_fixture(json!({ "name": "Private" }), None)
            .await
            .unwrap();
    let private = text(&private, "id").to_owned();

    // A member installs only with agents they may use, and owns what they install.
    let (status, refused) = call(
        &f,
        MEMBER,
        "POST",
        INSTALL,
        install(&f, &parameters(&private)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("access to this agent"),
        "{refused}"
    );
    assert_eq!(rule_count(&f).await, 0);
    let (status, installed) = call(
        &f,
        MEMBER,
        "POST",
        INSTALL,
        install(&f, &parameters(MAIN_AGENT_ID)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{installed}");
    assert_eq!(installed["rules"][0]["creatorAccountId"], MEMBER);
    assert_eq!(installed["rules"][0]["allowed"]["edit"], true);

    // An installation agent lists templates, never installs one outside an eligible
    // conversation.
    let listed = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "list_hook_rule_templates",
            json!({}),
        )
        .await;
    assert_ne!(listed["result"]["isError"], true, "{listed}");
    assert_eq!(
        mcp_result(&listed)["templates"][0]["template"]["name"],
        "support"
    );
    let refused = f
        .mcp(
            "/mcp-installation",
            &f.bearer,
            "install_hook_rule_template",
            install(&f, &parameters(MAIN_AGENT_ID)),
        )
        .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(rule_count(&f).await, 2);

    // A public client needs the manage scope, and installs for the owner.
    let reader = common::relay_fixture::grant(&f.s, &["read"]).await;
    let listed = f
        .mcp("/mcp", &reader, "list_hook_rule_templates", json!({}))
        .await;
    assert_ne!(listed["result"]["isError"], true, "{listed}");
    let refused = f
        .mcp(
            "/mcp",
            &reader,
            "install_hook_rule_template",
            install(&f, &parameters(MAIN_AGENT_ID)),
        )
        .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    let manager = common::relay_fixture::grant(&f.s, &["read", "manage"]).await;
    let created = f
        .mcp(
            "/mcp",
            &manager,
            "install_hook_rule_template",
            install(&f, &parameters(MAIN_AGENT_ID)),
        )
        .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    let created = mcp_result(&created);
    assert_eq!(created["rules"][0]["creatorAccountId"], OWNER);
    assert_eq!(created["rules"][0]["source"], "template");

    // A member whose agents hold nothing on the instance is offered no template.
    common::relay_fixture::add_member(&f.s, "other-member", "membership-other").await;
    let (_, offered) = call(
        &f,
        "other-member",
        "GET",
        "/api/hook-rules/templates",
        Value::Null,
    )
    .await;
    assert_eq!(offered["templates"], json!([]));
    let (status, refused) = call(
        &f,
        "other-member",
        "POST",
        INSTALL,
        install(&f, &parameters(MAIN_AGENT_ID)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{refused}");
}

#[tokio::test]
async fn packages_declare_only_templates_using_their_own_hooks_and_tools() {
    let f = Fixture::new().await;
    let mut manifest = f.manifest.clone();
    manifest["version"] = "2".into();
    let mut undeclared = template("Support replies");
    undeclared["rules"][0]["steps"][0]["then"][0]["action"]["tool"] = "other_tool".into();
    manifest["ruleTemplates"] = json!([undeclared]);
    let error = f.publish(&manifest).await.unwrap_err();
    assert_eq!(error.status, 400);
    assert!(
        error.message.contains("does not declare"),
        "{}",
        error.message
    );

    let mut named = template("Support replies");
    named["rules"][1]["trigger"]["instanceId"] = f.instance.clone().into();
    manifest["ruleTemplates"] = json!([named]);
    let error = f.publish(&manifest).await.unwrap_err();
    assert!(
        error.message.contains("name no instance"),
        "{}",
        error.message
    );

    let mut formula = template("Support replies");
    formula["rules"][1]["steps"][0]["action"]["prompt"] =
        json!({ "type": "formula", "expression": "missing_field" });
    manifest["ruleTemplates"] = json!([formula]);
    let error = f.publish(&manifest).await.unwrap_err();
    assert!(error.message.contains("Rule 2:"), "{}", error.message);
}
