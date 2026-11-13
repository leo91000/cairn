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
    let instance =
        f.s.store
            .plugin_instance(&f.instance)
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
