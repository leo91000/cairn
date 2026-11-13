//! The rule engine from a queued plugin hook event to tool calls, values, waits and
//! history (ADR-0075, « Règles de hook » and « Budget du moteur de règles »).
mod common;

use axum::{body::Body, http::StatusCode};
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    rules::engine,
    validation::text,
};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

#[path = "fixtures/plugin_grants.rs"]
mod fixture;

use common::agent_fixture::AgentFixture;
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

async fn granted() -> Fixture {
    let f = Fixture::new().await;
    f.set(json!([
        contribution("hook", "message", support()),
        contribution("tool", "grant_probe_post", support()),
    ]))
    .await;
    f
}

/// Queues a hook event as the plugin's transport would. The queue identity derives from
/// the external one, so queuing it again models an event read again after a stop.
async fn emit(f: &Fixture, external: &str, payload: Value) {
    emit_on(f, &f.instance, external, payload).await;
}

async fn emit_on(f: &Fixture, instance_id: &str, external: &str, payload: Value) {
    let instance =
        f.s.store
            .plugin_instance(instance_id)
            .await
            .unwrap()
            .unwrap();
    let package = cairn_installation::plugins::registry::active(&f.s, &instance.plugin_id)
        .await
        .unwrap();
    let external = external.to_owned();
    f.s.store
        .transaction(move |db| {
            db.0.execute(
                "INSERT INTO plugin_hook_events VALUES(?1,?2,?3,?4,?5,'message',?6,?7)",
                rusqlite::params![
                    format!("queued-{external}"),
                    instance.id,
                    instance.revision,
                    package.digest,
                    external,
                    payload.to_string(),
                    cairn_installation::config::now(),
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

fn steps(actions: &Value) -> Value {
    json!([{
        "type": "if",
        "id": "known",
        "condition": {
            "type": "authorIn",
            "author": { "type": "formula", "expression": "author" },
            "people": [{ "resourceType": "person", "id": "alice" }],
        },
        "then": actions,
        "otherwise": [],
    }])
}

fn post(id: &str, f: &Fixture, extra: &Value) -> Value {
    let mut input = json!({
        "channel": { "type": "literal", "value": "support" },
        "author": { "type": "template", "text": "by {{author}}" },
    });
    for (key, value) in extra.as_object().into_iter().flatten() {
        input[key] = value.clone();
    }
    json!({
        "type": "action",
        "id": id,
        "action": { "type": "callTool", "instanceId": f.instance, "tool": "grant_probe_post", "input": input },
    })
}

async fn rule(f: &Fixture, account: &str, steps: Value, triggerings_per_hour: u32) -> String {
    let (status, rule) = call(
        f,
        account,
        "POST",
        "/api/hook-rules",
        json!({
            "definition": {
                "name": "Engine",
                "trigger": { "type": "plugin", "instanceId": f.instance, "hook": "message" },
                "executionAgentId": MAIN_AGENT_ID,
                "steps": steps,
                "limits": { "triggeringsPerHour": triggerings_per_hour, "conversationsPerHour": 5 },
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    text(&rule, "id").to_owned()
}

async fn history(f: &Fixture, account: &str, rule: &str) -> Value {
    let (status, history) = call(
        f,
        account,
        "GET",
        &format!("/api/hook-rules/{rule}/triggerings"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    history
}

async fn detail(f: &Fixture, account: &str, triggering: &Value) -> Value {
    let id = text(triggering, "id");
    let (status, detail) = call(
        f,
        account,
        "GET",
        &format!("/api/hook-rule-triggerings/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    detail
}

#[tokio::test]
async fn an_event_calls_the_tool_remembers_a_value_and_resumes_after_a_wait() {
    let f = granted().await;
    let rule = rule(
        &f,
        OWNER,
        steps(&json!([
            post("post", &f, &json!({})),
            {
                "type": "action", "id": "remember",
                "action": { "type": "storeValue", "scope": "rule", "name": "last",
                    "value": { "type": "formula", "expression": "{'author': author, 'status': steps.post.status}" } },
            },
            {
                "type": "action", "id": "pause",
                "action": { "type": "wait", "seconds": { "type": "literal", "value": 1 } },
            },
            post("again", &f, &json!({ "author": { "type": "formula", "expression": "values.last.author" } })),
        ])),
        60,
    )
    .await;

    emit(
        &f,
        "event-1",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    let listed = history(&f, OWNER, &rule).await;
    let triggering = &listed["triggerings"][0];
    assert_eq!(triggering["status"], "waiting", "{listed}");
    assert!(triggering["resumesAt"].as_i64().unwrap() > 0);

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 2);
    let detail = detail(&f, OWNER, triggering).await;
    assert_eq!(detail["status"], "completed", "{detail}");
    assert_eq!(detail["actions"], 4);
    assert_eq!(detail["event"]["payload"]["author"], "alice");
    let steps = detail["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 4);
    assert_eq!(steps[0]["input"]["author"], "by alice");
    assert_eq!(
        steps[1]["result"],
        json!({ "author": "alice", "status": 200 })
    );
    assert_eq!(steps[3]["input"]["author"], "alice");

    // Both agent surfaces expose the same history and details as the authenticated UI.
    let reader = common::relay_fixture::grant(&f.s, &["read"]).await;
    let completed = history(&f, OWNER, &rule).await;
    for (path, bearer) in [
        ("/mcp", reader.as_str()),
        ("/mcp-installation", f.bearer.as_str()),
    ] {
        for (operation, input, expected) in [
            (
                "list_hook_rule_triggerings",
                json!({ "ruleId": rule }),
                &completed,
            ),
            (
                "get_hook_rule_triggering",
                json!({ "triggeringId": triggering["id"] }),
                &detail,
            ),
        ] {
            let response = f.mcp(path, bearer, operation, input).await;
            assert_ne!(response["result"]["isError"], true, "{response}");
            assert_eq!(&response["result"]["structuredContent"]["result"], expected);
        }
    }

    // An event read again after a stop, before its acknowledgement, is not triggered twice.
    emit(
        &f,
        "event-1",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn strangers_uncovered_resources_and_blocked_rules_cause_no_effect() {
    let f = granted().await;
    let rule = rule(&f, OWNER, steps(&json!([post("post", &f, &json!({}))])), 60).await;

    // An author the tree does not accept ends without effect and is only counted.
    emit(
        &f,
        "stranger",
        json!({ "channel": "support", "author": "mallory" }),
    )
    .await;
    // A payload outside the execution agent's hook grant is never delivered.
    emit(
        &f,
        "general",
        json!({ "channel": "general", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    let listed = history(&f, OWNER, &rule).await;
    assert_eq!(listed["triggerings"], json!([]));
    assert_eq!(listed["withoutEffect"], 1);

    let path = format!("/api/hook-rules/{rule}/enabled");
    call(
        &f,
        OWNER,
        "POST",
        &path,
        json!({ "ruleId": rule, "enabled": false }),
    )
    .await;
    emit(
        &f,
        "disabled",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    // Dropped events do not wait for the rule to come back.
    call(
        &f,
        OWNER,
        "POST",
        &path,
        json!({ "ruleId": rule, "enabled": true }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_hourly_cap_suspends_the_rule_and_tells_its_creator() {
    let f = granted().await;
    let rule = rule(&f, OWNER, steps(&json!([post("post", &f, &json!({}))])), 1).await;
    emit(
        &f,
        "first",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    emit(
        &f,
        "second",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);

    let (_, current) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{rule}"),
        Value::Null,
    )
    .await;
    assert_eq!(current["blocks"][0]["kind"], "budgetExhausted", "{current}");
    let listed = history(&f, OWNER, &rule).await;
    let statuses = listed["triggerings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["status"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        statuses.contains(&"failed") && statuses.contains(&"completed"),
        "{listed}"
    );
    let alerts = f.s.store.keys("push-outbox:").await.unwrap();
    assert!(
        alerts
            .iter()
            .any(|(_, event)| event["members"] == json!([OWNER])),
        "{alerts:?}"
    );

    // Suspended, the rule receives nothing until its creator or the owner resumes it.
    emit(
        &f,
        "third",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    let (status, _) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/hook-rules/{rule}/budget"),
        json!({ "ruleId": rule }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    emit(
        &f,
        "after-reset",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 2);
    let (_, current) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{rule}"),
        Value::Null,
    )
    .await;
    assert!(
        current["blocks"].as_array().unwrap().is_empty(),
        "{current}"
    );
}

#[tokio::test]
async fn refused_actions_retry_alone_and_interrupted_ones_never_replay() {
    let f = granted().await;
    let rule = rule(
        &f,
        OWNER,
        steps(&json!([
            post("first", &f, &json!({})),
            post(
                "refused",
                &f,
                &json!({ "retry": { "type": "literal", "value": true } })
            ),
        ])),
        60,
    )
    .await;
    emit(
        &f,
        "retry",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let listed = history(&f, OWNER, &rule).await;
    assert_eq!(listed["triggerings"][0]["status"], "retrying", "{listed}");
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);

    // The service accepts now: only the refused action runs again.
    f.marker("accept").await;
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    engine::tick(&f.s).await.unwrap();
    let detail = detail(&f, OWNER, &listed["triggerings"][0]).await;
    assert_eq!(detail["status"], "completed", "{detail}");
    assert_eq!(f.posts.load(Ordering::SeqCst), 2);
    assert_eq!(detail["steps"][1]["attempts"], 2);
    let triggering_id = text(&detail, "id").to_owned();
    let calls =
        f.s.store
            .read(move |db| {
                Ok(db.0.query_row(
                    "SELECT tool_calls FROM hook_rule_triggerings WHERE id=?",
                    [triggering_id],
                    |row| row.get::<_, u32>(0),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(calls, 3);

    // An action in flight when Cairn stopped stays ambiguous.
    emit(
        &f,
        "interrupted",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    let id = rule.clone();
    f.s.store
        .transaction(move |db| {
            db.0.execute(
                "INSERT INTO hook_rule_triggerings(id,rule_id,version,event_id,event,status,cursor,pending,in_flight,actions,tool_calls,counted_at,verify,verify_since,next_at,lease_until,error,created_at,updated_at,finished_at) VALUES('00000000-0000-4000-8000-0000000000aa',?1,1,'lost','{}','running','{\"frames\":[],\"steps\":{},\"count\":1}','{\"nodeId\":\"first\",\"position\":0,\"attempts\":1,\"input\":{}}',1,1,1,1,0,NULL,NULL,1,NULL,1,1,NULL)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    engine::tick(&f.s).await.unwrap();
    let (_, lost) = call(
        &f,
        OWNER,
        "GET",
        "/api/hook-rule-triggerings/00000000-0000-4000-8000-0000000000aa",
        Value::Null,
    )
    .await;
    assert_eq!(lost["status"], "failed", "{lost}");
    assert!(
        lost["error"]
            .as_str()
            .unwrap()
            .contains("may have taken effect")
    );
}

#[tokio::test]
async fn members_triggerings_need_their_membership_and_hide_details_from_others() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    common::relay_fixture::add_member(&f.s, "other", "membership-other").await;
    let rule = rule(
        &f,
        MEMBER,
        steps(&json!([
            post("post", &f, &json!({})),
            {
                "type": "action", "id": "pause",
                "action": { "type": "wait", "seconds": { "type": "literal", "value": 600 } },
            },
        ])),
        60,
    )
    .await;
    emit(
        &f,
        "member",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);

    let listed = history(&f, "other", &rule).await;
    let triggering = &listed["triggerings"][0];
    assert_eq!(triggering["status"], "waiting");
    assert_eq!(triggering["detailsVisible"], false);
    let hidden = detail(&f, "other", triggering).await;
    assert!(
        hidden.get("event").is_none() && hidden.get("steps").is_none(),
        "{hidden}"
    );
    assert!(detail(&f, OWNER, triggering).await["steps"].is_array());

    // Disabling stops its waiting triggerings at once.
    call(
        &f,
        MEMBER,
        "POST",
        &format!("/api/hook-rules/{rule}/enabled"),
        json!({ "ruleId": rule, "enabled": false }),
    )
    .await;
    assert_eq!(detail(&f, MEMBER, triggering).await["status"], "stopped");
}

#[test]
fn version_32_adds_the_engine_tables_and_reopens_without_changing_them() {
    let root = tempfile::tempdir().unwrap();
    drop(cairn_installation::store::Store::open(root.path()).unwrap());
    let db = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
    db.execute_batch(
        "DROP TABLE hook_rule_steps;
         DROP TABLE hook_rule_triggerings;
         DROP TABLE hook_rule_values;
         DROP TABLE hook_rule_quiet_hours;
         DROP TABLE hook_rule_receipts;
         PRAGMA user_version=32;",
    )
    .unwrap();

    for _ in 0..2 {
        drop(cairn_installation::store::Store::open(root.path()).unwrap());
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            33
        );
        for table in [
            "hook_rule_triggerings",
            "hook_rule_steps",
            "hook_rule_values",
            "hook_rule_quiet_hours",
            "hook_rule_receipts",
        ] {
            assert_eq!(
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
                0
            );
        }
        assert_eq!(
            db.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
}

#[tokio::test]
async fn a_service_retry_delay_longer_than_an_hour_is_respected() {
    let f = granted().await;
    let rule = rule(
        &f,
        OWNER,
        steps(&json!([post(
            "refused",
            &f,
            &json!({
                "retry": { "type": "literal", "value": true },
                "retryAfter": { "type": "literal", "value": 7200.125 },
            })
        )])),
        60,
    )
    .await;
    let before = cairn_installation::config::now();
    emit(
        &f,
        "long-retry",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let listed = history(&f, OWNER, &rule).await;
    let triggering = &listed["triggerings"][0];
    assert_eq!(triggering["status"], "retrying", "{listed}");
    assert!(triggering["resumesAt"].as_i64().unwrap() >= before + 7_200_125);
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn plugin_tools_receive_only_the_agents_public_identity() {
    let f = granted().await;
    let rule = rule(
        &f,
        OWNER,
        steps(&json!([post(
            "identity",
            &f,
            &json!({
                "agentView": { "type": "literal", "value": true },
            })
        )])),
        60,
    )
    .await;
    emit(
        &f,
        "identity",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let listed = history(&f, OWNER, &rule).await;
    let detail = detail(&f, OWNER, &listed["triggerings"][0]).await;
    let agent =
        f.s.store
            .agent_snapshot(MAIN_AGENT_ID)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        detail["steps"][0]["result"],
        json!({ "id": MAIN_AGENT_ID, "name": agent["name"] })
    );
}

#[tokio::test]
async fn a_deferred_listener_does_not_replay_another_rules_quiet_triggering() {
    let f = Fixture::new().await;
    f.set(json!([contribution("hook", "message", json!([]))]))
        .await;
    let quiet = rule(&f, OWNER, steps(&json!([])), 60).await;
    let agent =
        f.s.agent_fixture(json!({ "name": "Restricted listener" }), None)
            .await
            .unwrap();
    let agent_id = text(&agent, "id");
    f.set_for(
        agent_id,
        json!([contribution("hook", "message", support())]),
    )
    .await;
    let (_, source) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{quiet}"),
        Value::Null,
    )
    .await;
    let mut definition = source["definition"].clone();
    definition["executionAgentId"] = agent_id.into();
    let (status, other) = call(
        &f,
        OWNER,
        "POST",
        "/api/hook-rules",
        json!({ "definition": definition }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{other}");

    // Unknown resources make the restricted listener defer, while the broad grant
    // can receive this event without asking the resource resolver.
    emit(
        &f,
        "deferred",
        json!({ "channel": "unknown", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    engine::tick(&f.s).await.unwrap();
    assert_eq!(history(&f, OWNER, &quiet).await["withoutEffect"], 1);
    let (queued, receipts) =
        f.s.store
            .read(|db| {
                Ok((
                    db.0.query_row("SELECT count(*) FROM plugin_hook_events", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    db.0.query_row("SELECT count(*) FROM hook_rule_receipts", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                ))
            })
            .await
            .unwrap();
    assert_eq!((queued, receipts), (1, 1));
}

#[tokio::test]
async fn slow_tools_do_not_hold_up_new_hook_intake_or_other_executor_slots() {
    let f = granted().await;
    rule(&f, OWNER, steps(&json!([post("post", &f, &json!({}))])), 60).await;
    f.pause.store(true, Ordering::SeqCst);
    emit(
        &f,
        "slow",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    let service = f.s.clone();
    let worker = tokio::spawn(async move { engine::tick(&service).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), f.entered.notified())
        .await
        .unwrap();
    emit(
        &f,
        "new",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    let progressed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while f.posts.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    f.release.notify_one();
    worker.await.unwrap().unwrap();
    assert!(progressed.is_ok(), "A tool in flight blocked a new event.");
}

#[tokio::test]
async fn all_terminal_paths_are_pruned_without_losing_the_hourly_accounting() {
    let f = granted().await;
    let rule = rule(
        &f,
        OWNER,
        steps(&json!([post("post", &f, &json!({}))])),
        250,
    )
    .await;
    let id = rule.clone();
    f.s.store.transaction(move |db| {
        let at = cairn_installation::config::now();
        for index in 0..250 {
            let event = format!("failed-{index}");
            db.0.execute(
                "INSERT INTO hook_rule_triggerings(id,rule_id,version,event_id,event,status,cursor,in_flight,actions,tool_calls,counted_at,verify,created_at,updated_at,finished_at) \
                 VALUES(?1,?2,1,?3,'{}','failed','{\"frames\":[],\"steps\":{},\"count\":1}',0,1,1,?4,0,?5,?4,?4)",
                rusqlite::params![uuid::Uuid::new_v4().to_string(), id, event, at, at - 1000 + index],
            )?;
            db.0.execute("INSERT INTO hook_rule_receipts VALUES(?1,?2,?3)", rusqlite::params![id, event, at])?;
        }
        db.0.execute("INSERT INTO hook_rule_quiet_hours VALUES(?1,?2,1)", rusqlite::params![id, at / 3_600_000 - 24 * 8])?;
        Ok(())
    }).await.unwrap();
    engine::tick(&f.s).await.unwrap();
    let retained =
        f.s.store
            .read(|db| {
                Ok((
                    db.0.query_row("SELECT count(*) FROM hook_rule_triggerings", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    db.0.query_row("SELECT count(*) FROM hook_rule_receipts", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    db.0.query_row("SELECT count(*) FROM hook_rule_quiet_hours", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                ))
            })
            .await
            .unwrap();
    assert_eq!(retained, (200, 250, 0));
    emit(
        &f,
        "over-cap",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    let (_, current) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{rule}"),
        Value::Null,
    )
    .await;
    assert_eq!(current["blocks"][0]["kind"], "budgetExhausted");
}

#[tokio::test]
async fn retries_share_the_waiting_limit_and_reject_delays_over_seven_days() {
    let f = granted().await;
    let rule = rule(
        &f,
        OWNER,
        steps(&json!([post(
            "refused",
            &f,
            &json!({
                "retry": { "type": "literal", "value": true },
                "retryAfter": { "type": "formula", "expression": "payload.delay" },
            })
        )])),
        60,
    )
    .await;
    emit(
        &f,
        "too-long",
        json!({ "channel": "support", "author": "alice", "delay": 30 * 86_400 }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let listed = history(&f, OWNER, &rule).await;
    let failed = detail(&f, OWNER, &listed["triggerings"][0]).await;
    assert_eq!(failed["status"], "failed", "{failed}");
    assert!(failed["error"].as_str().unwrap().contains("seven days"));
    for index in 0..21 {
        emit(
            &f,
            &format!("retry-{index}"),
            json!({ "channel": "support", "author": "alice", "delay": 3600 }),
        )
        .await;
    }
    engine::tick(&f.s).await.unwrap();
    let listed = history(&f, OWNER, &rule).await;
    let retrying = listed["triggerings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["status"] == "retrying")
        .count();
    assert_eq!(retrying, 20, "{listed}");
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_member_retry_waits_for_fresh_membership_before_it_calls_the_tool() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let rule = rule(
        &f,
        MEMBER,
        steps(&json!([post(
            "refused",
            &f,
            &json!({
                "retry": { "type": "literal", "value": true },
                "retryAfter": { "type": "literal", "value": 3600 },
            })
        )])),
        60,
    )
    .await;
    emit(
        &f,
        "member-retry",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(
        history(&f, OWNER, &rule).await["triggerings"][0]["status"],
        "retrying"
    );
    f.marker("accept").await;
    common::relay_fixture::authority_unavailable(&f.s, true).await;
    // Restarting removes the in-memory freshness observation while preserving the
    // pending action and the last authoritative membership snapshot.
    let restarted = cairn_installation::service::Service::new(f.s.config.clone())
        .await
        .unwrap();
    restarted
        .plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    restarted.plugins.route_http_to(Some(f.http_origin.clone()));
    restarted
        .store
        .transaction(|db| {
            db.0.execute(
                "UPDATE hook_rule_triggerings SET next_at=0 WHERE status='retrying'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    engine::tick(&restarted).await.unwrap();
    let waiting = history(&f, OWNER, &rule).await;
    assert_eq!(waiting["triggerings"][0]["status"], "waiting", "{waiting}");
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    common::relay_fixture::authority_unavailable(&f.s, false).await;
    restarted.synchronize_task_authors().await.unwrap();
    restarted
        .store
        .transaction(|db| {
            db.0.execute(
                "UPDATE hook_rule_triggerings SET next_at=0 WHERE status='waiting'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    engine::tick(&restarted).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        history(&f, OWNER, &rule).await["triggerings"][0]["status"],
        "completed"
    );
}

#[tokio::test]
async fn an_unreadable_rule_does_not_block_other_listeners() {
    let f = granted().await;
    let broken = rule(&f, OWNER, steps(&json!([])), 60).await;
    let working = rule(&f, OWNER, steps(&json!([post("post", &f, &json!({}))])), 60).await;
    f.s.store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE hook_rule_versions SET definition='{}' WHERE rule_id=?",
                [broken],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    emit(
        &f,
        "valid-listener",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        history(&f, OWNER, &working).await["triggerings"][0]["status"],
        "completed"
    );
}

async fn hold_first_tool(
    f: &Fixture,
    event: &str,
) -> tokio::task::JoinHandle<cairn_installation::error::Result<usize>> {
    f.pause.store(true, Ordering::SeqCst);
    emit(f, event, json!({ "channel": "support", "author": "alice" })).await;
    let service = f.s.clone();
    let worker = tokio::spawn(async move { engine::tick(&service).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), f.entered.notified())
        .await
        .unwrap();
    worker
}

fn wait_step(seconds: u32) -> Value {
    json!({
        "type": "action", "id": "pause",
        "action": { "type": "wait", "seconds": { "type": "literal", "value": seconds } },
    })
}

#[tokio::test]
async fn absent_optional_payload_aliases_are_null_in_conditions_and_templates() {
    let f = granted().await;
    let rule = rule(&f, OWNER, json!([{
        "type": "if", "id": "noThread",
        "condition": { "type": "compare", "left": { "type": "formula", "expression": "thread" }, "operator": "absent" },
        "then": [post("post", &f, &json!({ "author": { "type": "template", "text": "discord:{{thread}}" } }))],
        "otherwise": [],
    }]), 60).await;
    emit(
        &f,
        "without-thread",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let listed = history(&f, OWNER, &rule).await;
    let detail = detail(&f, OWNER, &listed["triggerings"][0]).await;
    assert_eq!(detail["status"], "completed", "{detail}");
    assert_eq!(detail["steps"][0]["input"]["author"], "discord:");
    emit(
        &f,
        "with-thread",
        json!({ "channel": "support", "author": "alice", "thread": "support-thread" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert_eq!(history(&f, OWNER, &rule).await["withoutEffect"], 1);
}

#[tokio::test]
async fn lifting_a_non_exhausted_budget_does_not_reset_the_hourly_count() {
    let f = granted().await;
    let rule = rule(&f, OWNER, steps(&json!([post("post", &f, &json!({}))])), 1).await;
    emit(
        &f,
        "first-budget",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let (status, _) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/hook-rules/{rule}/budget"),
        json!({ "ruleId": rule }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    emit(
        &f,
        "second-budget",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    let (_, current) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{rule}"),
        Value::Null,
    )
    .await;
    assert_eq!(current["blocks"][0]["kind"], "budgetExhausted");
}

#[tokio::test]
async fn resuming_a_rule_does_not_revive_triggerings_stopped_during_a_tool_call() {
    for (endpoint, field, blocked, resumed) in [
        ("enabled", "enabled", false, true),
        ("emergency-pause", "paused", true, false),
    ] {
        let f = granted().await;
        let rule = rule(
            &f,
            OWNER,
            steps(&json!([
                post("first", &f, &json!({})),
                post("second", &f, &json!({})),
            ])),
            60,
        )
        .await;
        let worker = hold_first_tool(&f, "stop-resume").await;
        for state in [blocked, resumed] {
            let mut body = json!({ "ruleId": rule });
            body[field] = json!(state);
            let (status, response) = call(
                &f,
                OWNER,
                "POST",
                &format!("/api/hook-rules/{rule}/{endpoint}"),
                body,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{response}");
            assert_eq!(
                history(&f, OWNER, &rule).await["triggerings"][0]["status"],
                "stopped"
            );
        }
        f.release.notify_one();
        worker.await.unwrap().unwrap();
        assert_eq!(f.posts.load(Ordering::SeqCst), 1, "{endpoint}");
        let listed = history(&f, OWNER, &rule).await;
        let stopped = detail(&f, OWNER, &listed["triggerings"][0]).await;
        assert_eq!(stopped["status"], "stopped");
        assert_eq!(stopped["steps"][0]["status"], "completed");
        emit(
            &f,
            "new-after-resume",
            json!({ "channel": "support", "author": "alice" }),
        )
        .await;
        engine::tick(&f.s).await.unwrap();
        assert_eq!(f.posts.load(Ordering::SeqCst), 3);
    }
}

#[tokio::test]
async fn deleting_a_rule_during_a_tool_call_removes_its_history_and_prevents_the_next_action() {
    let f = granted().await;
    let rule = rule(
        &f,
        OWNER,
        steps(&json!([
            post("first", &f, &json!({})),
            post("second", &f, &json!({})),
        ])),
        60,
    )
    .await;
    let worker = hold_first_tool(&f, "delete-running").await;
    let (status, deleted) = call(
        &f,
        OWNER,
        "DELETE",
        &format!("/api/hook-rules/{rule}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    f.release.notify_one();
    worker.await.unwrap().unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    let remaining =
        f.s.store
            .read(|db| {
                Ok((
                    db.0.query_row("SELECT count(*) FROM hook_rule_triggerings", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    db.0.query_row("SELECT count(*) FROM hook_rule_steps", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    db.0.query_row("SELECT count(*) FROM hook_rule_receipts", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                ))
            })
            .await
            .unwrap();
    assert_eq!(remaining, (0, 0, 0));
}

#[tokio::test]
async fn old_versions_stop_when_their_creators_access_is_lost_even_after_an_edit() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let rule = rule(
        &f,
        MEMBER,
        steps(&json!([wait_step(600), post("post", &f, &json!({}))])),
        60,
    )
    .await;
    emit(
        &f,
        "old-version",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let old = history(&f, OWNER, &rule).await["triggerings"][0].clone();
    assert_eq!(old["status"], "waiting");
    let agent =
        f.s.agent_fixture(json!({ "name": "Replacement" }), None)
            .await
            .unwrap();
    let agent_id = text(&agent, "id");
    common::relay_fixture::allow_member(&f.s, MEMBER, agent_id).await;
    f.set_for(
        agent_id,
        json!([
            contribution("hook", "message", support()),
            contribution("tool", "grant_probe_post", support()),
        ]),
    )
    .await;
    let path = format!("/api/hook-rules/{rule}");
    let (_, current) = call(&f, MEMBER, "GET", &path, Value::Null).await;
    let mut definition = current["definition"].clone();
    definition["executionAgentId"] = agent_id.into();
    let (status, edited) = call(
        &f,
        MEMBER,
        "PUT",
        &path,
        json!({ "ruleId": rule, "revision": current["revision"], "definition": definition }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["version"], 2);
    let access = format!("/api/agents/{MAIN_AGENT_ID}/accounts/{MEMBER}");
    let (status, response) = call(&f, OWNER, "DELETE", &access, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    engine::tick(&f.s).await.unwrap();
    assert_eq!(detail(&f, OWNER, &old).await["status"], "stopped");
    let (status, _) = call(&f, OWNER, "PUT", &access, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    engine::tick(&f.s).await.unwrap();
    assert_eq!(detail(&f, OWNER, &old).await["status"], "stopped");
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_end_of_a_creators_membership_stops_waiting_work() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let rule = rule(
        &f,
        MEMBER,
        steps(&json!([wait_step(600), post("post", &f, &json!({}))])),
        60,
    )
    .await;
    emit(
        &f,
        "membership-end",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(
        history(&f, OWNER, &rule).await["triggerings"][0]["status"],
        "waiting"
    );
    common::relay_fixture::remove_member(&f.s, MEMBER).await;
    f.s.synchronize_task_authors().await.unwrap();
    assert_eq!(
        history(&f, OWNER, &rule).await["triggerings"][0]["status"],
        "stopped"
    );
    let (_, current) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{rule}"),
        Value::Null,
    )
    .await;
    assert!(
        current["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["kind"] == "membershipEnded")
    );
}

#[tokio::test]
async fn a_claim_error_does_not_abort_another_triggerings_in_flight_tool() {
    let f = granted().await;
    rule(&f, OWNER, json!([{
        "type": "if", "id": "which",
        "condition": { "type": "authorIn", "author": { "type": "formula", "expression": "author" }, "people": [{ "resourceType": "person", "id": "alice" }] },
        "then": [post("slow", &f, &json!({}))],
        "otherwise": [wait_step(1), post("afterWait", &f, &json!({}))],
    }]), 60).await;
    emit(
        &f,
        "wait-before-fault",
        json!({ "channel": "support", "author": "bob" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let worker = hold_first_tool(&f, "slow-during-fault").await;
    let attempted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = attempted.clone();
    f.s.store.transaction(move |db| {
        db.0.create_scalar_function("fail_claim", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| -> rusqlite::Result<i64> {
            signal.store(true, Ordering::SeqCst);
            Err(rusqlite::Error::UserFunctionError(Box::new(std::io::Error::other("synthetic claim failure"))))
        })?;
        db.0.execute_batch("CREATE TRIGGER fail_claim BEFORE UPDATE ON hook_rule_triggerings WHEN OLD.status='waiting' AND NEW.status='running' BEGIN SELECT fail_claim(); END;")?;
        Ok(())
    }).await.unwrap();
    let observed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !attempted.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    f.s.store
        .transaction(|db| {
            db.0.execute_batch("DROP TRIGGER fail_claim")?;
            Ok(())
        })
        .await
        .unwrap();
    f.release.notify_one();
    let result = worker.await.unwrap();
    assert!(observed.is_ok(), "The fault was not exercised.");
    result.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 2);
    let states =
        f.s.store
            .read(|db| {
                Ok(db.0.query_row(
                    "SELECT count(*) FROM hook_rule_triggerings WHERE status='completed'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(states, 2);
}

#[tokio::test]
async fn conversation_actions_and_predicates_fail_explicitly_until_ticket_g() {
    for nodes in [
        json!([{
            "type": "action",
            "id": "start",
            "action": {
                "type": "startAgent",
                "agentId": MAIN_AGENT_ID,
                "prompt": { "type": "literal", "value": "hello" },
            },
        }]),
        json!([{
            "type": "if",
            "id": "exists",
            "condition": {
                "type": "conversationExists",
                "key": { "type": "literal", "value": "key" },
            },
            "then": [],
            "otherwise": [],
        }]),
    ] {
        let f = granted().await;
        let rule = rule(&f, OWNER, nodes, 60).await;
        emit(
            &f,
            "conversation-stub",
            json!({ "channel": "support", "author": "alice" }),
        )
        .await;
        engine::tick(&f.s).await.unwrap();
        let listed = history(&f, OWNER, &rule).await;
        let failed = detail(&f, OWNER, &listed["triggerings"][0]).await;
        assert_eq!(failed["status"], "failed", "{failed}");
        assert!(
            failed["error"]
                .as_str()
                .unwrap()
                .contains("conversation keys"),
            "{failed}"
        );
    }
}

fn remember_seen() -> Value {
    json!({
        "type": "action", "id": "remember",
        "action": {
            "type": "storeValue", "scope": "rule", "name": "seen",
            "value": { "type": "literal", "value": true },
        },
    })
}

async fn has_seen(f: &Fixture, rule: &str) -> bool {
    let rule = rule.to_owned();
    f.s.store.read(move |db| {
        Ok(db.0.query_row("SELECT EXISTS(SELECT 1 FROM hook_rule_values WHERE rule_id=? AND name='seen' AND value='true')", [rule], |row| row.get::<_, bool>(0))?)
    }).await.unwrap()
}

#[tokio::test]
async fn a_slow_resolver_does_not_block_other_listeners_or_new_instances() {
    let f = Fixture::new().await;
    f.marker("live-parent").await;
    let other = common::save_plugin(
        &f.s,
        "grant-probe",
        &json!({ "name": "Fast", "enabled": true }),
        None,
    )
    .await
    .unwrap();
    let other_id = text(&other, "id");
    let current = f.api("GET", json!({})).await.unwrap();
    f.api("PUT", json!({
        "agentId": MAIN_AGENT_ID,
        "revision": current["revision"],
        "grants": [
            { "instanceId": f.instance, "access": { "type": "selected", "contributions": [contribution("hook", "message", json!([]))] } },
            { "instanceId": other_id, "access": { "type": "selected", "contributions": [contribution("hook", "message", json!([]))] } },
        ],
        "runCredentialInstances": [],
    })).await.unwrap();
    let fast = rule(&f, OWNER, json!([remember_seen()]), 60).await;
    let agent =
        f.s.agent_fixture(json!({ "name": "Resolving" }), None)
            .await
            .unwrap();
    let agent_id = text(&agent, "id");
    f.set_for(
        agent_id,
        json!([contribution("hook", "message", support())]),
    )
    .await;
    let (_, source) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{fast}"),
        Value::Null,
    )
    .await;
    let mut slow_definition = source["definition"].clone();
    slow_definition["executionAgentId"] = agent_id.into();
    let (status, response) = call(
        &f,
        OWNER,
        "POST",
        "/api/hook-rules",
        json!({ "definition": slow_definition }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let mut other_definition = source["definition"].clone();
    other_definition["trigger"]["instanceId"] = other_id.into();
    let (status, other_rule) = call(
        &f,
        OWNER,
        "POST",
        "/api/hook-rules",
        json!({ "definition": other_definition }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{other_rule}");
    let other_rule = text(&other_rule, "id");
    f.pause.store(true, Ordering::SeqCst);
    emit(
        &f,
        "slow-resolution",
        json!({ "channel": "support", "thread": "support-thread", "author": "alice" }),
    )
    .await;
    let service = f.s.clone();
    let worker = tokio::spawn(async move { engine::tick(&service).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), f.entered.notified())
        .await
        .unwrap();
    emit_on(
        &f,
        other_id,
        "new-instance",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    let progress = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !has_seen(&f, &fast).await || !has_seen(&f, other_rule).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    f.release.notify_one();
    worker.await.unwrap().unwrap();
    assert!(
        progress.is_ok(),
        "Resource resolution held up independent rule work."
    );
}

#[tokio::test]
async fn transient_delivery_resolution_is_backed_off() {
    let f = granted().await;
    f.marker("resolver-unavailable").await;
    rule(&f, OWNER, json!([remember_seen()]), 60).await;
    emit(
        &f,
        "unavailable",
        json!({ "channel": "support", "author": "alice" }),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let attempts = f.posts.load(Ordering::SeqCst);
    assert!(attempts > 0);
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), attempts);
}

#[tokio::test]
async fn an_idle_ruleset_does_not_need_the_sqlite_writer() {
    let f = granted().await;
    rule(&f, OWNER, json!([remember_seen()]), 60).await;
    let mut writer = rusqlite::Connection::open_with_flags(
        f.s.config.data_dir.join("manager.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .unwrap();
    let locked = writer
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), engine::tick(&f.s)).await;
    drop(locked);
    result
        .expect("Idle rule polling acquired the writer.")
        .unwrap();
}

#[tokio::test]
async fn reported_plugin_failures_stop_the_tree_and_only_confirmed_retries_wait() {
    for (response, expected) in [
        (
            json!({ "ok": false, "status": 403, "error": "permission_denied" }),
            "failed",
        ),
        (
            json!({
                "ok": false,
                "retryable": false,
                "status": 503,
                "retryAfter": "3600",
                "error": "request_not_confirmed",
                "rfcMessageId": "fixture@example.test",
            }),
            "failed",
        ),
        (
            json!({
                "ok": false,
                "retryable": true,
                "status": 429,
                "retryAfter": "3600",
                "error": "rate_limited",
            }),
            "retrying",
        ),
        (
            json!({
                "ok": false,
                "retryable": true,
                "status": 503,
                "error": "temporarily_unavailable",
            }),
            "retrying",
        ),
    ] {
        let f = granted().await;
        let rule = rule(
            &f,
            OWNER,
            steps(&json!([
                post(
                    "reported",
                    &f,
                    &json!({ "result": { "type": "literal", "value": response } })
                ),
                post("mustNotRun", &f, &json!({})),
            ])),
            60,
        )
        .await;
        let at = cairn_installation::config::now();
        emit(
            &f,
            "reported-failure",
            json!({ "channel": "support", "author": "alice" }),
        )
        .await;
        engine::tick(&f.s).await.unwrap();
        let listed = history(&f, OWNER, &rule).await;
        let result = detail(&f, OWNER, &listed["triggerings"][0]).await;
        assert_eq!(result["status"], expected, "{result}");
        assert_eq!(result["steps"][0]["result"], response);
        assert_eq!(result["steps"].as_array().unwrap().len(), 1);
        assert_eq!(f.posts.load(Ordering::SeqCst), 0);
        if expected == "retrying" && response.get("retryAfter").is_some() {
            assert!(result["resumesAt"].as_i64().unwrap() >= at + 3_600_000);
        }
    }
}

#[tokio::test]
async fn ignored_member_events_do_not_fill_waiting_slots_during_a_membership_outage() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let rule = rule(
        &f,
        MEMBER,
        steps(&json!([post("post", &f, &json!({}))])),
        60,
    )
    .await;
    common::relay_fixture::authority_unavailable(&f.s, true).await;
    emit(
        &f,
        "ignored-during-outage",
        json!({ "channel": "support", "author": "bob" }),
    )
    .await;
    let restarted = cairn_installation::service::Service::new(f.s.config.clone())
        .await
        .unwrap();
    restarted
        .plugins
        .use_worker(std::path::PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    restarted.plugins.route_http_to(Some(f.http_origin.clone()));
    engine::tick(&restarted).await.unwrap();
    let listed = history(&f, OWNER, &rule).await;
    assert!(
        listed["triggerings"].as_array().unwrap().is_empty(),
        "{listed}"
    );
    assert_eq!(listed["withoutEffect"], 1);
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
}
