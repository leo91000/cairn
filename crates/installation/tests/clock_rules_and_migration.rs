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
use common::{RelayContext, relay_fixture::router, run_fixture::DbRunFixture};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::sync::Arc;
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
                "UPDATE hook_rule_schedules SET next_run=?1 WHERE rule_id=?2",
                params![missed, rule_id_db],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Tick the rules engine: should emit exactly ONE catch-up triggering and start 1 conversation.
    cairn_installation::rules::engine::tick(&app.service)
        .await
        .unwrap();
    cairn_installation::rules::engine::tick(&app.service)
        .await
        .unwrap();

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

    // 5. While the first conversation has a queued message (working), run_now hits
    //    !ruleConversationWorking == false and does NOT start a second conversation.
    let (status, run_now) = app
        .request(
            &app.owner,
            "POST",
            &format!("/api/hook-rules/{rule_id}/run"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run_now}");
    cairn_installation::rules::engine::tick(&app.service)
        .await
        .unwrap();
    let (_, chats) = app
        .request(&app.owner, "GET", "/api/chats", Value::Null)
        .await;
    let count = chats
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["originRule"]["ruleId"] == rule_id)
        .count();
    assert_eq!(
        count, 1,
        "overlap guard must prevent second chat while working"
    );

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

    let (status, run_now) = app
        .request(
            &app.owner,
            "POST",
            &format!("/api/hook-rules/{rule_id}/run"),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run_now}");
    cairn_installation::rules::engine::tick(&app.service)
        .await
        .unwrap();
    let (_, chats) = app
        .request(&app.owner, "GET", "/api/chats", Value::Null)
        .await;
    let count = chats
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["originRule"]["ruleId"] == rule_id)
        .count();
    assert_eq!(
        count, 2,
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

    cairn_installation::rules::engine::tick(&app.service)
        .await
        .unwrap();
    cairn_installation::rules::engine::tick(&app.service)
        .await
        .unwrap();

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
    let regular_chat_id = id();
    let t0 = 1_700_000_000_000i64;
    let future_snooze = now() + 3_600_000;

    conn.execute_batch(
        "DROP TABLE IF EXISTS hook_rule_schedules;
         DROP TABLE IF EXISTS account_snoozes;
         DROP TABLE IF EXISTS account_reads;
         DROP TABLE IF EXISTS wait_conditions;
         DROP TABLE IF EXISTS durable_wakeups;
         DROP TABLE IF EXISTS hook_rule_versions;
         DROP TABLE IF EXISTS hook_rule_uses;

         CREATE TABLE hook_rule_versions(
          rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
          version INTEGER NOT NULL CHECK(version>=1),
          definition TEXT NOT NULL CHECK(json_valid(definition) AND json_type(definition)='object'),
          saved_by TEXT NOT NULL,
          source TEXT NOT NULL CHECK(source IN ('app','publicMcp','installationAgent','duplicate','template')),
          conversation_id TEXT,
          native_grant TEXT NOT NULL,
          saved_at INTEGER NOT NULL,
          PRIMARY KEY(rule_id,version)
         ) WITHOUT ROWID;

         CREATE TABLE hook_rule_uses(
          rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
          kind TEXT NOT NULL CHECK(kind IN ('executionAgent','startedAgent','hook','tool','cairnHook')),
          agent_id TEXT NOT NULL,
          instance_id TEXT NOT NULL,
          name TEXT NOT NULL,
          PRIMARY KEY(rule_id,kind,agent_id,instance_id,name)
         ) WITHOUT ROWID;

         CREATE TABLE hook_rule_task_depths(
          task_id TEXT PRIMARY KEY,
          depth INTEGER NOT NULL
         ) WITHOUT ROWID;

         CREATE TABLE missions(
          id TEXT PRIMARY KEY,
          agent_id TEXT NOT NULL,
          project_id TEXT,
          name TEXT NOT NULL,
          prompt TEXT NOT NULL,
          cron TEXT,
          timezone TEXT NOT NULL,
          worktree INTEGER NOT NULL DEFAULT 1,
          enabled INTEGER NOT NULL DEFAULT 1,
          archived INTEGER NOT NULL DEFAULT 0,
          next_run INTEGER,
          author_id TEXT,
          author_access_id TEXT,
          author_removed INTEGER NOT NULL DEFAULT 0,
          revision TEXT NOT NULL,
          created_at INTEGER NOT NULL,
          updated_at INTEGER NOT NULL,
          schedule_wait_reason TEXT,
          native_principals TEXT NOT NULL DEFAULT '[]',
          native_authority_nodes TEXT NOT NULL DEFAULT '[]',
          native_parent_run_id TEXT,
          native_parent_access TEXT,
          native_plugin_grants TEXT,
          native_provenance_unknown INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE mission_tags(mission_id TEXT NOT NULL, tag TEXT NOT NULL, PRIMARY KEY(mission_id,tag)) WITHOUT ROWID;
         CREATE TABLE mission_skills(mission_id TEXT NOT NULL, position INTEGER NOT NULL, skill_id TEXT NOT NULL, PRIMARY KEY(mission_id,position)) WITHOUT ROWID;

         CREATE TABLE account_reads(
          account_id TEXT NOT NULL,
          kind TEXT NOT NULL CHECK(kind IN ('chat','mission')),
          item_id TEXT NOT NULL,
          finished_at INTEGER NOT NULL,
          PRIMARY KEY(account_id,kind,item_id)
         ) WITHOUT ROWID;

         CREATE TABLE account_dismissals(
          account_id TEXT NOT NULL,
          task_id TEXT NOT NULL,
          finished_at INTEGER NOT NULL,
          snoozed_until INTEGER,
          PRIMARY KEY(account_id,task_id)
         ) WITHOUT ROWID;

         CREATE TABLE durable_wakeups(
          key INTEGER PRIMARY KEY,
          chat_id TEXT REFERENCES conversations(id) ON DELETE CASCADE,
          mission_run_id TEXT REFERENCES runs(id) ON DELETE CASCADE,
          id TEXT NOT NULL,
          message_id TEXT,
          reason TEXT NOT NULL,
          seconds INTEGER NOT NULL,
          created_at INTEGER NOT NULL,
          wake_at INTEGER NOT NULL,
          group_mode TEXT,
          time_kind TEXT,
          time_seconds INTEGER,
          time_date TEXT,
          observation TEXT,
          display TEXT,
          next_check_at INTEGER,
          last_error TEXT
         );
         CREATE TABLE wait_conditions(
          wakeup_key INTEGER NOT NULL REFERENCES durable_wakeups(key) ON DELETE CASCADE,
          position INTEGER NOT NULL,
          kind TEXT NOT NULL,
          target_run_id TEXT,
          target_chat_id TEXT,
          plugin_id TEXT,
          condition_type TEXT,
          project_id TEXT,
          plugin_input TEXT,
          observation TEXT,
          display TEXT,
          next_check_at INTEGER,
          last_error TEXT,
          PRIMARY KEY(wakeup_key,position)
         ) WITHOUT ROWID;",
    )
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
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,next_run,author_id,author_access_id,author_removed,revision,created_at,updated_at,native_authority_nodes) \
         VALUES(?1,?2,'Daily standup','Summarize updates','0 9 * * *','Europe/Paris',1,0,?3,'owner-acct','acc-1',0,'r1',?4,?4,'[\"local\"]')",
        params![active_mission_id, MAIN_AGENT_ID, t0 + 60_000, t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,author_id,author_access_id,author_removed,revision,created_at,updated_at) \
         VALUES(?1,?2,'Paused mission','Do work','0 10 * * *','UTC',0,1,'owner-acct','acc-1',0,'r1',?3,?3)",
        params![disabled_mission_id, MAIN_AGENT_ID, t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,author_id,author_access_id,author_removed,revision,created_at,updated_at) \
         VALUES(?1,?2,'Former member mission','Do work','0 11 * * *','UTC',1,0,'former-member','acc-2',1,'r1',?3,?3)",
        params![removed_mission_id, MAIN_AGENT_ID, t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,author_id,author_access_id,author_removed,revision,created_at,updated_at,schedule_wait_reason) \
         VALUES(?1,?2,'Suspended mission','Do work','0 12 * * *','UTC',1,0,'member-acct','acc-3',0,'r1',?3,?3,'Its author no longer has access to this agent. It resumes once the owner gives access again.')",
        params![suspended_mission_id, MAIN_AGENT_ID, t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO missions(id,agent_id,name,prompt,cron,timezone,enabled,archived,author_id,author_access_id,author_removed,revision,created_at,updated_at) \
         VALUES(?1,?2,'Manual one-shot','Run once',NULL,'UTC',1,0,'owner-acct','acc-1',0,'r1',?3,?3)",
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

    // Seed two legacy mission runs: one finished+blocked (for active_mission_id) and one waiting with wakeup (for oneshot_mission_id).
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,started_at,finished_at,snapshot,boundary,\
           recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'succeeded','schedule',?3,?3,?4,?5,?6,0,0,0)",
        params![
            blocked_run_id,
            active_mission_id,
            t0,
            t0 + 5000,
            json!({ "task": { "name": "Daily standup" }, "agent": { "id": MAIN_AGENT_ID, "name": "Main" } }).to_string(),
            json!({ "outcome": { "status": "needs_input", "reason": "Waiting on review" }, "nativeProvenanceUnknown": false }).to_string(),
        ],
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
            json!({ "launched": true, "completed": true }).to_string()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO durable_wakeups(key,chat_id,mission_run_id,id,reason,seconds,created_at,wake_at,time_kind,time_seconds) \
         VALUES(1,NULL,?1,'w1','Wait for deploy',60,?2,?3,'delay',60)",
        params![waiting_run_id, t0, t0 + 60_000],
    )
    .unwrap();

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

    let converted_run = id();
    conn.execute(
        "INSERT INTO runs(id,task_id,status,trigger,created_at,finished_at,snapshot,boundary, \
          recovery_pending,resume_available,resume_count) \
         VALUES(?1,?2,'succeeded','chat',?3,?3,?4,?5,0,0,0)",
        params![
            converted_run,
            regular_chat_id,
            t0,
            json!({ "agent": agent, "task": { "name": "Existing chat", "prompt": "Old brief", "worktree": false } }).to_string(),
            json!({ "sourceTaskId": active_mission_id, "sourceTaskPrompt": "Old brief" }).to_string(),
        ],
    ).unwrap();
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
        "INSERT INTO account_dismissals VALUES('owner-acct',?1,?2,?3)",
        params![active_mission_id, t0 + 5000, future_snooze],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_dismissals VALUES('member-acct',?1,?2,NULL)",
        params![active_mission_id, t0 + 5000],
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
    let next_run: Option<i64> = migrated
        .query_row(
            "SELECT next_run FROM hook_rule_schedules WHERE rule_id=?1",
            [&active_mission_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(next_run, Some(t0 + 60_000));

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
    let suspended_author: (String, bool) = migrated
        .query_row(
            "SELECT creator_account_id,creator_owner FROM hook_rules WHERE id=?1",
            [&suspended_mission_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(suspended_author, ("member-acct".into(), false));
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

    // Reads & snoozes migrated cleanly:
    // - regular chat read preserved
    // - owner active snooze -> account_snoozes on blocked_chat_id
    // - member permanent dismiss -> account_reads on blocked_chat_id
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
        (converted_run, active_mission_id.clone(), true, None)
    );
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
            "SELECT finished_at FROM account_reads WHERE account_id='member-acct' AND chat_id=?1",
            [&blocked_chat_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(member_read, t0 + 5000);

    drop(migrated);
    let service = cairn_installation::service::Service::new(config)
        .await
        .unwrap();
    // Before Beacon confirms migrated membership, a due owner slot stays available.
    cairn_installation::rules::engine::tick(&service)
        .await
        .unwrap();
    let schedule: i64 = service
        .store
        .read(move |db| {
            Ok(db.0.query_row(
                "SELECT next_run FROM hook_rule_schedules WHERE rule_id=?",
                [&active_mission_id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(schedule, next_run.unwrap());
    let detail = service.chat_detail(&waiting_chat_id).await.unwrap();
    assert_eq!(detail["run"]["chatExecution"]["messageId"], "legacy-turn");
    assert_eq!(detail["wakeup"]["messageId"], "legacy-turn");
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
}
