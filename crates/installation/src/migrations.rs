//! Schema v38 -> v39 migration (ADR-0094): converts legacy missions into Clock hook rules,
//! converts mission runs into ordinary conversations linked to their originating rule,
//! migrates read marks and snoozes, and drops retired mission tables.
use crate::{
    config::{id, now},
    error::{Error, Result},
    rules::storage::{RuleRow, ScheduleRow, Use, VersionRow},
    store::Db,
};
use cairn_contracts::{
    AgentId, HookRuleDefinition, HookRuleLimits, HookRuleSource, HookRuleTrigger, ProjectId,
    RuleAction, RuleCondition, RuleNode, RuleValue,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(crate) const PRE_MISSION_SUNSET_VERSION: i64 = 38;
pub(crate) const TARGET_VERSION: i64 = 39;

struct LegacyMission {
    id: String,
    agent_id: String,
    project_id: Option<String>,
    name: String,
    prompt: String,
    cron: Option<String>,
    timezone: String,
    enabled: bool,
    archived: bool,
    next_run: Option<i64>,
    author_id: Option<String>,
    author_access_id: Option<String>,
    author_removed: bool,
    created_at: i64,
    updated_at: i64,
    native_principals: String,
    native_authority_nodes: String,
    native_parent_run_id: Option<String>,
    native_parent_access: Option<String>,
    native_provenance_unknown: bool,
    native_plugin_grants: Option<String>,
}

struct MigratedMission {
    rule_id: String,
    name: String,
    creator: String,
    agent: String,
    brief: String,
}

struct MigratedRun {
    chat_id: String,
    activity_at: i64,
    needs_review: bool,
}

struct LegacyRun {
    id: String,
    task_id: String,
    project_id: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    task_name: String,
    agent_name: String,
    snapshot: Option<String>,
    boundary: String,
    status: String,
    session_id: Option<String>,
    checkpoint: Option<String>,
}

fn legacy_run_batch(tx: &Connection, cursor: &(i64, String)) -> Result<Vec<LegacyRun>> {
    let mut stmt = tx.prepare_cached(
        "SELECT r.id,r.task_id,r.project_id,r.created_at,r.started_at,r.finished_at,\
             r.task_name,r.agent_name,r.snapshot,r.boundary,r.status,r.session_id,k.state \
             FROM runs r LEFT JOIN run_checkpoints k ON k.run_id=r.id \
             WHERE (r.created_at,r.id)>(?1,?2) AND r.trigger!='chat' \
             AND NOT EXISTS(SELECT 1 FROM conversations c WHERE c.run_id=r.id) \
             ORDER BY r.created_at ASC, r.id ASC LIMIT 32",
    )?;
    let rows = stmt
        .query_map(params![cursor.0, cursor.1], |r| {
            Ok(LegacyRun {
                id: r.get(0)?,
                task_id: r.get(1)?,
                project_id: r.get(2)?,
                created_at: r.get(3)?,
                started_at: r.get(4)?,
                finished_at: r.get(5)?,
                task_name: r.get(6)?,
                agent_name: r.get(7)?,
                snapshot: r.get(8)?,
                boundary: r.get(9)?,
                status: r.get(10)?,
                session_id: r.get(11)?,
                checkpoint: r.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn table_exists(tx: &Connection, name: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
        [name],
        |row| row.get(0),
    )
}

fn available_rule_id(tx: &Connection, mission_id: &str) -> Result<String> {
    let claimed = Db(tx).hook_rule_identity_claimed(mission_id)?;
    Ok(if claimed { id() } else { mission_id.to_owned() })
}

fn retired_mission_rule_id(
    tx: &Connection,
    retired_rules: &mut HashMap<String, String>,
    mission_id: &str,
) -> Result<String> {
    if let Some(rule_id) = retired_rules.get(mission_id) {
        return Ok(rule_id.clone());
    }
    let rule_id = available_rule_id(tx, mission_id)?;
    retired_rules.insert(mission_id.to_owned(), rule_id.clone());
    Ok(rule_id)
}

fn rebuild_rule_tables(tx: &Connection) -> Result<()> {
    // Allow source='migration' in hook_rule_versions and kind='clock' in hook_rule_uses,
    // and add hook_rule_schedules.
    tx.execute_batch(
        "CREATE TABLE hook_rule_versions_v39(
          rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
          version INTEGER NOT NULL CHECK(version>=1),
          definition TEXT NOT NULL CHECK(json_valid(definition) AND json_type(definition)='object' AND length(CAST(definition AS BLOB))<=524288),
          saved_by TEXT NOT NULL CHECK(length(saved_by) BETWEEN 1 AND 100),
          source TEXT NOT NULL CHECK(source IN ('app','publicMcp','installationAgent','duplicate','template','migration')),
          conversation_id TEXT CHECK(conversation_id IS NULL OR length(conversation_id) BETWEEN 1 AND 100),
          native_grant TEXT NOT NULL CHECK(json_valid(native_grant) AND json_type(native_grant)='object'),
          saved_at INTEGER NOT NULL CHECK(saved_at BETWEEN 0 AND 9007199254740991),
          PRIMARY KEY(rule_id,version)
         ) WITHOUT ROWID;
         INSERT INTO hook_rule_versions_v39 SELECT * FROM hook_rule_versions;
         DROP TABLE hook_rule_versions;
         ALTER TABLE hook_rule_versions_v39 RENAME TO hook_rule_versions;

         CREATE TABLE hook_rule_uses_v39(
          rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
          kind TEXT NOT NULL CHECK(kind IN ('executionAgent','startedAgent','hook','tool','cairnHook','clock')),
          agent_id TEXT NOT NULL,
          instance_id TEXT NOT NULL,
          name TEXT NOT NULL CHECK(length(name)<=200),
          PRIMARY KEY(rule_id,kind,agent_id,instance_id,name)
         ) WITHOUT ROWID;
         INSERT INTO hook_rule_uses_v39 SELECT * FROM hook_rule_uses;
         DROP TABLE hook_rule_uses;
         ALTER TABLE hook_rule_uses_v39 RENAME TO hook_rule_uses;
         CREATE INDEX IF NOT EXISTS hook_rule_uses_agent ON hook_rule_uses(agent_id,kind);
         CREATE INDEX IF NOT EXISTS hook_rule_uses_instance ON hook_rule_uses(instance_id,kind);

         CREATE TABLE IF NOT EXISTS hook_rule_schedules(
          rule_id TEXT PRIMARY KEY NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
          next_run INTEGER CHECK(next_run BETWEEN 0 AND 9007199254740991),
          last_scheduled_for INTEGER CHECK(last_scheduled_for BETWEEN 0 AND 9007199254740991),
          fired_at INTEGER CHECK(fired_at BETWEEN 0 AND 9007199254740991),
          legacy_chain_depth INTEGER NOT NULL DEFAULT 0 CHECK(legacy_chain_depth BETWEEN 0 AND 8)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS hook_rule_schedules_due ON hook_rule_schedules(next_run) WHERE next_run IS NOT NULL;
         CREATE INDEX IF NOT EXISTS hook_rule_conversations_rule ON hook_rule_conversations(rule_id);",
    )?;
    Ok(())
}

fn known_owner_account(tx: &Connection) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT creator_account_id FROM hook_rules WHERE creator_owner=1 ORDER BY created_at LIMIT 1",
        [],
        |row| row.get(0),
    )
    .optional()
}

fn slug_for_migrated_chat(tx: &Connection, chat_id: &str) -> rusqlite::Result<String> {
    let short: String = chat_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    let base = format!("schedule-{short}");
    let mut candidate = base.clone();
    let mut attempt = 0u32;
    loop {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE slug=?)",
            [&candidate],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(candidate);
        }
        attempt += 1;
        candidate = format!("{base}-{attempt}");
    }
}

pub(crate) fn upgrade_to_v39(writer: &mut Connection) -> Result<()> {
    let tx = writer.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == TARGET_VERSION {
        return Ok(());
    }
    if version != PRE_MISSION_SUNSET_VERSION {
        return Err(Error::bad(
            "Unexpected database version during mission-sunset migration.",
        ));
    }

    rebuild_rule_tables(&tx)?;

    let at = now();
    let known_owner = known_owner_account(&tx)?;
    let mut mission_meta = HashMap::new();

    if table_exists(&tx, "missions")? {
        let missions = {
            let mut stmt = tx.prepare(
                "SELECT id,agent_id,project_id,name,prompt,cron,timezone,enabled,archived,next_run,\
                 author_id,author_access_id,author_removed,created_at,updated_at,\
                 native_principals,native_authority_nodes,native_parent_run_id,native_parent_access,\
                 native_provenance_unknown,native_plugin_grants FROM missions ORDER BY created_at,id",
            )?;
            stmt.query_map([], |r| {
                Ok(LegacyMission {
                    id: r.get(0)?,
                    agent_id: r.get(1)?,
                    project_id: r.get(2)?,
                    name: r.get(3)?,
                    prompt: r.get(4)?,
                    cron: r.get(5)?,
                    timezone: r.get(6)?,
                    enabled: r.get(7)?,
                    archived: r.get(8)?,
                    next_run: r.get(9)?,
                    author_id: r.get(10)?,
                    author_access_id: r.get(11)?,
                    author_removed: r.get(12)?,
                    created_at: r.get(13)?,
                    updated_at: r.get(14)?,
                    native_principals: r.get(15)?,
                    native_authority_nodes: r.get(16)?,
                    native_parent_run_id: r.get(17)?,
                    native_parent_access: r.get(18)?,
                    native_provenance_unknown: r.get(19)?,
                    native_plugin_grants: r.get(20)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };

        let db = Db(&tx);
        let has_legacy_depths = table_exists(&tx, "hook_rule_task_depths")?;
        for mission in missions {
            let creator_account_id = mission
                .author_id
                .clone()
                .or_else(|| known_owner.clone())
                .unwrap_or_else(|| "owner".to_owned());
            // The database does not record a mission author's installation role. The rules
            // engine verifies member authority with Beacon before it acts, and reconciliation
            // recognizes the confirmed owner and exact membership generation.
            let membership_ended = mission.author_removed || mission.author_id.is_none();
            let rule_id = available_rule_id(&tx, &mission.id)?;
            mission_meta.insert(
                mission.id.clone(),
                MigratedMission {
                    rule_id: rule_id.clone(),
                    name: mission.name.clone(),
                    creator: creator_account_id.clone(),
                    agent: mission.agent_id.clone(),
                    brief: mission.prompt.clone(),
                },
            );

            let Ok(agent_uuid) = uuid::Uuid::parse_str(&mission.agent_id) else {
                continue;
            };
            let agent_id = AgentId(agent_uuid);
            let project_id = mission
                .project_id
                .as_deref()
                .and_then(|p| uuid::Uuid::parse_str(p).ok())
                .map(ProjectId);

            let cron = mission
                .cron
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_owned);
            let timezone = if mission.timezone.parse::<chrono_tz::Tz>().is_ok() {
                mission.timezone.clone()
            } else {
                "Europe/Paris".to_owned()
            };
            let trigger = match &cron {
                Some(pattern) => HookRuleTrigger::Clock {
                    cron: Some(pattern.clone()),
                    at: None,
                    timezone: timezone.clone(),
                },
                None => HookRuleTrigger::Clock {
                    cron: None,
                    at: Some(mission.created_at.max(1)),
                    timezone: timezone.clone(),
                },
            };

            let prompt_text = if mission.prompt.trim().is_empty() {
                "Run scheduled task.".to_owned()
            } else {
                mission.prompt.clone()
            };
            let name = {
                let trimmed = mission.name.trim();
                let clean: String = trimmed
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(100)
                    .collect();
                if clean.is_empty() {
                    "Migrated schedule".to_owned()
                } else {
                    clean
                }
            };

            let definition = HookRuleDefinition {
                name,
                description: String::new(),
                trigger,
                execution_agent_id: agent_id,
                steps: vec![RuleNode::If {
                    id: "skip_if_working".into(),
                    condition: RuleCondition::Not {
                        condition: Box::new(RuleCondition::RuleConversationWorking {}),
                    },
                    then: vec![RuleNode::Action {
                        id: "start_agent".into(),
                        label: String::new(),
                        action: RuleAction::StartAgent {
                            agent_id,
                            prompt: RuleValue::text(&prompt_text),
                            project_id,
                            key: None,
                            author: None,
                        },
                    }],
                    otherwise: Vec::new(),
                }],
                limits: HookRuleLimits::default(),
            };

            let disabled = !mission.enabled || mission.archived || cron.is_none();
            let row = RuleRow {
                id: rule_id,
                creator_account_id: creator_account_id.clone(),
                creator_access_id: mission.author_access_id.clone(),
                creator_owner: false,
                revision: id(),
                version: 1,
                disabled_at: disabled.then_some(at),
                emergency_paused_at: None,
                budget_exhausted_at: None,
                budget_reason: None,
                membership_ended_at: membership_ended.then_some(at),
                created_at: mission.created_at,
                updated_at: mission.updated_at.max(mission.created_at),
            };

            let native_principals: Value =
                serde_json::from_str(&mission.native_principals).unwrap_or_else(|_| json!([]));
            let native_authority_nodes: Value =
                serde_json::from_str(&mission.native_authority_nodes).unwrap_or_else(|_| json!([]));
            let native_grant = json!({
                "nativePrincipals": native_principals,
                "nativeAuthorityNodes": native_authority_nodes,
                "nativeParentRunId": mission.native_parent_run_id,
                "nativeParentAccess": mission.native_parent_access,
                "nativeProvenanceUnknown": mission.native_provenance_unknown,
                "nativePluginGrants": mission.native_plugin_grants.as_deref().map(serde_json::from_str::<Value>).transpose()?,
            });

            db.save_hook_rule(&row)?;
            let uses = vec![
                Use {
                    kind: "executionAgent",
                    agent_id: mission.agent_id.clone(),
                    instance_id: String::new(),
                    name: String::new(),
                },
                Use {
                    kind: "startedAgent",
                    agent_id: mission.agent_id.clone(),
                    instance_id: String::new(),
                    name: String::new(),
                },
                Use {
                    kind: "clock",
                    agent_id: mission.agent_id.clone(),
                    instance_id: String::new(),
                    name: String::new(),
                },
            ];
            db.insert_hook_rule_version(
                &VersionRow {
                    rule_id: row.id.clone(),
                    version: 1,
                    definition,
                    saved_by: creator_account_id,
                    source: HookRuleSource::Migration,
                    conversation_id: None,
                    native_grant,
                    saved_at: mission.created_at,
                },
                &uses,
            )?;
            let legacy_chain_depth = if has_legacy_depths {
                tx.query_row(
                    "SELECT depth FROM hook_rule_task_depths WHERE task_id=?",
                    [&mission.id],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0)
            } else {
                0
            };
            let next_run = match &cron {
                Some(pattern) if !disabled && !membership_ended => mission.next_run.or_else(|| {
                    crate::service::next_occurrences(pattern, &timezone, at, 1)
                        .ok()
                        .and_then(|v| v.into_iter().next())
                }),
                _ => None,
            };
            db.save_hook_rule_schedule(&ScheduleRow {
                rule_id: row.id.clone(),
                next_run,
                last_scheduled_for: None,
                fired_at: cron.is_none().then(|| mission.created_at.max(1)),
                legacy_chain_depth,
            })?;
        }
    }

    let mut retired_rules = HashMap::new();
    link_converted_mission_runs(
        &tx,
        &mission_meta,
        &mut retired_rules,
        known_owner.as_deref(),
    )?;

    // Read full snapshots in bounded batches. Only the small read-mark and
    // dismissal mapping survives a batch.
    let mut migrated_chats_by_mission: HashMap<String, Vec<MigratedRun>> = HashMap::new();

    let mut cursor = (i64::MIN, String::new());
    loop {
        let runs = legacy_run_batch(&tx, &cursor)?;
        if runs.is_empty() {
            break;
        }
        for run in runs {
            cursor = (run.created_at, run.id.clone());
            let snapshot: Value = run
                .snapshot
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            let meta = mission_meta.get(&run.task_id);
            let agent_id = snapshot["agent"]["id"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| meta.map(|m| m.agent.clone()))
                .unwrap_or_default();
            if agent_id.is_empty() {
                continue;
            }
            let agent_name = if !run.agent_name.is_empty() {
                Some(run.agent_name.clone())
            } else {
                snapshot["agent"]["name"].as_str().map(str::to_owned)
            };
            let project_name = snapshot["project"]["name"].as_str().map(str::to_owned);
            let live_agent_id: Option<String> = tx
                .query_row("SELECT id FROM agents WHERE id=?", [&agent_id], |r| {
                    r.get(0)
                })
                .optional()?;
            let live_project_id: Option<String> = match &run.project_id {
                Some(pid) => tx
                    .query_row("SELECT id FROM projects WHERE id=?", [pid], |r| r.get(0))
                    .optional()?,
                None => None,
            };
            let title = if !run.task_name.trim().is_empty() {
                run.task_name.clone()
            } else if let Some(m) = meta {
                m.name.clone()
            } else {
                "Migrated run".to_owned()
            };
            let chat_id = id();
            let slug = slug_for_migrated_chat(&tx, &chat_id)?;
            let updated_at = run.finished_at.or(run.started_at).unwrap_or(run.created_at);
            let rev = id();
            let boundary: Value = serde_json::from_str(&run.boundary)?;
            let parked = !matches!(run.status.as_str(), "queued" | "running");
            let expired = parked && boundary["environmentState"] == "expired";
            let expired_at = expired
                .then(|| boundary["environmentExpiredAt"].as_i64())
                .flatten();
            let retirement = boundary["environmentRetirementId"]
                .as_str()
                .filter(|_| expired);

            tx.execute(
            "INSERT INTO conversations(\
             id,slug,title,agent_id,live_agent_id,project_id,live_project_id,run_id,\
             paused,lifecycle,created_at,updated_at,last_activity_at,revision,\
             session_restart_requested,cancelled_by_deletion,agent_name,project_name,persisted_at,\
             native_principals,native_authority_nodes,native_parent_run_id,native_parent_access,\
             native_provenance_unknown,native_plugin_grants,environment_state,environment_expired_at,environment_error\
            ) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,0,'active',?9,?10,?10,?11,0,0,?12,?13,?10,\
              json_extract(boundary,'$.nativePrincipals'),json_extract(boundary,'$.nativeAuthorityNodes'),\
              json_extract(boundary,'$.nativeParentRunId'),json_extract(boundary,'$.nativeParentAccess'),\
              coalesce(json_extract(boundary,'$.nativeProvenanceUnknown'),1),\
              json_extract(boundary,'$.nativePluginGrants'),?14,?15,\
              (SELECT json_extract(data,'$.error') FROM records WHERE kind='environment-retirements' AND id=?16)\
              FROM runs WHERE id=?8",
            params![
                chat_id,
                slug,
                title,
                agent_id,
                live_agent_id,
                run.project_id,
                live_project_id,
                run.id,
                run.created_at,
                updated_at,
                rev,
                agent_name,
                project_name,
                expired.then_some("expired"),
                expired_at,
                retirement,
            ],
        )?;

            let rule_name = meta.map_or_else(|| title.clone(), |m| m.name.clone());
            let creator_account_id = meta
                .map(|m| m.creator.clone())
                .or_else(|| known_owner.clone())
                .unwrap_or_else(|| "owner".to_owned());
            let rule_id = match meta {
                Some(mission) => mission.rule_id.clone(),
                None => retired_mission_rule_id(&tx, &mut retired_rules, &run.task_id)?,
            };
            let origin_json = json!({
                "hook": "clock",
                "ruleName": rule_name,
                "schedule": true,
            })
            .to_string();
            tx.execute(
            "INSERT INTO hook_rule_conversations(\
             chat_id,creator_account_id,key,agent_id,rule_id,author,origin,run_id,cursor,watch,created_at\
            ) VALUES(?1,?2,NULL,?3,?4,NULL,?5,?6,0,'{}',?7)",
            params![
                chat_id,
                creator_account_id,
                agent_id,
                rule_id,
                origin_json,
                run.id,
                run.created_at,
            ],
        )?;

            // Record as rule-started in conversation_writers so ADR-0075 treats it as rule-originated.
            tx.execute(
                "INSERT OR IGNORE INTO conversation_writers(\
             chat_id,account_id,agent_id,plugin_instructions,created_at,instruction_tainted,\
             reached_at,reached_by,reached_account_id,reached_rule_id\
            ) VALUES(?1,NULL,?2,'',?3,0,?3,'rule',NULL,?4)",
                params![chat_id, agent_id, run.created_at, rule_id],
            )?;

            let legacy_wait: Option<Option<String>> = tx
                .query_row(
                    "SELECT message_id FROM durable_wakeups WHERE mission_run_id=?",
                    [&run.id],
                    |row| row.get(0),
                )
                .optional()?;
            let turn = legacy_wait
                .as_ref()
                .and_then(|message| message.as_deref())
                .or_else(|| boundary["missionTurnId"].as_str())
                .unwrap_or(&run.id);
            let brief = snapshot["task"]["prompt"]
                .as_str()
                .or_else(|| meta.map(|mission| mission.brief.as_str()))
                .unwrap_or(&title);
            let checkpoint: Value = run
                .checkpoint
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or_default();
            let launched = checkpoint["turnPending"] != true
                && (checkpoint["launched"] == true
                    || checkpoint["launchedTurn"].is_string()
                    || (run.started_at.is_some() && run.session_id.is_some()));
            let recovery = launched && run.status != "succeeded";
            let text = boundary["waitContinuation"]["reason"].as_str().map_or_else(
                || {
                    if recovery {
                        crate::run_output::RESUME_PROMPT.to_owned()
                    } else {
                        format!("Task brief:\n{brief}")
                    }
                },
                |reason| {
                    let observation = boundary["waitContinuation"]
                        .get("observation")
                        .filter(|data| !data.is_null());
                    crate::chats::continuation::resume_prompt(reason, observation)
                },
            );
            let execution = json!({
                "messageId": turn,
                "text": text,
                "attachments": [],
                "recovery": recovery,
            });
            // Older mission processes used the saved turn id without a conversation
            // message. Preserve that id in the conversation, wakeup and checkpoint.
            tx.execute(
            "UPDATE runs SET task_id=?1,trigger='chat', \
             status=CASE WHEN status='waiting' AND ?2 THEN 'succeeded' ELSE status END, \
             finished_at=CASE WHEN status='waiting' AND ?2 THEN coalesce(finished_at,?3) ELSE finished_at END, \
             boundary=json_set(boundary,'$.chatExecution',json(?4),'$.sourceTaskId',?5, \
                 '$.sourceTaskPrompt',?6,'$.sourceSessionId',session_id) WHERE id=?7",
            params![chat_id, legacy_wait.is_some(), updated_at, execution.to_string(), run.task_id, brief, run.id],
        )?;
            if launched {
                tx.execute(
                    "UPDATE run_checkpoints SET state=json_set(state,'$.launchedTurn',?2) \
                 WHERE run_id=?1 AND coalesce(json_extract(state,'$.launchedTurn'),'')=''",
                    params![run.id, turn],
                )?;
            }
            if legacy_wait.is_some() {
                tx.execute(
                    "UPDATE durable_wakeups SET chat_id=?1,mission_run_id=NULL,message_id=?2 \
                 WHERE mission_run_id=?3",
                    params![chat_id, turn, run.id],
                )?;
            }

            migrated_chats_by_mission
                .entry(run.task_id)
                .or_default()
                .push(MigratedRun {
                    chat_id,
                    activity_at: updated_at,
                    needs_review: run.finished_at.is_some()
                        && matches!(
                            boundary["outcome"]["status"].as_str(),
                            Some("blocked" | "needs_input")
                        ),
                });
        }
    }

    // Repoint references to the calling run in one pass after all conversations exist.
    tx.execute(
        "UPDATE runs AS child SET requested_by=json_remove(\
             json_set(child.requested_by,'$.chatId',parent.id),'$.taskId') \
         FROM conversations AS parent \
         WHERE json_type(child.requested_by,'$.taskId') IS NOT NULL \
         AND parent.run_id=json_extract(child.requested_by,'$.runId')",
        [],
    )?;

    // Include conversations the old converter had already created. Their saved
    // direct preparation cannot become the working directory of another turn.
    tx.execute_batch(
        "UPDATE runs SET snapshot=json_set(snapshot,'$.task.worktree',json('true')) \
         WHERE json_type(boundary,'$.sourceTaskId')='text' AND json_type(snapshot,'$.task')='object'; \
         UPDATE run_checkpoints SET state=json_remove(state,'$.prepared','$.workspaceInitialized') \
         WHERE run_id IN (SELECT id FROM runs WHERE json_type(boundary,'$.sourceTaskId')='text') \
           AND EXISTS(SELECT 1 FROM json_each(state,'$.prepared.workspaces') \
                      WHERE json_extract(value,'$.kind')='direct');",
    )?;

    // Migrate account_reads and account_dismissals to v39 chat-only tables.
    tx.execute_batch(
        "CREATE TABLE account_reads_v39(
            account_id TEXT NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
            chat_id TEXT NOT NULL CHECK(length(chat_id) BETWEEN 32 AND 45),
            finished_at INTEGER NOT NULL CHECK(finished_at BETWEEN 0 AND 9007199254740991),
            PRIMARY KEY(account_id,chat_id)
        ) WITHOUT ROWID;
        CREATE TABLE account_snoozes(
            account_id TEXT NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
            chat_id TEXT NOT NULL CHECK(length(chat_id) BETWEEN 32 AND 45),
            finished_at INTEGER NOT NULL CHECK(finished_at BETWEEN 1 AND 9007199254740991),
            snoozed_until INTEGER NOT NULL CHECK(snoozed_until BETWEEN finished_at + 1 AND 9007199254740991),
            PRIMARY KEY(account_id,chat_id)
        ) WITHOUT ROWID;",
    )?;

    if table_exists(&tx, "account_reads")? {
        tx.execute(
            "INSERT INTO account_reads_v39(account_id,chat_id,finished_at) \
             SELECT account_id,item_id,finished_at FROM account_reads WHERE kind='chat'",
            [],
        )?;
        let mission_reads = {
            let mut stmt = tx.prepare(
                "SELECT account_id,item_id,finished_at FROM account_reads WHERE kind='mission'",
            )?;
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (account_id, mission_id, finished_at) in mission_reads {
            if let Some(chats) = migrated_chats_by_mission.get(&mission_id) {
                for chat in chats {
                    if chat.activity_at <= finished_at {
                        tx.execute(
                            "INSERT INTO account_reads_v39(account_id,chat_id,finished_at) VALUES(?1,?2,?3) \
                             ON CONFLICT(account_id,chat_id) DO UPDATE SET finished_at=max(finished_at,excluded.finished_at)",
                            params![account_id, chat.chat_id, finished_at],
                        )?;
                    }
                }
            }
        }
        tx.execute("DROP TABLE account_reads", [])?;
    }
    tx.execute("ALTER TABLE account_reads_v39 RENAME TO account_reads", [])?;

    if table_exists(&tx, "account_dismissals")? {
        let dismissals = {
            let mut stmt = tx.prepare(
                "SELECT account_id,task_id,finished_at,snoozed_until FROM account_dismissals",
            )?;
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (account_id, mission_id, finished_at, snoozed_until) in dismissals {
            let Some(chats) = migrated_chats_by_mission.get(&mission_id) else {
                continue;
            };
            for chat in chats
                .iter()
                .filter(|chat| chat.needs_review && chat.activity_at <= finished_at)
            {
                match snoozed_until {
                    Some(until) if until > finished_at => {
                        tx.execute(
                            "INSERT INTO account_snoozes(account_id,chat_id,finished_at,snoozed_until) VALUES(?1,?2,?3,?4) \
                             ON CONFLICT(account_id,chat_id) DO UPDATE SET finished_at=excluded.finished_at,snoozed_until=excluded.snoozed_until",
                            params![account_id, chat.chat_id, finished_at, until],
                        )?;
                    }
                    None => {
                        // ADR-0094: dismiss becomes mark read.
                        tx.execute(
                            "INSERT INTO account_reads(account_id,chat_id,finished_at) VALUES(?1,?2,?3) \
                             ON CONFLICT(account_id,chat_id) DO UPDATE SET finished_at=max(finished_at,excluded.finished_at)",
                            params![account_id, chat.chat_id, finished_at],
                        )?;
                    }
                    _ => {}
                }
            }
        }
        tx.execute("DROP TABLE account_dismissals", [])?;
    }

    // Rebuild durable_wakeups without mission_run_id (only chat_id remains).
    if table_exists(&tx, "durable_wakeups")? {
        tx.execute_batch(
            "CREATE TABLE durable_wakeups_v39(
                key INTEGER PRIMARY KEY,
                chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                id TEXT NOT NULL CHECK(length(id)>0),
                message_id TEXT,
                reason TEXT NOT NULL,
                seconds INTEGER NOT NULL CHECK(seconds BETWEEN 1 AND 86400),
                created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
                wake_at INTEGER NOT NULL CHECK(wake_at BETWEEN 0 AND 9007199254740991),
                group_mode TEXT CHECK(group_mode IN ('all','any')),
                time_kind TEXT CHECK(time_kind IN ('delay','at')),
                time_seconds INTEGER CHECK(time_seconds BETWEEN 1 AND 86400),
                time_date TEXT,
                observation TEXT CHECK(observation IS NULL OR json_valid(observation)),
                display TEXT CHECK(display IS NULL OR json_valid(display)),
                next_check_at INTEGER CHECK(next_check_at BETWEEN 0 AND 9007199254740991),
                last_error TEXT,
                CHECK((time_kind IS NULL AND time_seconds IS NULL AND time_date IS NULL) OR (time_kind='delay' AND time_seconds IS NOT NULL AND time_date IS NULL) OR (time_kind='at' AND time_date IS NOT NULL AND time_seconds IS NULL))
            );
            INSERT INTO durable_wakeups_v39(
                key,chat_id,id,message_id,reason,seconds,created_at,wake_at,group_mode,
                time_kind,time_seconds,time_date,observation,display,next_check_at,last_error
            )
            SELECT key,chat_id,id,message_id,reason,seconds,created_at,wake_at,group_mode,
                time_kind,time_seconds,time_date,observation,display,next_check_at,last_error
            FROM durable_wakeups WHERE chat_id IS NOT NULL;
            CREATE TABLE wait_conditions_v39(
                wakeup_key INTEGER NOT NULL REFERENCES durable_wakeups_v39(key) ON DELETE CASCADE,
                position INTEGER NOT NULL CHECK(position>=0),
                kind TEXT NOT NULL CHECK(kind IN ('run_finished','task_finished','job_finished','delay','at','plugin')),
                target_run_id TEXT,
                target_chat_id TEXT,
                plugin_id TEXT,
                condition_type TEXT,
                project_id TEXT,
                plugin_input TEXT CHECK(plugin_input IS NULL OR (json_valid(plugin_input) AND json_type(plugin_input)='object')),
                observation TEXT CHECK(observation IS NULL OR json_valid(observation)),
                display TEXT CHECK(display IS NULL OR json_valid(display)),
                next_check_at INTEGER CHECK(next_check_at BETWEEN 0 AND 9007199254740991),
                last_error TEXT,
                CHECK((kind='run_finished' AND target_run_id IS NOT NULL AND target_chat_id IS NULL AND plugin_id IS NULL AND condition_type IS NULL AND project_id IS NULL AND plugin_input IS NULL)
                   OR (kind='task_finished' AND target_chat_id IS NOT NULL AND target_run_id IS NULL AND plugin_id IS NULL AND condition_type IS NULL AND project_id IS NULL AND plugin_input IS NULL)
                   OR (kind IN ('job_finished','delay','at') AND target_run_id IS NULL AND target_chat_id IS NULL AND plugin_id IS NULL AND condition_type IS NOT NULL AND project_id IS NULL AND plugin_input IS NULL)
                   OR (kind='plugin' AND target_run_id IS NULL AND target_chat_id IS NULL AND plugin_id IS NOT NULL AND condition_type IS NOT NULL AND plugin_input IS NOT NULL)),
                PRIMARY KEY(wakeup_key,position)
            ) WITHOUT ROWID;
            INSERT INTO wait_conditions_v39
            SELECT c.* FROM wait_conditions c JOIN durable_wakeups_v39 w ON w.key=c.wakeup_key;
            DROP TABLE wait_conditions;
            DROP TABLE durable_wakeups;
            ALTER TABLE durable_wakeups_v39 RENAME TO durable_wakeups;
            ALTER TABLE wait_conditions_v39 RENAME TO wait_conditions;
            CREATE UNIQUE INDEX durable_wakeups_chat ON durable_wakeups(chat_id);",
        )?;
    }

    // Pending questions keep the original follow-up. Older outcomes retain their
    // reason and evidence under the remaining status for unresolved work.
    tx.execute(
        "UPDATE runs SET boundary=json_set(boundary,'$.outcome.status','blocked') \
         WHERE json_extract(boundary,'$.outcome.status')='needs_input'",
        [],
    )?;

    // Drop retired mission tables.
    tx.execute_batch(
        "DROP TABLE IF EXISTS hook_rule_task_depths;
         DROP TABLE IF EXISTS mission_tags;
         DROP TABLE IF EXISTS mission_skills;
         DROP TABLE IF EXISTS missions;",
    )?;

    tx.pragma_update(None, "user_version", TARGET_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// Link legacy conversions before changing any additional run's trigger. The
/// scan touches the existing conversation histories once regardless of how
/// many missions the installation has.
fn link_converted_mission_runs(
    tx: &Connection,
    missions: &HashMap<String, MigratedMission>,
    retired_rules: &mut HashMap<String, String>,
    known_owner: Option<&str>,
) -> Result<()> {
    let mut converted = tx.prepare(
        "SELECT c.id,c.agent_id,c.created_at,r.id,json_extract(r.boundary,'$.sourceTaskId'), \
            coalesce(nullif(r.task_name,''),c.title) \
         FROM conversations c JOIN runs r ON r.id=c.run_id \
         WHERE r.trigger='chat' AND json_type(r.boundary,'$.sourceTaskId')='text'",
    )?;
    let mut rows = converted.query([])?;
    let mut insert = tx.prepare(
        "INSERT INTO hook_rule_conversations( \
             chat_id,creator_account_id,agent_id,rule_id,origin,run_id,created_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(chat_id) DO NOTHING",
    )?;
    while let Some(row) = rows.next()? {
        let mission_id: String = row.get(4)?;
        let historical_title: String = row.get(5)?;
        let (rule_id, rule_name, creator) = match missions.get(&mission_id) {
            Some(mission) => (
                mission.rule_id.clone(),
                mission.name.as_str(),
                mission.creator.as_str(),
            ),
            None => (
                retired_mission_rule_id(tx, retired_rules, &mission_id)?,
                historical_title.as_str(),
                known_owner.unwrap_or("owner"),
            ),
        };
        let chat_id: String = row.get(0)?;
        let agent_id: String = row.get(1)?;
        let created_at: i64 = row.get(2)?;
        let run_id: String = row.get(3)?;
        let origin = json!({
            "hook": "clock",
            "ruleName": rule_name,
            "schedule": true,
        });
        insert.execute(params![
            chat_id,
            creator,
            agent_id,
            rule_id,
            origin.to_string(),
            run_id,
            created_at,
        ])?;
    }
    Ok(())
}
