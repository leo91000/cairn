//! Integration tests for ADR-0094: Clock ("Horloge") hook rules, overlap guard
//! (`RuleConversationWorking`), manual `run_now` & `preview_schedule`, outcome
//! simplification (`completed` / `blocked`), and SQLite v38 -> v39 migration.
mod common;

use axum::{Router, body::Body, http::StatusCode};
use cairn_installation::{
    config::{MAIN_AGENT_ID, id, now},
    store::Store,
    validation::text,
};
use common::{ProjectFixture, RelayContext, relay_fixture::router, run_fixture::DbRunFixture};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, sync::Arc};
use tempfile::TempDir;

struct App {
    _root: TempDir,
    router: Router,
    service: Arc<cairn_installation::service::Service>,
    owner: RelayContext,
    member: RelayContext,
}

impl App {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        let service = cairn_installation::service::Service::new(common::config(root.path()))
            .await
            .unwrap();
        let router = router(service.clone()).await.unwrap();
        let owner = RelayContext::new(&common::relay_fixture::context(&service).await);
        let member = RelayContext::new(
            &common::relay_fixture::member_context(&service, "fixture-member").await,
        );
        Self {
            _root: root,
            router,
            service,
            owner,
            member,
        }
    }

    async fn request(
        &self,
        who: &RelayContext,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let request = who
            .authorize(common::request(method, path))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = common::send(&self.router, request).await;
        (response.status(), common::read_json(response).await)
    }

    async fn wait_for_clock_admission(&self, rule: &str, scheduled: i64) {
        common::eventually(
            std::time::Duration::from_secs(20),
            std::time::Duration::from_millis(20),
            async || {
                cairn_installation::rules::engine::tick(&self.service)
                    .await
                    .unwrap();
                let rule = rule.to_owned();
                let next: Option<i64> = self
                    .service
                    .store
                    .read(move |db| {
                        Ok(db.0.query_row(
                            "SELECT next_run FROM hook_rule_schedules WHERE rule_id=?",
                            [&rule],
                            |row| row.get(0),
                        )?)
                    })
                    .await
                    .unwrap();
                (next != Some(scheduled)).then_some(())
            },
        )
        .await;
        cairn_installation::rules::engine::tick(&self.service)
            .await
            .unwrap();
    }
}

fn schedule_rule_definition(name: &str, trigger: &Value) -> Value {
    json!({
        "name": name,
        "description": "ADR-0094 test schedule",
        "executionAgentId": MAIN_AGENT_ID,
        "trigger": trigger,
        "limits": cairn_contracts::HookRuleLimits::default(),
        "steps": [
            {
                "type": "if",
                "id": "skip_if_working",
                "condition": {
                    "type": "not",
                    "condition": { "type": "ruleConversationWorking" }
                },
                "then": [
                    {
                        "type": "action",
                        "id": "start_agent",
                        "action": {
                            "type": "startAgent",
                            "agentId": MAIN_AGENT_ID,
                            "prompt": { "type": "literal", "value": "Check inbox and summarize." }
                        }
                    }
                ],
                "otherwise": []
            }
        ]
    })
}

#[tokio::test]
async fn clock_rejects_dates_outside_the_rfc3339_year_range() {
    let app = App::new().await;
    let trigger = json!({
        "type": "clock",
        "at": cairn_contracts::MAX_CLOCK_DATE_MILLIS + 1,
        "timezone": "UTC",
    });
    let (status, result) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "definition": schedule_rule_definition("Invalid date", &trigger) }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
}

#[tokio::test]
async fn clock_rules_cannot_bind_or_steer_a_conversation_for_reuse() {
    let app = App::new().await;
    let mut definition = schedule_rule_definition(
        "Fresh daily conversation",
        &json!({ "type": "clock", "cron": "0 9 * * *", "timezone": "UTC" }),
    );
    let key = json!({ "type": "literal", "value": "daily" });
    definition["steps"][0]["then"][0]["action"]["key"] = key.clone();
    definition["steps"].as_array_mut().unwrap().push(json!({
        "type": "action",
        "id": "steer_daily",
        "action": {
            "type": "steerConversation",
            "key": key,
            "message": { "type": "literal", "value": "Review the updates." },
        },
    }));
    let input = json!({ "definition": definition });
    let (status, validation) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules/validate",
            input.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{validation}");
    let problems = validation["problems"].as_array().unwrap();
    for path in ["/steps/0/then/0/action/key", "/steps/1/action"] {
        assert!(
            problems.iter().any(|problem| problem["path"] == path),
            "{validation}"
        );
    }
    let (status, result) = app
        .request(&app.owner, "POST", "/api/hook-rules", input)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
}

#[tokio::test]
async fn a_past_one_shot_cannot_create_an_automatic_run() {
    let app = App::new().await;
    let definition = schedule_rule_definition(
        "Past schedule",
        &json!({ "type": "clock", "at": now() - 60_000, "timezone": "UTC" }),
    );
    let (status, validation) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules/validate",
            json!({ "definition": definition }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{validation}");
    assert!(
        validation["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|problem| problem["path"] == "/trigger/at")
    );
    let (status, result) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "definition": definition }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");

    let (status, saved) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "definition": definition, "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let rule_id = text(&saved, "id");
    let (status, enabled) = app
        .request(
            &app.owner,
            "POST",
            &format!("/api/hook-rules/{rule_id}/enabled"),
            json!({ "ruleId": rule_id, "enabled": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert!(enabled["nextRun"].is_null());
    assert_eq!(
        cairn_installation::rules::engine::tick(&app.service)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn a_deleted_clock_rule_keeps_its_conversation_origin_identity() {
    let app = App::new().await;
    let creation = id();
    let trigger = json!({ "type": "clock", "cron": "0 9 * * *", "timezone": "UTC" });
    let definition = schedule_rule_definition("Original schedule", &trigger);
    let (status, created) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "creationId": creation, "definition": definition }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["id"], creation);
    let path = format!("/api/hook-rules/{creation}");
    let (status, started) = app
        .request(&app.owner, "POST", &format!("{path}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    cairn_installation::rules::engine::tick(&app.service)
        .await
        .unwrap();
    let (status, chats) = app
        .request(&app.owner, "GET", "/api/chats", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{chats}");
    assert!(
        chats
            .as_array()
            .unwrap()
            .iter()
            .any(|chat| chat["originRule"]["ruleId"] == creation)
    );
    let (status, deleted) = app
        .request(&app.owner, "DELETE", &path, json!({ "ruleId": creation }))
        .await;
    assert!(status.is_success(), "{deleted}");

    let replacement = schedule_rule_definition("Different schedule", &trigger);
    let (status, reused) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "creationId": creation, "definition": replacement }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reused}");
}

#[tokio::test]
async fn clock_rules_prevent_removing_their_selected_project() {
    let app = App::new().await;
    let project = app
        .service
        .project_fixture(
            json!({ "name": "Used by Clock", "path": app._root.path() }),
            None,
        )
        .await
        .unwrap();
    let mut definition = schedule_rule_definition(
        "Project schedule",
        &json!({ "type": "clock", "cron": "0 9 * * *", "timezone": "UTC" }),
    );
    definition["steps"][0]["then"][0]["action"]["projectId"] = project["id"].clone();
    let (status, rule) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "definition": definition }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    let path = format!("/api/projects/{}", text(&project, "id"));
    let (status, refused) = app.request(&app.owner, "DELETE", &path, Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(
        text(&refused, "error").contains("used by a schedule"),
        "{refused}"
    );
}

#[tokio::test]
async fn run_now_checks_the_current_membership_before_admission() {
    let app = App::new().await;
    let member = "fixture-member";
    common::relay_fixture::allow_member(&app.service, member, MAIN_AGENT_ID).await;
    let definition = schedule_rule_definition(
        "Member's schedule",
        &json!({ "type": "clock", "cron": "0 9 * * *", "timezone": "UTC" }),
    );
    let (status, rule) = app
        .request(
            &app.member,
            "POST",
            "/api/hook-rules",
            json!({ "definition": definition }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    let rule_id = text(&rule, "id").to_owned();
    let path = format!("/api/hook-rules/{rule_id}/run");

    let (status, admitted) = app.request(&app.member, "POST", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{admitted}");

    common::relay_fixture::authority_unavailable(&app.service, true).await;
    let (status, refused) = app.request(&app.member, "POST", &path, Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{refused}");
    common::relay_fixture::authority_unavailable(&app.service, false).await;

    // Beacon accepted a new invitation without notifying the installation. The
    // earlier request's membership confirmation is still fresh in its cache.
    common::relay_fixture::add_member(&app.service, member, "new-membership").await;
    let (status, refused) = app.request(&app.member, "POST", &path, Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(
        text(&refused, "error").contains("creator left"),
        "{refused}"
    );
    let admitted_count: i64 = app
        .service
        .store
        .read(move |db| {
            Ok(db.0.query_row(
                "SELECT count(*) FROM audit WHERE action='hook_rule.run_now' AND json_extract(detail,'$.ruleId')=?",
                [&rule_id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(admitted_count, 1);
}

#[tokio::test]
async fn run_now_records_a_capacity_refusal() {
    let app = App::new().await;
    let definition = schedule_rule_definition(
        "Busy schedule",
        &json!({ "type": "clock", "cron": "0 9 * * *", "timezone": "UTC" }),
    );
    let (status, rule) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "definition": definition }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    let rule_id = text(&rule, "id").to_owned();
    let queued_rule = rule_id.clone();
    app.service
        .store
        .transaction(move |db| {
            db.0.execute(
            "WITH RECURSIVE queue(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM queue WHERE n<1000) \
             INSERT INTO hook_rule_triggerings(id,rule_id,version,event_id,event,status,cursor, \
                in_flight,actions,tool_calls,verify,created_at,updated_at) \
             SELECT 'pending-'||n,?1,1,'event-'||n,'{}','running','{}',0,0,0,0,?2,?2 FROM queue",
            params![queued_rule, now()],
        )?;
            Ok(())
        })
        .await
        .unwrap();
    let (status, refused) = app
        .request(
            &app.owner,
            "POST",
            &format!("/api/hook-rules/{rule_id}/run"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    let recorded: i64 =
        app.service
            .store
            .read(move |db| {
                Ok(db.0.query_row(
            "SELECT coalesce(sum(not_admitted),0) FROM hook_rule_quiet_hours WHERE rule_id=?",
            [&rule_id], |row| row.get(0),
        )?)
            })
            .await
            .unwrap();
    assert_eq!(recorded, 1);
}

#[tokio::test]
async fn clock_cron_rule_catches_up_once_guards_overlap_and_runs_now() {
    let app = App::new().await;

    // 1. Preview schedule validates cron + timezone.
    let (status, preview) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules/schedule/preview",
            json!({ "cron": "0 9 * * *", "timezone": "Europe/Paris", "count": 3 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["occurrences"].as_array().unwrap().len(), 3);

    let (status, _) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules/schedule/preview",
            json!({ "cron": "not a cron", "timezone": "UTC", "count": 3 }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 2. Create a cron Clock rule with !ruleConversationWorking guard.
    let def = schedule_rule_definition(
        "Morning triage",
        &json!({ "type": "clock", "cron": "*/5 * * * *", "timezone": "UTC" }),
    );
    let (status, created) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "definition": def }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let rule_id = text(&created, "id").to_owned();
    assert!(created["nextRun"].as_i64().is_some());
    assert_eq!(created["allowed"]["runNow"], true);

    // 3. Rewind next_run 2 hours into the past to simulate downtime catch-up.
    let missed = now() - 2 * 3_600_000;
    let rule_id_db = rule_id.clone();
    app.service
        .store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE hook_rule_schedules SET next_run=?1,legacy_chain_depth=8 WHERE rule_id=?2",
                params![missed, rule_id_db],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Tick the rules engine: should emit exactly ONE catch-up triggering and start 1 conversation.
    app.wait_for_clock_admission(&rule_id, missed).await;

    let (status, chats) = app
        .request(&app.owner, "GET", "/api/chats", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    let started: Vec<&Value> = chats
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["originRule"]["ruleId"] == rule_id)
        .collect();
    assert_eq!(
        started.len(),
        1,
        "single catch-up must start 1 chat: {chats}"
    );
    assert_eq!(started[0]["originRule"]["ruleName"], "Morning triage");
    assert_eq!(started[0]["originRule"]["schedule"], true);
    let first_chat_id = text(started[0], "id").to_owned();
    let first_chat_for_depth = first_chat_id.clone();
    let depth: u32 = app
        .service
        .store
        .read(move |db| {
            Ok(db.0.query_row(
                "SELECT depth FROM hook_rule_chain_depths WHERE chat_id=?",
                [&first_chat_for_depth],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(depth, 8);

    // Next run was advanced into the future (> now).
    let (status, detail) = app
        .request(
            &app.owner,
            "GET",
            &format!("/api/hook-rules/{rule_id}"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail["nextRun"].as_i64().unwrap() > now());

    // 4. Member cannot trigger someone else's rule with run_now.
    let (status, _) = app
        .request(
            &app.member,
            "POST",
            &format!("/api/hook-rules/{rule_id}/run"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let run_now_and_count = async || {
        let (status, triggering) = app
            .request(
                &app.owner,
                "POST",
                &format!("/api/hook-rules/{rule_id}/run"),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{triggering}");
        cairn_installation::rules::engine::tick(&app.service)
            .await
            .unwrap();
        let (status, chats) = app
            .request(&app.owner, "GET", "/api/chats", Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK, "{chats}");
        chats
            .as_array()
            .unwrap()
            .iter()
            .filter(|chat| chat["originRule"]["ruleId"] == rule_id)
            .count()
    };

    // A pause holds queued input, so the overlap guard still sees the first chat.
    app.service.chat_pause(&first_chat_id, true).await.unwrap();
    assert_eq!(run_now_and_count().await, 1);

    // 6. Once the first conversation finishes its turn — even with an unanswered pending
    //    question — RuleConversationWorking is false, so run_now starts a second chat!
    let first_chat_db = first_chat_id.clone();
    app.service
        .store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE chat_messages SET status='delivered' WHERE chat_id=?1",
                [&first_chat_db],
            )?;
            let run_id = id();
            db.add_run_fixture(
                &json!({
                    "id": run_id,
                    "taskId": first_chat_db,
                    "status": "succeeded",
                    "trigger": "chat",
                    "createdAt": now(),
                    "finishedAt": now(),
                }),
                None,
            )?;
            db.0.execute(
                "UPDATE conversations SET run_id=?1 WHERE id=?2",
                params![run_id, first_chat_db],
            )?;
            let wakeup = serde_json::from_value(json!({
                "id": "scheduled-review",
                "messageId": null,
                "reason": "Wait for the CI result",
                "seconds": 60,
                "createdAt": now(),
                "wakeAt": now() + 60_000,
            }))?;
            db.save_wakeup(true, &first_chat_db, Some(&wakeup))?;
            db.save_question(&serde_json::from_value(json!({
                "id": id(),
                "chatId": first_chat_db,
                "runId": run_id,
                "source": "cairn",
                "status": "pending",
                "blocking": false,
                "createdAt": now(),
                "fields": [{ "id": "f", "title": "Need input?", "mode": "single", "secret": false, "options": [] }],
            }))?)?;
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(
        run_now_and_count().await,
        1,
        "a paused wakeup still holds overlap"
    );
    app.service
        .store
        .write(move |db| db.save_wakeup(true, &first_chat_id, None))
        .await
        .unwrap();
    assert_eq!(
        run_now_and_count().await,
        2,
        "unanswered question must not block overlap guard once turn finished"
    );
}

#[tokio::test]
async fn a_clock_brief_at_the_legacy_limit_can_be_edited_and_duplicated() {
    let app = App::new().await;
    let mut definition = schedule_rule_definition(
        "Preserve the full brief",
        &json!({ "type": "clock", "cron": "0 9 * * *", "timezone": "UTC" }),
    );
    let prompt = "🗒".repeat(50_000);
    definition["steps"][0]["then"][0]["action"]["prompt"]["value"] = prompt.clone().into();
    let (status, created) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "definition": definition }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Could not save the full legacy brief"
    );
    let rule = text(&created, "id");
    definition["description"] = "Edited description".into();
    let (status, validation) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules/validate",
            json!({ "ruleId": rule, "definition": definition }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{validation}");
    assert_eq!(validation["problems"], json!([]), "{validation}");
    let (status, edited) = app
        .request(
            &app.owner,
            "PUT",
            &format!("/api/hook-rules/{rule}"),
            json!({ "ruleId": rule, "revision": created["revision"], "definition": definition }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Could not edit the full legacy brief"
    );
    assert_eq!(
        edited["definition"]["steps"][0]["then"][0]["action"]["prompt"]["value"],
        prompt
    );
    let (status, copied) = app
        .request(
            &app.owner,
            "POST",
            &format!("/api/hook-rules/{rule}/duplicate"),
            json!({ "ruleId": rule }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Could not duplicate the full legacy brief"
    );
    assert_eq!(
        copied["definition"]["steps"][0]["then"][0]["action"]["prompt"]["value"],
        prompt
    );
}

#[tokio::test]
async fn clock_single_date_fires_once_and_never_repeats() {
    let app = App::new().await;
    let target = now() + 60_000;
    let ambiguous = schedule_rule_definition(
        "Invalid clock",
        &json!({ "type": "clock", "cron": "   ", "at": target, "timezone": "UTC" }),
    );
    let (status, _) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "definition": ambiguous }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut def = schedule_rule_definition(
        "One-shot deploy check",
        &json!({ "type": "clock", "at": target, "timezone": "UTC" }),
    );
    def["steps"][0]["then"][0]["action"]["prompt"]["value"] = "/clear is part of my brief".into();
    let (status, created) = app
        .request(
            &app.owner,
            "POST",
            "/api/hook-rules",
            json!({ "definition": def }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let rule_id = text(&created, "id").to_owned();
    assert_eq!(created["nextRun"], target);

    // Move next_run into the past so it is due now.
    let due = now() - 1000;
    let rule_id_db = rule_id.clone();
    app.service
        .store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE hook_rule_schedules SET next_run=?1 WHERE rule_id=?2",
                params![due, rule_id_db],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    app.wait_for_clock_admission(&rule_id, due).await;

    let (_, detail) = app
        .request(
            &app.owner,
            "GET",
            &format!("/api/hook-rules/{rule_id}"),
            Value::Null,
        )
        .await;
    assert!(
        detail["nextRun"].is_null(),
        "one-shot must clear nextRun after firing: {detail}"
    );
    let (_, chats) = app
        .request(&app.owner, "GET", "/api/chats", Value::Null)
        .await;
    let started: Vec<&Value> = chats
        .as_array()
        .unwrap()
        .iter()
        .filter(|chat| chat["originRule"]["ruleId"] == rule_id)
        .collect();
    assert_eq!(started.len(), 1);
    let chat = text(started[0], "id");
    app.service
        .chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = app.service.chat_detail(chat).await.unwrap();
    assert_eq!(
        detail["run"]["chatExecution"]["text"],
        "Task brief:\n/clear is part of my brief"
    );
}

#[test]
fn report_outcome_rejects_needs_input_and_accepts_completed_or_blocked() {
    let schema = cairn_installation::outcome::tool()["inputSchema"].clone();
    assert!(!jsonschema::is_valid(
        &schema,
        &json!({
            "status": "needs_input",
            "reason": "Need user input",
            "evidence": ["asked"],
        }),
    ));
    assert!(jsonschema::is_valid(
        &schema,
        &json!({
            "status": "blocked",
            "reason": "Missing deployment token.",
            "evidence": ["401 from deploy API"],
        }),
    ));
    assert!(jsonschema::is_valid(
        &schema,
        &json!({
            "status": "completed",
            "reason": "All checks passed.",
            "evidence": ["cargo test ok"],
        }),
    ));
}

#[tokio::test]
async fn automatic_clock_admission_requires_live_beacon_authority_for_all_owner_versions() {
    let app = App::new().await;
    let scheduled = now() - 3_600_000;
    let mut rules = Vec::new();
    for migrated in [false, true] {
        let definition = schedule_rule_definition(
            "Scheduled review",
            &json!({ "type": "clock", "cron": "*/5 * * * *", "timezone": "UTC" }),
        );
        let (status, rule) = app
            .request(
                &app.owner,
                "POST",
                "/api/hook-rules",
                json!({ "definition": definition }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{rule}");
        let rule_id = text(&rule, "id").to_owned();
        rules.push(rule_id.clone());
        app.service
            .store
            .transaction(move |db| {
                db.0.execute(
                    "UPDATE hook_rule_schedules SET next_run=?1 WHERE rule_id=?2",
                    params![scheduled, rule_id],
                )?;
                if migrated {
                    db.0.execute(
                        "UPDATE hook_rule_versions SET source='migration' WHERE rule_id=?",
                        [&rule_id],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
    }

    let contacted = Arc::new(tokio::sync::Notify::new());
    let revoke = Arc::new(tokio::sync::Notify::new());
    let route = axum::routing::get({
        let contacted = contacted.clone();
        let revoke = revoke.clone();
        move || async move {
            contacted.notify_one();
            revoke.notified().await;
            StatusCode::UNAUTHORIZED
        }
    });
    let (origin, beacon) =
        common::serve_locally(Router::new().route("/api/relay/{installation}/task-authors", route))
            .await;
    let identity = app
        .service
        .config
        .data_dir
        .join("installation-relay/identity.json");
    std::fs::write(
        &identity,
        json!({ "origin": origin, "installationId": id(), "token": "fixture-only" }).to_string(),
    )
    .unwrap();

    let bound = std::time::Duration::from_secs(3);
    tokio::time::timeout(bound, cairn_installation::rules::engine::tick(&app.service))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(bound, contacted.notified())
        .await
        .unwrap();
    revoke.notify_one();
    for rule in rules {
        let (status, detail) = app
            .request(
                &app.owner,
                "GET",
                &format!("/api/hook-rules/{rule}"),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert_eq!(detail["nextRun"], scheduled);
    }
    let (_, chats) = app
        .request(&app.owner, "GET", "/api/chats", Value::Null)
        .await;
    assert!(chats.as_array().unwrap().is_empty());
    beacon.abort();
}

#[tokio::test]
async fn sqlite_v38_to_v39_migrates_missions_runs_reads_and_dismissals_cleanly() {
    let dir = TempDir::new().unwrap();
    let config = common::config(dir.path());
    std::fs::create_dir_all(&config.home).unwrap();
    let initial = cairn_installation::service::Service::new(config.clone())
        .await
        .unwrap();
    let agent = initial
        .store
        .read(|db| db.agent_snapshot(MAIN_AGENT_ID))
        .await
        .unwrap()
        .unwrap();
    drop(initial);
    let db_path = config.data_dir.join("manager.db");
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();

    let active_mission_id = id();
    let disabled_mission_id = id();
    let removed_mission_id = id();
    let suspended_mission_id = id();
    let oneshot_mission_id = id();
    let blocked_run_id = id();
    let waiting_run_id = id();
    let retirement_run_id = id();
    let resuming_retirement_run_id = id();
    let stopped_wait_run_id = id();
    let failed_wait_run_id = id();
    let superseded_wait_run_id = id();
    let regular_chat_id = id();
    let t0 = 1_700_000_000_000i64;
    let future_snooze = now() + 3_600_000;

    conn.execute_batch(
        "DROP TABLE IF EXISTS hook_rule_schedules;
         DROP INDEX IF EXISTS hook_rule_migrated_history;
         DROP INDEX IF EXISTS hook_rule_conversations_rule;
         DROP TABLE IF EXISTS account_snoozes;
         DROP TABLE IF EXISTS account_reads;
         DROP TABLE IF EXISTS wait_conditions;
         DROP TABLE IF EXISTS durable_wakeups;
         DROP TABLE IF EXISTS hook_rule_versions;
         DROP TABLE IF EXISTS hook_rule_uses;",
    )
    .unwrap();
    conn.execute_batch(include_str!("fixtures/v38-missions.sql"))
        .unwrap();

    // Seed owner in kv so migration knows the owner accountId.
    conn.execute(
        "INSERT OR REPLACE INTO kv VALUES('installation-claim',?1,NULL)",
        [json!({ "ownerAccountId": "owner-acct" }).to_string()],
    )
    .unwrap();

    // Seed an existing regular conversation for account_reads preservation.
    conn.execute(
        "INSERT INTO conversations(id,slug,agent_id,title,paused,lifecycle,revision,created_at,updated_at,persisted_at,session_restart_requested,cancelled_by_deletion) \
         VALUES(?1,'existing-chat',?2,'Existing chat',0,'active','r1',?3,?3,?3,0,0)",
        params![regular_chat_id, MAIN_AGENT_ID, t0],
    )
    .unwrap();

    // Seed 5 missions covering active cron, disabled/archived, removed author, suspended rights, and one-shot.
    conn.execute(
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,next_run,author_id,author_access_id,author_removed,revision,created_at,updated_at,native_authority_nodes, \
         worktree,skills_inherited,native_principals,native_provenance_unknown) \
         VALUES(?1,?2,'Daily standup','Summarize updates','0 9 * * *','Europe/Paris',1,0,?3,'owner-acct','acc-1',0,'r1',?4,?4,'[\"local\"]',1,1,'[]',0)",
        params![active_mission_id, MAIN_AGENT_ID, t0, t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO hook_rule_task_depths(task_id,depth) VALUES(?1,8)",
        [&active_mission_id],
    )
    .unwrap();
    conn.execute(
        "UPDATE missions SET failure_message='The selected node was unavailable',
           failure_scheduled_for=?2,failure_at=?3 WHERE id=?1",
        params![active_mission_id, t0 - 60_000, t0 - 59_000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,author_id,author_access_id,author_removed,revision,created_at,updated_at, \
         worktree,skills_inherited,native_principals,native_authority_nodes,native_provenance_unknown) \
         VALUES(?1,?2,'Paused mission','Do work','0 10 * * *','UTC',0,1,'owner-acct','acc-1',0,'r1',?3,?3,1,1,'[]','[]',0)",
        params![disabled_mission_id, MAIN_AGENT_ID, t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,author_id,author_access_id,author_removed,revision,created_at,updated_at, \
         worktree,skills_inherited,native_principals,native_authority_nodes,native_provenance_unknown) \
         VALUES(?1,?2,'Former member mission','Do work','0 11 * * *','UTC',1,0,'former-member','acc-2',1,'r1',?3,?3,1,1,'[]','[]',0)",
        params![removed_mission_id, MAIN_AGENT_ID, t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,author_id,author_access_id,author_removed,revision,created_at,updated_at,schedule_wait_reason, \
         worktree,skills_inherited,native_principals,native_authority_nodes,native_provenance_unknown) \
         VALUES(?1,?2,'Suspended mission','Do work','0 12 * * *','UTC',1,0,'member-acct','acc-3',0,'r1',?3,?3,'Its author no longer has access to this agent. It resumes once the owner gives access again.',1,1,'[]','[]',0)",
        params![suspended_mission_id, MAIN_AGENT_ID, t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,author_id,author_access_id,author_removed,revision,created_at,updated_at, \
         worktree,skills_inherited,native_principals,native_authority_nodes,native_provenance_unknown) \
         VALUES(?1,?2,'Manual one-shot','Run once',NULL,'UTC',1,0,'owner-acct','acc-1',0,'r1',?3,?3,1,1,'[]','[]',0)",
        params![oneshot_mission_id, MAIN_AGENT_ID, t0],
    )
    .unwrap();

    // A rule and mission could share a client-supplied UUID in v38.
    conn.execute(
        "INSERT INTO hook_rules(id,creator_account_id,creator_owner,revision,version,created_at,updated_at) \
         VALUES(?1,'owner-acct',1,'r1',1,?2,?2)",
        params![disabled_mission_id, t0],
    ).unwrap();
    conn.execute(
        "INSERT INTO hook_rule_versions VALUES(?1,1,?2,'owner-acct','app',NULL,'{}',?3)",
        params![
            disabled_mission_id,
            schedule_rule_definition(
                "Independent rule",
                &json!({ "type": "cairn", "hook": "ended" })
            )
            .to_string(),
            t0,
        ],
    )
    .unwrap();

    // A deleted rule's conversations kept its origin UUID.
    let retained_rule_chat = id();
    conn.execute(
        "INSERT INTO conversations(id,slug,agent_id,title,paused,lifecycle,revision,created_at,updated_at,persisted_at,session_restart_requested,cancelled_by_deletion) \
         VALUES(?1,'retained-rule-chat',?2,'Earlier rule conversation',0,'active','r1',?3,?3,?3,0,0)",
        params![retained_rule_chat, MAIN_AGENT_ID, t0],
    ).unwrap();
    conn.execute(
        "INSERT INTO hook_rule_conversations(chat_id,creator_account_id,agent_id,rule_id,origin,created_at) \
         VALUES(?1,'owner-acct',?2,?3,'{}',?4)",
        params![retained_rule_chat, MAIN_AGENT_ID, suspended_mission_id, t0],
    ).unwrap();

    // Seed two legacy mission runs: one finished+blocked (for active_mission_id) and one waiting with wakeup (for oneshot_mission_id).
    // Tied creation times cross the history migration's batch boundary.
    for _ in 0..35 {
        conn.execute(
            "INSERT INTO runs(id,task_id,status,trigger,created_at,finished_at,snapshot,boundary, \
             recovery_pending,resume_available,resume_count) \
             VALUES(?1,?2,'succeeded','schedule',?3,?3,?4,'{\"batchFixture\":true}',0,0,0)",
            params![
                id(),
                disabled_mission_id,
                t0,
                json!({ "agent": agent }).to_string()
            ],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,started_at,finished_at,snapshot,boundary,dedupe,summary,\
           recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'succeeded','schedule',?3,?3,?4,?5,?6,?7,'Original summary',0,0,0)",
        params![
            blocked_run_id,
            active_mission_id,
            t0,
            t0 + 5000,
            json!({ "task": { "name": "Daily standup" }, "agent": { "id": MAIN_AGENT_ID, "name": "Main" } }).to_string(),
            json!({
                "outcome": { "status": "needs_input", "reason": "Waiting on review", "evidence": ["saved report"] },
                "nativeProvenanceUnknown": false,
                "environmentState": "expired",
                "environmentExpiredAt": t0 + 6_000,
                "environmentRetirementId": "old-workspace",
            }).to_string(),
            format!("{active_mission_id}:{t0}"),
        ],
    )
    .unwrap();
    let failed_with_report = id();
    let later_blocked_run = id();
    for (run, status, finished_at) in [
        (&failed_with_report, "failed", t0 + 4_000),
        (&later_blocked_run, "succeeded", t0 + 6_000),
    ] {
        conn.execute(
            "INSERT INTO runs(id,task_id,status,trigger,created_at,finished_at,snapshot,boundary,
             recovery_pending,resume_available,resume_count)
             VALUES(?1,?2,?3,'schedule',?4,?5,?6,?7,0,0,0)",
            params![
                run, active_mission_id, status, t0, finished_at,
                json!({ "agent": agent }).to_string(),
                json!({ "outcome": { "status": "blocked", "reason": "A separate outcome", "evidence": [] } }).to_string(),
            ],
        ).unwrap();
    }
    conn.execute(
        "INSERT INTO records(kind,id,data,updated_at) VALUES('environment-retirements','old-workspace',?1,?2)",
        params![json!({
            "id": "old-workspace",
            "runId": blocked_run_id,
            "expiredAt": t0 + 6_000,
            "nodes": [],
            "isolated": false,
            "workspaces": [],
            "projects": [],
            "fenced": true,
            "local": true,
            "error": "Node unavailable",
        })
        .to_string(), t0 + 6_000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,started_at,boundary,snapshot,session_id, \
           recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'waiting','manual',?3,?3,?4,?5,'saved-session',0,1,0)",
        params![
            waiting_run_id,
            oneshot_mission_id,
            t0,
            json!({ "missionTurnId": "legacy-turn", "nativeProvenanceUnknown": false }).to_string(),
            json!({
                "agent": agent,
                "task": { "id": oneshot_mission_id, "name": "Manual one-shot", "agentId": MAIN_AGENT_ID, "prompt": "Run once", "worktree": true },
                "project": null, "projects": [], "skills": [],
            }).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO run_checkpoints(run_id,state) VALUES(?1,?2)",
        params![
            waiting_run_id,
            json!({
                "launched": true,
                "completed": true,
                "runnerId": "legacy-controller-attempt",
                "remainingMs": 10_000,
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO durable_wakeups(key,chat_id,mission_run_id,id,reason,seconds,created_at,wake_at,time_kind,time_seconds) \
         VALUES(1,NULL,?1,'w1','Wait for deploy',60,?2,?3,'delay',60)",
        params![waiting_run_id, t0, t0 + 60_000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,started_at,finished_at,boundary,
           snapshot,session_id,recovery_pending,resume_available,resume_count)
         SELECT ?1,task_id,'cancelled','schedule',created_at,started_at,?2,boundary,
           snapshot,session_id,0,0,0
         FROM runs WHERE id=?3",
        params![retirement_run_id, t0 + 30_000, waiting_run_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO run_checkpoints(run_id,state) VALUES(?1,?2)",
        params![
            retirement_run_id,
            json!({ "remainingMs": 12_000, "completed": true }).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO records(kind,id,data,updated_at) VALUES('environment-expirations',?1,?2,?3)",
        params![
            retirement_run_id,
            json!({
                "id": retirement_run_id,
                "runId": retirement_run_id,
                "expiredAt": t0 + 30_000,
                "cause": "node-change",
                "environment": {
                    "trigger": "schedule",
                    "wakeup": {
                        "id": "retired-wake",
                        "messageId": "legacy-turn",
                        "reason": "Wait through environment retirement",
                        "seconds": 60,
                        "createdAt": t0,
                        "wakeAt": t0 + 60_000,
                    },
                },
            })
            .to_string(),
            t0 + 30_000
        ],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,started_at,finished_at,boundary,
           snapshot,session_id,recovery_pending,resume_available,resume_count)
         SELECT ?1,task_id,'cancelled','schedule',created_at,started_at,finished_at,boundary,
           snapshot,session_id,0,0,0 FROM runs WHERE id=?2",
        params![resuming_retirement_run_id, retirement_run_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO records(kind,id,data,updated_at) VALUES('environment-expirations',?1,?2,?3)",
        params![
            resuming_retirement_run_id,
            json!({
                "id": resuming_retirement_run_id,
                "runId": resuming_retirement_run_id,
                "expiredAt": t0 + 30_000,
                "cause": "node-change",
                "continueTurn": true,
                "environment": { "trigger": "schedule" },
            })
            .to_string(),
            t0 + 30_000
        ],
    )
    .unwrap();

    for (run, turn, wakeup) in [
        (
            &stopped_wait_run_id,
            "stopped-migrated-turn",
            "stopped-wake",
        ),
        (&failed_wait_run_id, "failed-migrated-turn", "failed-wake"),
        (
            &superseded_wait_run_id,
            "superseded-migrated-turn",
            "superseded-wake",
        ),
    ] {
        conn.execute(
            "INSERT INTO runs(id,task_id,status,trigger,created_at,started_at,finished_at,boundary,
               snapshot,session_id,recovery_pending,resume_available,resume_count,summary)
             SELECT ?1,task_id,'cancelled','schedule',created_at,started_at,finished_at,
               json_set(boundary,'$.missionTurnId',?3),
               snapshot,session_id,0,0,0,'Partial mission result' FROM runs WHERE id=?2",
            params![run, retirement_run_id, turn],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_checkpoints(run_id,state) VALUES(?1,'{\"completed\":true,\"remainingMs\":12000}')",
            [run],
        ).unwrap();
        conn.execute(
            "INSERT INTO records(kind,id,data,updated_at)
             SELECT kind,?1,json_set(data,'$.id',?1,'$.runId',?1,'$.environment.wakeup.id',?3,
               '$.environment.wakeup.messageId',?4),updated_at FROM records WHERE id=?2",
            params![run, retirement_run_id, wakeup, turn],
        )
        .unwrap();
    }

    let queued_id = id();
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,snapshot,boundary, \
          recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'queued','schedule',?3,?4,'{}',0,0,0)",
        params![
            queued_id,
            active_mission_id,
            t0 + 7_000,
            json!({ "agent": agent, "task": { "name": "Daily standup", "prompt": "Summarize updates", "worktree": false } }).to_string(),
        ],
    ).unwrap();

    conn.execute(
        "INSERT INTO run_checkpoints(run_id,state) VALUES(?1,?2)",
        params![
            queued_id,
            json!({ "prepared": { "workspaces": [{ "kind": "direct", "path": "/legacy-project" }] } }).to_string(),
        ],
    ).unwrap();
    let queued_continuation = id();
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,snapshot,boundary, \
          recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'queued','schedule',?3,?4,?5,0,1,0)",
        params![
            queued_continuation,
            disabled_mission_id,
            t0 + 8_000,
            json!({ "agent": agent, "task": { "name": "Daily standup", "prompt": "Summarize updates" } }).to_string(),
            json!({
                "missionTurnId": "resolved-wait",
                "waitContinuation": { "reason": "Wait for the CI", "observation": { "state": "completed", "status": "succeeded" } }
            }).to_string(),
        ],
    ).unwrap();
    let mut initial_retries = Vec::new();
    for (startup, stored_retry) in [(false, true), (true, true), (false, false), (true, false)] {
        let retry_run = id();
        let mut profile = agent.clone();
        profile["provider"] = if startup { "codex" } else { "antigravity" }.into();
        let session = (!startup).then_some("saved-antigravity-session");
        conn.execute(
            "INSERT INTO runs(id,task_id,status,trigger,created_at,started_at,snapshot,boundary,
              recovery_pending,resume_available,resume_count,session_id,retry_attempt,retry_limit,
              retry_next_at,retry_cause)
             VALUES(?1,?2,?8,'schedule',?3,?3,?4,'{}',1,1,0,?5,?9,?10,?6,?7)",
            params![
                retry_run,
                id(),
                t0 + 8_500,
                json!({ "agent": profile, "task": { "name": "Antigravity retry",
                    "prompt": "Prepare the original report." } })
                .to_string(),
                session,
                stored_retry.then_some(t0 + 60_000),
                stored_retry.then(|| json!({
                    "kind": if startup { "timeout" } else { "rate-limit" },
                    "message": if startup { "Codex initialize request timed out." } else { "Provider capacity" },
                    "startup": startup,
                }).to_string()),
                if stored_retry { "queued" } else { "running" },
                stored_retry.then_some(1),
                stored_retry.then_some(5),
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_checkpoints(run_id,state) VALUES(?1,?2)",
            params![
                retry_run,
                json!({
                    "launched": true,
                    "launchedTurn": retry_run,
                    "antigravityPaidTurn": if startup { None } else { Some(retry_run.as_str()) }
                })
                .to_string()
            ],
        )
        .unwrap();
        initial_retries.push((retry_run, startup, stored_retry));
    }

    let converted_run = id();
    let original_run_created_at = t0 - 86_400_000;
    let original_run_finished_at = original_run_created_at + 1000;
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,finished_at,snapshot,boundary, \
          recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'failed','chat',?3,?4,?5,?6,0,0,0)",
        params![
            converted_run,
            regular_chat_id,
            original_run_created_at,
            t0 + 1000,
            json!({ "agent": agent, "task": { "name": "Existing chat", "prompt": "Old brief", "worktree": false } }).to_string(),
            json!({
                "sourceTaskId": active_mission_id,
                "sourceTaskPrompt": "Old brief",
                "chatExecution": {
                    "messageId": "legacy-compaction",
                    "operation": "compact",
                    "previous": {
                        "summary": "Previous ordinary reply",
                        "finishedAt": t0 + 500,
                        "outcome": {
                            "status": "needs_input",
                            "reason": "Needs the saved input",
                            "evidence": ["retained compaction evidence"],
                        },
                    },
                },
            }).to_string(),
        ],
    ).unwrap();
    conn.execute(
        "INSERT INTO events(run_id,created_at,type,text) VALUES(?1,?2,'status','succeeded')",
        params![converted_run, original_run_finished_at],
    )
    .unwrap();
    conn.execute(
        "UPDATE conversations SET run_id=?1 WHERE id=?2",
        params![converted_run, regular_chat_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO run_checkpoints(run_id,state) VALUES(?1,?2)",
        params![
            converted_run,
            json!({ "prepared": { "workspaces": [{ "kind": "direct" }] } }).to_string()
        ],
    )
    .unwrap();

    // Removing a mission did not remove a conversation previously converted from it.
    let deleted_mission_id = id();
    let converted_orphan = id();
    let converted_orphan_run = id();
    conn.execute(
        "INSERT INTO hook_rules(id,creator_account_id,creator_owner,revision,version,created_at,updated_at) \
         VALUES(?1,'owner-acct',1,'r1',1,?2,?2)",
        params![deleted_mission_id, t0],
    ).unwrap();
    conn.execute(
        "INSERT INTO hook_rule_versions VALUES(?1,1,?2,'owner-acct','app',NULL,'{}',?3)",
        params![
            deleted_mission_id,
            schedule_rule_definition(
                "Unrelated rule",
                &json!({ "type": "cairn", "hook": "ended" })
            )
            .to_string(),
            t0,
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,finished_at,snapshot,boundary, \
          recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'succeeded','chat',?3,?3,?4,?5,0,0,0)",
        params![
            converted_orphan_run,
            converted_orphan,
            t0,
            json!({ "agent": agent, "task": { "name": "Deleted mission" } }).to_string(),
            json!({ "sourceTaskId": deleted_mission_id }).to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO conversations(id,slug,agent_id,title,run_id,paused,lifecycle,revision,created_at,updated_at,persisted_at,session_restart_requested,cancelled_by_deletion) \
         VALUES(?1,'renamed-chat',?2,'Renamed chat',?3,0,'active','r1',?4,?4,?4,0,0)",
        params![converted_orphan, MAIN_AGENT_ID, converted_orphan_run, t0],
    ).unwrap();
    let unconverted_orphan_run = id();
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,finished_at,snapshot,boundary, \
          recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'succeeded','schedule',?3,?3,?4,'{}',0,0,0)",
        params![
            unconverted_orphan_run,
            deleted_mission_id,
            t0,
            json!({ "agent": agent, "task": { "name": "Deleted mission" } }).to_string(),
        ],
    )
    .unwrap();

    let pending_wait_run_id = id();
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,started_at,session_id,snapshot,boundary,
          recovery_pending,resume_available,resume_count)
         VALUES(?1,?2,'queued','schedule',?3,?3,'fencing-session',?4,'{}',1,1,0)",
        params![
            pending_wait_run_id,
            deleted_mission_id,
            t0 + 12_000,
            json!({ "agent": agent, "task": { "name": "Deleted mission" } }).to_string(),
        ],
    ).unwrap();
    conn.execute(
        "INSERT INTO run_checkpoints(run_id,state) VALUES(?1,?2)",
        params![
            pending_wait_run_id,
            json!({
                "launched": true,
                "completed": true,
                "settled": {
                    "status": "waiting",
                    "summary": null,
                    "finishedAt": null,
                },
            })
            .to_string(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO durable_wakeups(key,chat_id,mission_run_id,id,reason,seconds,created_at,wake_at,time_kind,time_seconds)
         VALUES(2,NULL,?1,'fenced-wait','Wait while the old attempt fences',60,?2,?3,'delay',60)",
        params![pending_wait_run_id, t0 + 12_000, t0 + 72_000],
    ).unwrap();

    let interrupted_run = id();
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,started_at,finished_at,session_id,snapshot,boundary, \
          recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'interrupted','manual',?3,?3,?3,'old-session',?4,'{}',0,1,0)",
        params![
            interrupted_run,
            suspended_mission_id,
            t0 + 9_000,
            json!({ "agent": agent, "task": { "name": "Interrupted mission", "prompt": "/clear this is a brief" } }).to_string(),
        ],
    ).unwrap();
    conn.execute(
        "INSERT INTO run_checkpoints(run_id,state) VALUES(?1,?2)",
        params![
            interrupted_run,
            json!({ "launched": true, "launchedTurn": "" }).to_string()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO run_checkpoints(run_id,state) VALUES(?1,?2)",
        params![
            queued_continuation,
            json!({ "launched": true, "launchedTurn": "", "turnPending": true }).to_string(),
        ],
    )
    .unwrap();

    // Seed legacy account_reads & account_dismissals (1 active snooze, 1 permanent dismiss).
    conn.execute(
        "INSERT INTO account_reads VALUES('owner-acct','chat',?1,?2)",
        params![regular_chat_id, t0 + 1000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_reads VALUES('owner-acct','mission',?1,?2)",
        params![active_mission_id, t0 + 5000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_reads VALUES('opened-acct','mission',?1,?2)",
        params![active_mission_id, t0 + 5000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_reads VALUES('opened-acct','mission',?1,?2)",
        params![deleted_mission_id, t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_dismissals VALUES('owner-acct',?1,?2,?3)",
        params![active_mission_id, t0 + 5000, future_snooze],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_dismissals VALUES('member-acct',?1,?2,NULL)",
        params![active_mission_id, t0 + 6000],
    )
    .unwrap();

    conn.pragma_update(None, "user_version", 38).unwrap();
    drop(conn);

    // Open through Store::open to run the v38 -> v39 migration.
    let store = Store::open(&config.data_dir).unwrap();
    drop(store);

    let migrated = Connection::open(&db_path).unwrap();
    let version: i64 = migrated
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 39);
    let quick: String = migrated
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(quick, "ok");
    let fk_bad = migrated
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap();
    assert!(!fk_bad, "foreign_key_check must pass after v38->v39");

    // Retired tables are gone.
    for retired in [
        "missions",
        "mission_tags",
        "mission_skills",
        "hook_rule_task_depths",
        "account_dismissals",
    ] {
        assert!(
            !migrated.table_exists(None, retired).unwrap(),
            "{retired} should be dropped"
        );
    }

    // All 5 missions became hook_rules with source='migration'.
    let rule_count: i64 = migrated
        .query_row(
            "SELECT count(*) FROM hook_rule_versions WHERE source='migration'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rule_count, 5);
    let batched: i64 = migrated
        .query_row(
            "SELECT count(*) FROM runs r JOIN conversations c ON c.run_id=r.id \
         WHERE r.trigger='chat' AND json_extract(r.boundary,'$.batchFixture')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(batched, 35);

    // Active cron mission preserves schedule & native authority.
    let (disabled, membership_ended, owner): (Option<i64>, Option<i64>, bool) = migrated
        .query_row(
            "SELECT disabled_at,membership_ended_at,creator_owner FROM hook_rules WHERE id=?1",
            [&active_mission_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(disabled.is_none());
    assert!(membership_ended.is_none());
    assert!(
        !owner,
        "only a verified Beacon observation can confer owner authority"
    );
    let (next_run, legacy_depth): (Option<i64>, u32) = migrated
        .query_row(
            "SELECT next_run,legacy_chain_depth FROM hook_rule_schedules WHERE rule_id=?1",
            [&active_mission_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(next_run, Some(t0));
    assert_eq!(legacy_depth, 8);
    assert!(
        migrated.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='index' AND name='hook_rule_migrated_history')",
            [],
            |row| row.get::<_, bool>(0),
        ).unwrap()
    );
    assert!(
        migrated
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM hook_rule_receipts WHERE rule_id=?1 AND event_id=?2)",
                params![active_mission_id, format!("clock:{active_mission_id}:{t0}")],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    );
    let limits: (u32, u32) = migrated
        .query_row(
            "SELECT json_extract(definition,'$.limits.triggeringsPerHour'),
                    json_extract(definition,'$.limits.conversationsPerHour')
             FROM hook_rule_versions WHERE rule_id=? AND version=1",
            [&active_mission_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(limits, (100, 100));
    let failure: (String, String, i64, String) = migrated
        .query_row(
            "SELECT status,error,finished_at,json_extract(event,'$.payload.scheduledAt')
         FROM hook_rule_triggerings WHERE rule_id=?",
            [&active_mission_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(failure.0, "failed");
    assert_eq!(failure.1, "The selected node was unavailable");
    assert_eq!(failure.2, t0 - 59_000);
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&failure.3)
            .unwrap()
            .timestamp_millis(),
        t0 - 60_000
    );

    // Disabled/archived -> disabled_at set; removed author -> membership_ended_at set;
    // a suspended author still needs current agent access; one-shot -> disabled_at set (no cron).
    let (replacement, disabled_at): (String, Option<i64>) = migrated
        .query_row(
            "SELECT r.id,r.disabled_at FROM hook_rules r JOIN hook_rule_versions v ON v.rule_id=r.id \
             WHERE v.source='migration' AND json_extract(v.definition,'$.name')='Paused mission'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_ne!(replacement, disabled_mission_id);
    assert!(disabled_at.is_some());
    let removed_at: Option<i64> = migrated
        .query_row(
            "SELECT membership_ended_at FROM hook_rules WHERE id=?1",
            [&removed_mission_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(removed_at.is_some());
    let (suspended_rule_id, creator, owner): (String, String, bool) = migrated
        .query_row(
            "SELECT r.id,r.creator_account_id,r.creator_owner FROM hook_rules r \
             JOIN hook_rule_versions v ON v.rule_id=r.id AND v.version=r.version \
             WHERE json_extract(v.definition,'$.name')='Suspended mission'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((creator, owner), ("member-acct".into(), false));
    assert_ne!(suspended_rule_id, suspended_mission_id);
    let retained_origin: String = migrated
        .query_row(
            "SELECT rule_id FROM hook_rule_conversations WHERE chat_id=?",
            [&retained_rule_chat],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained_origin, suspended_mission_id);
    let oneshot_disabled: Option<i64> = migrated
        .query_row(
            "SELECT disabled_at FROM hook_rules WHERE id=?1",
            [&oneshot_mission_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(oneshot_disabled.is_some());

    // Both legacy mission runs were converted into conversations linked via hook_rule_conversations.
    let blocked_chat_id: String = migrated
        .query_row(
            "SELECT id FROM conversations WHERE run_id=?1",
            [&blocked_run_id],
            |row| row.get(0),
        )
        .unwrap();
    let linked_rule: String = migrated
        .query_row(
            "SELECT rule_id FROM hook_rule_conversations WHERE chat_id=?1",
            [&blocked_chat_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(linked_rule, active_mission_id);
    let retired: (String, i64, String) = migrated.query_row(
        "SELECT environment_state,environment_expired_at,environment_error FROM conversations WHERE id=?",
        [&blocked_chat_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        retired,
        ("expired".into(), t0 + 6_000, "Node unavailable".into())
    );
    let run_task_id: String = migrated
        .query_row(
            "SELECT task_id FROM runs WHERE id=?1",
            [&blocked_run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(run_task_id, blocked_chat_id);

    // Waiting mission run's durable_wakeup now points at its migrated conversation!
    let waiting_chat_id: String = migrated
        .query_row(
            "SELECT id FROM conversations WHERE run_id=?1",
            [&waiting_run_id],
            |row| row.get(0),
        )
        .unwrap();
    let wakeup_chat_id: String = migrated
        .query_row(
            "SELECT chat_id FROM durable_wakeups WHERE key=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(wakeup_chat_id, waiting_chat_id);
    let waiting_status: String = migrated
        .query_row(
            "SELECT status FROM runs WHERE id=?1",
            [&waiting_run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(waiting_status, "succeeded");
    let pending_settlement: (String, bool, String, i64) = migrated
        .query_row(
            "SELECT r.status,r.recovery_pending,json_extract(k.state,'$.settled.status'),
                json_extract(k.state,'$.settled.finishedAt')
         FROM runs r JOIN run_checkpoints k ON k.run_id=r.id WHERE r.id=?",
            [&pending_wait_run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        pending_settlement,
        ("queued".into(), true, "succeeded".into(), t0 + 12_000),
    );

    // Reads & snoozes migrated cleanly:
    // - regular chat read preserved
    // - owner active snooze -> account_snoozes on blocked_chat_id
    // - member permanent dismiss -> account_reads on the later blocked run
    let regular_read: i64 = migrated
        .query_row(
            "SELECT finished_at FROM account_reads WHERE account_id='owner-acct' AND chat_id=?1",
            [&regular_chat_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(regular_read, t0 + 1000);
    let converted: (String, String, bool, Option<String>) = migrated.query_row(
        "SELECT c.run_id,h.rule_id,json_extract(r.snapshot,'$.task.worktree'),json_extract(k.state,'$.prepared') \
         FROM conversations c JOIN hook_rule_conversations h ON h.chat_id=c.id \
         JOIN runs r ON r.id=c.run_id JOIN run_checkpoints k ON k.run_id=r.id WHERE c.id=?",
        [&regular_chat_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(
        converted,
        (converted_run.clone(), active_mission_id.clone(), true, None)
    );
    let saved_compaction_outcome: String = migrated
        .query_row(
            "SELECT json_extract(boundary,'$.chatExecution.previous.outcome') FROM runs WHERE id=?",
            [&converted_run],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&saved_compaction_outcome).unwrap(),
        json!({
            "status": "blocked",
            "reason": "Needs the saved input",
            "evidence": ["retained compaction evidence"],
        })
    );
    let previous_origin: (String, String) = migrated.query_row(
        "SELECT rule_id,json_extract(origin,'$.ruleName') FROM hook_rule_conversations WHERE chat_id=?",
        [&converted_orphan],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_ne!(previous_origin.0, deleted_mission_id);
    assert_eq!(previous_origin.1, "Deleted mission");
    let new_origin: String = migrated
        .query_row(
            "SELECT rule_id FROM hook_rule_conversations WHERE run_id=?",
            [&unconverted_orphan_run],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(new_origin, previous_origin.0);
    let owner_snooze: i64 = migrated
        .query_row(
            "SELECT snoozed_until FROM account_snoozes WHERE account_id='owner-acct' AND chat_id=?1",
            [&blocked_chat_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owner_snooze, future_snooze);
    let queued: (String, String) = migrated
        .query_row(
            "SELECT task_id,json_extract(boundary,'$.chatExecution.text') FROM runs WHERE id=?",
            [&queued_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(queued.1.contains("Summarize updates"));
    let uses_worktree: bool = migrated
        .query_row(
            "SELECT json_extract(snapshot,'$.task.worktree') FROM runs WHERE id=?",
            [&queued_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(uses_worktree);
    let prepared: Option<String> = migrated
        .query_row(
            "SELECT json_extract(state,'$.prepared') FROM run_checkpoints WHERE run_id=?",
            [&queued_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(prepared.is_none());
    let continuation: String = migrated
        .query_row(
            "SELECT json_extract(boundary,'$.chatExecution.text') FROM runs WHERE id=?",
            [&queued_continuation],
            |row| row.get(0),
        )
        .unwrap();
    assert!(continuation.contains("Wait for the CI") && continuation.contains("succeeded"));
    for (retry_run, startup, stored_retry) in &initial_retries {
        let mut run = cairn_installation::store::Db(&migrated)
            .run(retry_run)
            .unwrap()
            .unwrap();
        assert_eq!(run["sourceTaskInitialTurn"], true);
        assert!(text(&run["chatExecution"], "text").contains("Continue the same task"));
        if !stored_retry {
            assert!(run["retry"].is_null());
            // Model the retry the worker schedules after recovering the refusal.
            run["retry"] = json!({
                "attempt": 1, "limit": 5, "nextAttemptAt": t0 + 60_000,
                "cause": {
                    "kind": if *startup { "timeout" } else { "rate-limit" },
                    "message": if *startup { "Codex initialize request timed out." } else { "Provider capacity" },
                    "startup": startup,
                },
            });
        }
        if *startup {
            // Model the run reloaded after mark_running consumes the startup slot.
            run["status"] = "running".into();
            run["retry"]["nextAttemptAt"] = Value::Null;
        }
        let plan = cairn_installation::run_output::chat_plan(
            &run,
            &json!({ "cwd": "/fixture/workspace", "output": "/fixture/output/events.json" }),
            dir.path(),
            &json!({}),
            run["sessionId"].as_str(),
        )
        .unwrap();
        assert_eq!(plan["execution"]["messageId"], *retry_run);
        assert_eq!(
            plan["execution"]["text"], "Task brief:\nPrepare the original report.",
            "startup={startup}, pre_migration_retry={stored_retry}, provider={}, status={}, retry={}, authorized={}",
            plan["provider"], run["status"], run["retry"], plan["antigravityRetryAuthorized"],
        );
        assert_eq!(plan["execution"]["recovery"], false);
        if *startup {
            assert_eq!(plan["provider"], "codex");
            assert!(plan["antigravityRetryAuthorized"].is_null());
        } else {
            assert_eq!(plan["provider"], "antigravity");
            assert_eq!(run["retry"]["nextAttemptAt"], t0 + 60_000);
            assert_eq!(plan["antigravityRetryAuthorized"], true);
        }
    }
    let interrupted: (bool, String, String) = migrated
        .query_row(
            "SELECT json_extract(r.boundary,'$.chatExecution.recovery'), \
         json_extract(r.boundary,'$.chatExecution.text'),json_extract(k.state,'$.launchedTurn') \
         FROM runs r JOIN run_checkpoints k ON k.run_id=r.id WHERE r.id=?",
            [&interrupted_run],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(interrupted.0);
    assert!(interrupted.1.contains("verify external effects"));
    assert_eq!(interrupted.2, interrupted_run);
    let fresh_continuation: bool = migrated
        .query_row(
            "SELECT json_extract(boundary,'$.chatExecution.recovery') FROM runs WHERE id=?",
            [&queued_continuation],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!fresh_continuation);

    let queued_snooze_count: i64 = migrated
        .query_row(
            "SELECT count(*) FROM account_snoozes WHERE chat_id=?",
            [&queued.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued_snooze_count, 0);
    let outcome: String = migrated
        .query_row(
            "SELECT json_extract(boundary,'$.outcome.status') FROM runs WHERE id=?",
            [&blocked_run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "blocked");

    let member_read: i64 = migrated
        .query_row(
            "SELECT r.finished_at FROM account_reads r JOIN conversations c ON c.id=r.chat_id
             WHERE r.account_id='member-acct' AND c.run_id=?1",
            [&later_blocked_run],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(member_read, t0 + 6000);
    let member_read_older_question: bool = migrated.query_row(
        "SELECT EXISTS(SELECT 1 FROM account_reads WHERE account_id='member-acct' AND chat_id=?1)",
        [&blocked_chat_id], |row| row.get(0),
    ).unwrap();
    assert!(
        !member_read_older_question,
        "v38 never permanently dismissed needs_input"
    );
    let failed_read: i64 = migrated
        .query_row(
            "SELECT r.finished_at FROM account_reads r JOIN conversations c ON c.id=r.chat_id
         WHERE r.account_id='opened-acct' AND c.run_id=?1",
            [&failed_with_report],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(failed_read, t0 + 5000);
    let failed_snooze: bool = migrated
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM account_snoozes s JOIN conversations c ON c.id=s.chat_id
         WHERE s.account_id='owner-acct' AND c.run_id=?1)",
            [&failed_with_report],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !failed_snooze,
        "v38 applied snoozes only to successful results"
    );
    let merely_opened: bool = migrated
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM account_reads WHERE account_id='opened-acct' AND chat_id=?1)",
            [&blocked_chat_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !merely_opened,
        "Opening a blocked mission did not dismiss it."
    );
    let ordinary_read: i64 = migrated
        .query_row(
            "SELECT r.finished_at FROM account_reads r JOIN conversations c ON c.id=r.chat_id
             WHERE r.account_id='opened-acct' AND c.run_id=?1",
            [&unconverted_orphan_run],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ordinary_read, t0);

    for retired in [&retirement_run_id, &resuming_retirement_run_id] {
        let provisional: Option<String> = migrated.query_row(
            "SELECT json_extract(origin,'$.legacyRun.status') FROM hook_rule_conversations WHERE run_id=?",
            [retired], |row| row.get(0),
        ).unwrap();
        assert!(
            provisional.is_none(),
            "pending retirement can continue the original occurrence"
        );
    }
    drop(migrated);
    let service = cairn_installation::service::Service::new(config)
        .await
        .unwrap();
    let app = router(service.clone()).await.unwrap();
    let owner = RelayContext::new(&common::relay_fixture::context(&service).await);
    let get = async |who: &RelayContext, path: &str| {
        let request = who
            .authorize(common::request("GET", path))
            .body(Body::empty())
            .unwrap();
        let response = common::send(&app, request).await;
        let status = response.status();
        let body = common::read_json(response).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    };
    let retired_run = retirement_run_id.clone();
    let retirement_chat_id = service
        .store
        .read(move |db| Ok(db.conversation_for_run(&retired_run)?.unwrap().id))
        .await
        .unwrap();
    let retirement_chat = service.chat_detail(&retirement_chat_id).await.unwrap();
    assert_eq!(retirement_chat["wakeup"]["id"], "retired-wake");
    let retired_history = get(
        &owner,
        &format!("/api/hook-rule-triggerings/{retirement_chat_id}"),
    )
    .await;
    assert_eq!(retired_history["status"], "waiting");
    assert!(retired_history["finishedAt"].is_null());
    let pending_resume = service
        .store
        .run(&resuming_retirement_run_id)
        .await
        .unwrap();
    let resumed_history = format!(
        "/api/hook-rule-triggerings/{}",
        text(&pending_resume, "taskId")
    );
    let pending_history = get(&owner, &resumed_history).await;
    assert_eq!(pending_history["status"], "running");
    assert!(pending_history["finishedAt"].is_null());
    service
        .chat_send(
            &retirement_chat_id,
            json!({ "id": id(), "text": "Follow up without waiting", "mode": "queue" }),
        )
        .await
        .unwrap();
    service.detach_stopped_environments().await.unwrap();
    let resuming_retired = service
        .store
        .run(&resuming_retirement_run_id)
        .await
        .unwrap();
    assert_eq!(resuming_retired["status"], "queued");
    let resumed = get(&owner, &resumed_history).await;
    assert_eq!(resumed["status"], "running");
    assert!(resumed["finishedAt"].is_null());
    let stopping_run = service.store.run(&stopped_wait_run_id).await.unwrap();
    let stopped_wait_chat_id = text(&stopping_run, "taskId");
    let stopped_history_path = format!("/api/hook-rule-triggerings/{stopped_wait_chat_id}");
    assert_eq!(
        get(&owner, &stopped_history_path).await["status"],
        "waiting"
    );
    let stop = owner
        .authorize(common::request(
            "POST",
            &format!("/api/chats/{stopped_wait_chat_id}/stop"),
        ))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let response = common::send(&app, stop).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_json(response).await
    );
    let stopped_live = service.chat_detail(stopped_wait_chat_id).await.unwrap();
    assert_eq!(stopped_live["run"]["status"], "succeeded");
    assert!(stopped_live["wakeup"].is_null());
    let stopped_occurrence = get(&owner, &stopped_history_path).await;
    assert_eq!(stopped_occurrence["status"], "stopped");
    assert_eq!(
        stopped_occurrence["event"]["payload"]["migratedConversation"]["summary"],
        "Partial mission result"
    );
    let failed_wait = service.store.run(&failed_wait_run_id).await.unwrap();
    let failed_history_path = format!(
        "/api/hook-rule-triggerings/{}",
        text(&failed_wait, "taskId")
    );
    assert_eq!(get(&owner, &failed_history_path).await["status"], "waiting");
    service
        .store
        .change_run(
            &failed_wait_run_id,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Failed),
                error: Some(Some("Recovery could not retain the native session".into())),
                recovery_pending: Some(false),
                finished_at: Some(Some(now())),
                wakeup: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let failed_occurrence = get(&owner, &failed_history_path).await;
    assert_eq!(failed_occurrence["status"], "failed");
    assert!(service.store.run(&failed_wait_run_id).await.unwrap()["wakeup"].is_null());
    assert_eq!(
        failed_occurrence["error"],
        "Recovery could not retain the native session"
    );
    let superseded_run = service.store.run(&superseded_wait_run_id).await.unwrap();
    let superseded_chat = text(&superseded_run, "taskId");
    let superseded_history = format!("/api/hook-rule-triggerings/{superseded_chat}");
    assert_eq!(get(&owner, &superseded_history).await["status"], "waiting");
    let request = owner
        .authorize(common::request(
            "POST",
            &format!("/api/chats/{superseded_chat}/messages"),
        ))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "id": id(), "text": "Continue with a different request", "mode": "queue" })
                .to_string(),
        ))
        .unwrap();
    let response = common::send(&app, request).await;
    let status = response.status();
    let reply = common::read_json(response).await;
    assert!(status.is_success(), "{reply}");
    let superseded = get(&owner, &superseded_history).await;
    assert_eq!(superseded["status"], "stopped");
    assert_eq!(
        superseded["event"]["payload"]["migratedConversation"]["summary"],
        "Partial mission result"
    );
    let retirement_chat = service.chat_detail(&retirement_chat_id).await.unwrap();
    assert!(retirement_chat["wakeup"].is_null());
    assert_eq!(
        retirement_chat["messages"][0]["text"],
        "Follow up without waiting"
    );
    let cleanup = service
        .worker
        .cleanup(&service, &retirement_run_id)
        .await
        .unwrap_err();
    assert_eq!(cleanup.status, 409);
    assert!(cleanup.message.contains("pending questions and messages"));
    // An overdue migrated schedule must neither act without Beacon's
    // membership confirmation nor keep the engine waiting on an unavailable Beacon.
    let contacted = Arc::new(tokio::sync::Notify::new());
    let confirm = Arc::new(tokio::sync::Notify::new());
    let policy_route = axum::routing::get({
        let contacted = contacted.clone();
        let confirm = confirm.clone();
        move || {
            let contacted = contacted.clone();
            let confirm = confirm.clone();
            async move {
                contacted.notify_one();
                confirm.notified().await;
                axum::Json(cairn_protocol::TaskAuthorPolicy {
                    owner: cairn_protocol::TaskAuthorGrant {
                        account_id: "owner-acct".into(),
                        access_id: "acc-1".into(),
                    },
                    members: vec![],
                })
            }
        }
    });
    let (origin, stalled_beacon) = common::serve_locally(
        Router::new().route("/api/relay/{installation}/task-authors", policy_route),
    )
    .await;
    let relay = service.config.data_dir.join("installation-relay");
    std::fs::create_dir_all(&relay).unwrap();
    let identity = relay.join("identity.json");
    std::fs::write(
        &identity,
        json!({ "origin": origin, "installationId": id(), "token": "fixture-only" }).to_string(),
    )
    .unwrap();
    std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o600)).unwrap();
    let admission_bound = std::time::Duration::from_secs(3);
    tokio::time::timeout(
        admission_bound,
        cairn_installation::rules::engine::tick(&service),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(admission_bound, contacted.notified())
        .await
        .unwrap();
    let schedule_rule_id = active_mission_id.clone();
    let schedule: i64 = service
        .store
        .read(move |db| {
            Ok(db.0.query_row(
                "SELECT next_run FROM hook_rule_schedules WHERE rule_id=?",
                [&schedule_rule_id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(schedule, next_run.unwrap());
    confirm.notify_one();
    let next_slot = common::eventually(
        admission_bound,
        std::time::Duration::from_millis(10),
        async || {
            let rule = active_mission_id.clone();
            service.store.read(move |db| {
                let next: i64 = db.0.query_row(
                    "SELECT next_run FROM hook_rule_schedules WHERE rule_id=?1",
                    [&rule],
                    |row| row.get(0),
                )?;
                let duplicate: bool = db.0.query_row(
                    "SELECT EXISTS(SELECT 1 FROM hook_rule_triggerings WHERE rule_id=?1 AND event_id=?2)",
                    params![rule, format!("clock:{rule}:{t0}")],
                    |row| row.get(0),
                )?;
                Ok((next > t0).then_some((next, duplicate)))
            }).await.unwrap()
        },
    ).await;
    assert!(next_slot.0 > now());
    assert!(!next_slot.1, "a v38 admission already delivered this slot");
    stalled_beacon.abort();
    common::relay_fixture::claimed(&service).await.unwrap();
    let detail = service.chat_detail(&waiting_chat_id).await.unwrap();
    assert_eq!(detail["run"]["chatExecution"]["messageId"], "legacy-turn");
    assert_eq!(detail["wakeup"]["messageId"], "legacy-turn");
    assert_eq!(detail["run"]["recoveryPending"], true);
    let waiting_history = get(
        &owner,
        &format!("/api/hook-rule-triggerings/{waiting_chat_id}"),
    )
    .await;
    assert_eq!(waiting_history["status"], "waiting");
    assert!(waiting_history["finishedAt"].is_null());
    assert_eq!(waiting_history["resumesAt"], detail["wakeup"]["wakeAt"]);
    service
        .chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let fenced = service.chat_detail(&waiting_chat_id).await.unwrap();
    assert_eq!(fenced["status"], "succeeded");
    assert_eq!(fenced["wakeup"]["id"], "w1");

    // The fixture runs without a worker. Model completion of controller
    // fencing before checking wakeup admission.
    let fenced_id = waiting_run_id.clone();
    service
        .store
        .transaction(move |db| {
            db.0.execute(
                "UPDATE run_checkpoints SET state=json_remove(state,'$.runnerId') WHERE run_id=?",
                [&fenced_id],
            )?;
            db.change_run(
                &fenced_id,
                cairn_installation::runs::RunChanges {
                    recovery_pending: Some(false),
                    ..Default::default()
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let cleanup = service
        .worker
        .cleanup(&service, &waiting_run_id)
        .await
        .unwrap_err();
    assert_eq!(cleanup.status, 409);
    assert!(cleanup.message.contains("scheduled continuation"));
    service
        .chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let resumed = service.store.run(&waiting_run_id).await.unwrap();
    let after = service.chat_detail(&waiting_chat_id).await.unwrap();
    assert_eq!(
        resumed["status"], "queued",
        "wake: {}, error: {}",
        after["wakeup"], after["error"]
    );
    assert_ne!(resumed["chatExecution"]["messageId"], "legacy-turn");
    assert!(text(&resumed["chatExecution"], "text").contains("Wait for deploy"));
    let checkpoint = service
        .store
        .checkpoint_boundary(&waiting_run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint["remainingMs"], 10_000);
    assert!(checkpoint.get("migratedMissionWakeupId").is_none());

    let stale_resume = owner
        .authorize(common::request(
            "POST",
            &format!("/api/runs/{waiting_run_id}/resume"),
        ))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"turnId":"legacy-turn"}"#))
        .unwrap();
    let response = common::send(&app, stale_resume).await;
    let status = response.status();
    let refusal = common::read_json(response).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
    assert!(
        text(&refusal, "error").contains("This turn changed"),
        "{refusal}"
    );
    let member =
        RelayContext::new(&common::relay_fixture::member_context(&service, "history-reader").await);
    let history = format!("/api/hook-rules/{active_mission_id}/triggerings");
    let entry = format!("/api/hook-rule-triggerings/{blocked_chat_id}");
    let list = get(&owner, &history).await;
    assert!(
        list["triggerings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["id"] == blocked_chat_id && entry["status"] == "completed" })
    );
    service
        .store
        .change_run(
            &blocked_run_id,
            cairn_installation::runs::RunChanges {
                summary: Some("Later reply".into()),
                boundary: Some(json!({ "outcome": null })),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let retained = get(&owner, &entry).await;
    assert_eq!(
        retained["event"]["payload"]["migratedConversation"]["id"],
        blocked_chat_id
    );
    assert_eq!(
        retained["event"]["payload"]["migratedConversation"]["runStatus"],
        "succeeded"
    );
    let hidden = get(&member, &entry).await;
    let original = &retained["event"]["payload"]["migratedConversation"];
    assert_eq!(original["summary"], "Original summary");
    assert_eq!(original["brief"], "Summarize updates");
    assert_eq!(
        original["outcome"],
        json!({ "status": "blocked", "reason": "Waiting on review", "evidence": ["saved report"] })
    );
    assert_eq!(hidden["detailsVisible"], false);
    assert!(hidden["event"].is_null());
    let converted_entry = get(
        &owner,
        &format!("/api/hook-rule-triggerings/{regular_chat_id}"),
    )
    .await;
    assert_eq!(converted_entry["status"], "completed");
    assert_eq!(converted_entry["createdAt"], original_run_created_at);
    assert_eq!(converted_entry["finishedAt"], original_run_finished_at);

    // Both an already-admitted run and an automatic continuation of a legacy
    // wait settle in v39. A later reply reuses the run without rewriting history.
    let first_finish = now() - 2_000;
    let later_finish = first_finish + 1_000;
    for (run_id, chat_id) in [(&queued_id, &queued.0), (&waiting_run_id, &waiting_chat_id)] {
        let current = service.store.run(run_id).await.unwrap();
        let wake_at = now() + 60_000;
        let continued_wait: cairn_contracts::ChatWakeup = serde_json::from_value(json!({
            "id": id(),
            "messageId": current["chatExecution"]["messageId"],
            "reason": "Check again before finishing",
            "seconds": 60,
            "createdAt": wake_at - 60_000,
            "wakeAt": wake_at,
        }))
        .unwrap();
        let current_chat = chat_id.clone();
        let current_run = run_id.clone();
        service
            .store
            .transaction(move |db| {
                db.save_wakeup(true, &current_chat, Some(&continued_wait))?;
                db.change_run(
                    &current_run,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Succeeded),
                        finished_at: Some(Some(wake_at - 60_000)),
                        recovery_pending: Some(false),
                        ..Default::default()
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let history_path = format!("/api/hook-rule-triggerings/{chat_id}");
        let still_waiting = get(&owner, &history_path).await;
        assert_eq!(still_waiting["status"], "waiting", "{still_waiting}");
        assert_eq!(still_waiting["resumesAt"], wake_at);
        assert!(still_waiting["finishedAt"].is_null());
        let current_chat = chat_id.clone();
        let current_run = run_id.clone();
        service
            .store
            .transaction(move |db| {
                db.save_wakeup(true, &current_chat, None)?;
                db.change_run(
                    &current_run,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Queued),
                        finished_at: Some(None),
                        ..Default::default()
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();
        for (status, finished_at) in [
            (cairn_contracts::RunStatus::Succeeded, Some(first_finish)),
            (cairn_contracts::RunStatus::Queued, None),
            (cairn_contracts::RunStatus::Failed, Some(later_finish)),
        ] {
            service.store.change_run(run_id, cairn_installation::runs::RunChanges {
                status: Some(status),
                recovery_pending: Some(false),
                finished_at: Some(finished_at),
                summary: Some(if finished_at == Some(first_finish) { "First result" } else { "Later state" }.into()),
                boundary: Some(json!({
                    "outcome": (finished_at == Some(first_finish)).then(|| json!({ "status": "completed", "reason": "Original work finished", "evidence": [] })),
                })),
                ..Default::default()
            }).await.unwrap();
        }
        let history = get(&owner, &history_path).await;
        assert_eq!(history["status"], "completed", "{history}");
        assert_eq!(history["finishedAt"], first_finish, "{history}");
        assert_eq!(
            history["event"]["payload"]["migratedConversation"]["summary"],
            "First result"
        );
        assert_eq!(
            history["event"]["payload"]["migratedConversation"]["outcome"]["reason"],
            "Original work finished"
        );
        assert_eq!(service.store.run(run_id).await.unwrap()["status"], "failed");
    }
}
