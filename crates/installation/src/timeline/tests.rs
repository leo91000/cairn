use super::*;
use crate::store::Store;
use cairn_contracts::{FailureOrigin, NoticeCode, SpeedNotice, TimelineNotice, TimelineState};
use rusqlite::Connection;
use std::collections::BTreeMap;

// Dump produced by the previous main's real Store, run admission and event writer.
const PREVIOUS: &str = include_str!("../../tests/fixtures/timeline-v18.sql");

const CODEX: &str = "00000000-0000-4000-8000-0000000000a1";
const CLAUDE: &str = "00000000-0000-4000-8000-0000000000a2";
const OPENCODE: &str = "00000000-0000-4000-8000-0000000000a3";
const ACTIVITY: &str = "00000000-0000-4000-8000-0000000000a4";

fn previous(root: &std::path::Path) -> Connection {
    let db = Connection::open(root.join("manager.db")).unwrap();
    db.execute_batch(PREVIOUS).unwrap();
    std::fs::write(root.join("mcp-encryption-key"), [42_u8; 32]).unwrap();
    db
}

/// Every row of every table, except the projection this version adds.
fn tables(db: &Connection) -> BTreeMap<String, Vec<String>> {
    let names = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'timeline_%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let mut statement = db.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
            let columns = statement.column_count();
            let mut rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| {
                            Ok(format!(
                                "{:?}",
                                row.get::<_, rusqlite::types::Value>(index)?
                            ))
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map(|values| values.join("|"))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows.sort();
            (name, rows)
        })
        .collect()
}

fn version(db: &Connection) -> i64 {
    db.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

async fn cards(store: &Store, run: &str) -> Vec<TimelineEntry> {
    let run = run.to_owned();
    store.read(move |db| window(db, &run, 0)).await.unwrap()
}

#[tokio::test]
async fn private_answer_marker_hides_historical_and_live_payload_text() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let marker = "Answered a private question.";
    let secret = "private answer that must stay hidden";
    let payload = serde_json::json!({"text": secret});
    db.execute(
        "INSERT INTO events(run_id,created_at,type,text,payload) VALUES(?,?,?,?,?)",
        rusqlite::params![CODEX, 2, "chat.user", marker, payload.to_string()],
    )
    .unwrap();
    let store = Store::open(root.path()).unwrap();
    let migrated = cards(&store, CODEX).await;
    assert!(migrated.iter().any(
        |card| matches!(&card.content, TimelineContent::UserMessage { text, .. } if text == marker)
    ));
    assert!(!format!("{migrated:?}").contains(secret));

    store
        .event(CODEX, &EventKind::ChatUser, marker, Some(payload))
        .await
        .unwrap();
    let live = cards(&store, CODEX).await;
    assert_eq!(live.iter().filter(|card| matches!(&card.content, TimelineContent::UserMessage { text, .. } if text == marker)).count(), 2);
    assert!(!format!("{live:?}").contains(secret));
}

#[tokio::test]
async fn long_engine_item_ids_survive_migration_and_live_updates() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let first = "é".repeat(1100);
    let second = format!("{first}b");
    for id in [&first, &second] {
        let payload =
            serde_json::json!({"item": {"id": id, "type": "agent_message", "text": "Before"}});
        db.execute(
            "INSERT INTO events(run_id,created_at,type,text,payload) VALUES(?,?,?,?,?)",
            rusqlite::params![CODEX, 2, "item.updated", "Before", payload.to_string()],
        )
        .unwrap();
    }
    let before = tables(&db);
    let store = Store::open(root.path()).unwrap();
    assert_eq!(version(&db), 19);
    assert_eq!(tables(&db), before);
    let old = cards(&store, CODEX).await;
    let first_card = old.iter().find(|card| matches!(&card.content, TimelineContent::AgentMessage { item_id, .. } if item_id == &first)).unwrap();
    let second_card = old.iter().find(|card| matches!(&card.content, TimelineContent::AgentMessage { item_id, .. } if item_id == &second)).unwrap();
    assert_ne!(first_card.id, second_card.id);
    assert!(first_card.id.chars().count() <= 1024);
    let stable_id = first_card.id.clone();
    store.event(CODEX, &EventKind::ItemUpdated, "After", Some(serde_json::json!({"item": {"id": first, "type": "agent_message", "text": "After"}}))).await.unwrap();
    let updated = cards(&store, CODEX).await;
    assert!(updated.iter().any(|card| card.id == stable_id
        && matches!(&card.content, TimelineContent::AgentMessage { text, .. } if text == "After")));
}

#[tokio::test]
async fn checkpoint_keeps_no_failure_text_and_still_folds_truncated_reasons() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let store = Store::open(root.path()).unwrap();
    let message = "private diagnostic with a long tail";
    store
        .event(
            CODEX,
            &EventKind::TurnFailed,
            message,
            Some(serde_json::json!({"error":{"message":message}})),
        )
        .await
        .unwrap();
    store
        .event(
            CODEX,
            &EventKind::TurnFailed,
            message,
            Some(serde_json::json!({"error":{"message":message}})),
        )
        .await
        .unwrap();
    let latest: i64 = db
        .query_row(
            "SELECT max(id) FROM events WHERE run_id=?",
            [CODEX],
            |row| row.get(0),
        )
        .unwrap();
    let state: String = db
        .query_row(
            "SELECT state FROM timeline_state WHERE run_id=?",
            [CODEX],
            |row| row.get(0),
        )
        .unwrap();
    let state: serde_json::Value = serde_json::from_str(&state).unwrap();
    let digest = crate::auth::hex_digest(message);
    let reason = state["failure"]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reason| reason["digest"] == digest)
        .unwrap();
    assert_eq!(reason["source"], latest);
    store
        .event(
            CODEX,
            &EventKind::Status,
            "failed",
            Some(serde_json::json!({"error":{"message":"private diagnostic"}})),
        )
        .await
        .unwrap();
    let checkpoint: String = db
        .query_row(
            "SELECT state FROM timeline_state WHERE run_id=?",
            [CODEX],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!checkpoint.contains("private diagnostic"));
    let entries = cards(&store, CODEX).await;
    let matching: Vec<_> = entries
        .iter()
        .filter_map(|card| match &card.content {
            TimelineContent::Notice { notice }
                if notice
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.starts_with("private diagnostic")) =>
            {
                Some(notice)
            }
            _ => None,
        })
        .collect();
    assert_eq!(matching.iter().filter(|notice| notice.prominent).count(), 1);
}

fn notice(entry: &TimelineEntry) -> &TimelineNotice {
    match &entry.content {
        TimelineContent::Notice { notice } => notice,
        content => panic!("not a notice: {content:?}"),
    }
}

/// What a conversation shows: messages and prominent notices, in position order.
fn shown(cards: &[TimelineEntry]) -> Vec<String> {
    cards
        .iter()
        .filter_map(|card| match &card.content {
            TimelineContent::UserMessage { text, .. } => Some(format!("user: {text}")),
            TimelineContent::AgentMessage { echo: true, .. } => None,
            TimelineContent::AgentMessage {
                text, state, via, ..
            } => Some(format!(
                "agent{}: {text} ({state:?})",
                via.as_ref()
                    .map_or(String::new(), |via| format!(" via {via}"))
            )),
            TimelineContent::Notice { notice } if notice.prominent => Some(format!(
                "{:?} {:?}: {}",
                notice.code,
                notice.state,
                notice.detail.as_deref().unwrap_or_default()
            )),
            TimelineContent::Notice { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn upgrade_builds_the_timeline_from_retained_events_and_changes_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let before = tables(&db);
    assert!(before["events"].len() >= 48);

    let store = Store::open(root.path()).unwrap();
    assert_eq!(version(&db), 19);
    assert_eq!(tables(&db), before);
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        !db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );

    // The conversation reads as the clients used to fold it.
    let codex = cards(&store, CODEX).await;
    assert_eq!(
        shown(&codex),
        [
            "user: Fix the build",
            "agent: Looking at the build. Fixed. (Done)",
            "user: And the tests?",
            "agent via OpenRouter: Tests pass. (Done)",
            "user: Deploy it",
            "Failure Error: Rate limit exceeded",
        ]
    );
    let failure = codex
        .iter()
        .filter(|card| matches!(&card.content, TimelineContent::Notice { notice } if notice.code == NoticeCode::Failure))
        .collect::<Vec<_>>();
    // The first attempt's card announced the retry; the retried failure replaced it.
    assert_eq!(notice(failure[0]).retry.as_ref().unwrap().attempt, 1);
    assert!(!notice(failure[0]).prominent);
    assert_eq!(notice(failure[0]).origin, Some(FailureOrigin::Turn));
    let last = failure.iter().find(|card| notice(card).prominent).unwrap();
    assert_eq!(notice(last).retried, Some(1));
    assert_eq!(last.attempt, 1);
    // Reconnects recovered once work continued; a reused item ID is a new reply in its turn.
    let connections = codex
        .iter()
        .filter(|card| matches!(&card.content, TimelineContent::Notice { notice } if notice.code == NoticeCode::Connection))
        .collect::<Vec<_>>();
    assert_eq!(connections.len(), 2);
    assert!(
        connections
            .iter()
            .all(|card| notice(card).recovered && notice(card).state == TimelineState::Info)
    );
    let replies = codex
        .iter()
        .filter(|card| matches!(card.content, TimelineContent::AgentMessage { .. }))
        .map(|card| (card.id.as_str(), card.turn))
        .collect::<Vec<_>>();
    assert_eq!(replies, [("message:1:msg_1", 1), ("message:2:msg_1", 2)]);
    // Stderr that looks like protocol stays process output.
    let diagnostic = codex.last().unwrap();
    assert_eq!(notice(diagnostic).code, NoticeCode::Diagnostic);
    assert!(
        notice(diagnostic)
            .detail
            .as_deref()
            .unwrap()
            .starts_with("{\"type\":\"turn.completed\"}")
    );
    let TimelineContent::UserMessage { attachments, .. } = &codex
        .iter()
        .find(|card| card.id.starts_with("user:") && card.position > codex[0].position)
        .unwrap()
        .content
    else {
        unreachable!()
    };
    assert_eq!(attachments[0].name, "log.txt");

    let claude = cards(&store, CLAUDE).await;
    assert_eq!(
        shown(&claude),
        [
            "user: (private answer)",
            "Speed Info: ",
            "agent: Working on it (Done)",
            "EngineNotice Info: Context almost full",
            "RunStatus Stopped: ",
        ]
    );
    let speed = claude
        .iter()
        .find(|card| card.position == claude[0].position + 1);
    assert_eq!(notice(speed.unwrap()).speed, Some(SpeedNotice::Standard));
    let retries = claude
        .iter()
        .filter(|card| matches!(&card.content, TimelineContent::Notice { notice } if notice.code == NoticeCode::EngineRetry))
        .collect::<Vec<_>>();
    assert!(!notice(retries[0]).prominent && !notice(retries[0]).recovered);
    assert!(notice(retries[1]).recovered);
    // The limit belonged to the stopped execution; the adapter's stop echo is not a failure.
    let limit = claude
        .iter()
        .find(|card| notice_code(card) == Some(NoticeCode::UsageLimit))
        .unwrap();
    assert!(!notice(limit).prominent);
    let echo = claude
        .iter()
        .find(|card| notice_code(card) == Some(NoticeCode::Failure))
        .unwrap();
    assert!(!notice(echo).prominent);
    assert_eq!(
        notice(claude.last().unwrap()).status.as_deref(),
        Some("cancelled")
    );

    let opencode = cards(&store, OPENCODE).await;
    assert_eq!(
        shown(&opencode),
        [
            "agent: Partial (Done)",
            "Failure Error: controller lost",
            "RunStatus Stopped: ",
        ]
    );

    // Reopening the upgraded database performs no further migration.
    drop(store);
    let rows = tables(&db);
    let projection: i64 = db
        .query_row("SELECT COUNT(*) FROM timeline_entries", [], |r| r.get(0))
        .unwrap();
    Store::open(root.path()).unwrap();
    assert_eq!(tables(&db), rows);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM timeline_entries", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        projection
    );
}

fn notice_code(card: &TimelineEntry) -> Option<NoticeCode> {
    match &card.content {
        TimelineContent::Notice { notice } => Some(notice.code),
        _ => None,
    }
}

#[test]
fn a_failed_upgrade_leaves_version_18_and_its_events_intact() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    // A conflicting leftover table makes the projection's schema fail mid-upgrade.
    db.execute_batch("CREATE TABLE timeline_state(run_id TEXT);")
        .unwrap();
    let before = tables(&db);
    assert!(Store::open(root.path()).is_err());
    assert_eq!(version(&db), 18);
    assert_eq!(tables(&db), before);
    assert!(
        !db.prepare("SELECT 1 FROM sqlite_master WHERE name='timeline_entries'")
            .unwrap()
            .exists([])
            .unwrap()
    );
}

/// The projection as stored, with the run identity normalized.
fn stored(db: &Connection, run: &str) -> Vec<(String, i64, i64, i64, String)> {
    db.prepare("SELECT id,position,revision,source,content FROM timeline_entries WHERE run_id=? ORDER BY position,id")
        .unwrap()
        .query_map([run], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn normalize(mut cards: Vec<Card>) -> Vec<(String, i64, i64, i64, String)> {
    cards.sort_by(|a, b| (a.position, &a.id).cmp(&(b.position, &b.id)));
    cards
        .into_iter()
        .map(|card| {
            (
                card.id,
                card.position,
                card.revision,
                card.source,
                stored_content(&card.content).unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn live_batches_full_replay_and_checkpoint_restart_build_the_same_timeline() {
    let root = tempfile::tempdir().unwrap();
    let db = previous(root.path());
    let corpus = db
        .prepare("SELECT run_id,created_at,type,text,payload FROM events ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let store = Store::open(root.path()).unwrap();
    // Record the corpus again through the live writer, in transactions of varying size.
    db.execute_batch(
        "DELETE FROM timeline_entries; DELETE FROM timeline_state; DELETE FROM events;",
    )
    .unwrap();
    drop(db);
    let mut batch = Vec::new();
    for (index, event) in corpus.into_iter().enumerate() {
        batch.push(event);
        if index % 3 != 2 && index % 7 != 0 {
            continue;
        }
        let events = std::mem::take(&mut batch);
        store
            .transaction(move |db| {
                for (run, created_at, kind, text, payload) in events {
                    let payload = payload
                        .map(|p| serde_json::from_str::<Value>(&p))
                        .transpose()?;
                    db.event_at(
                        &run,
                        &EventKind::from_adapter(&kind),
                        &text,
                        payload.as_ref(),
                        created_at,
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
    }
    let events = std::mem::take(&mut batch);
    store
        .write(move |db| {
            for (run, created_at, kind, text, payload) in events {
                let payload = payload
                    .map(|p| serde_json::from_str::<Value>(&p))
                    .transpose()?;
                db.event_at(
                    &run,
                    &EventKind::from_adapter(&kind),
                    &text,
                    payload.as_ref(),
                    created_at,
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    drop(store);

    let db = Connection::open(root.path().join("manager.db")).unwrap();
    for run in [CODEX, CLAUDE, OPENCODE] {
        let events = db
            .prepare("SELECT id,run_id,created_at,type,text,payload FROM events WHERE run_id=? ORDER BY id")
            .unwrap()
            .query_map([run], |row| {
                Ok(StoredEvent {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    created_at: row.get(2)?,
                    kind: EventKind::from_adapter(&row.get::<_, String>(3)?),
                    text: row.get(4)?,
                    payload: row
                        .get::<_, Option<String>>(5)?
                        .map(|p| serde_json::value::RawValue::from_string(p).unwrap()),
                    display_id: None,
                    timeline: false,
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let recorded = events;
        let mut full = Replay::default();
        for event in &recorded {
            full.apply(event).unwrap();
        }
        let (state, cards) = full.finish();
        assert_eq!(stored(&db, run), normalize(cards.clone()), "{run}");
        let checkpoint: String = db
            .query_row(
                "SELECT state FROM timeline_state WHERE run_id=?",
                [run],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<State>(&checkpoint).unwrap(), state);

        // Restarting from a persisted checkpoint continues exactly like the full replay.
        for split in 1..recorded.len() {
            let mut first = Replay::default();
            for event in &recorded[..split] {
                first.apply(event).unwrap();
            }
            let saved_state = serde_json::to_string(&first.state).unwrap();
            let saved_cards = serde_json::to_string(&first.cards).unwrap();
            let mut resumed = Replay {
                state: serde_json::from_str(&saved_state).unwrap(),
                cards: serde_json::from_str(&saved_cards).unwrap(),
                order: first.order,
                // The real writer reads these reasons from the retained source events.
                reasons: first.reasons,
            };
            for event in &recorded[split..] {
                resumed.apply(event).unwrap();
            }
            let (resumed_state, resumed_cards) = resumed.finish();
            assert_eq!(resumed_state, state);
            assert_eq!(normalize(resumed_cards), normalize(cards.clone()));
        }
    }
}

/// The wire both apps render: each run's UI events and the server's cards for them.
/// `CAIRN_UPDATE_FIXTURES=1` rewrites the shared corpus after a deliberate change.
#[tokio::test]
async fn shared_client_corpus_matches_the_server_timeline() {
    let root = tempfile::tempdir().unwrap();
    previous(root.path());
    let store = Store::open(root.path()).unwrap();
    // The tool-activity sample the apps already share, recorded as one more run.
    let sample: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/activity-events.json"
    ))
    .unwrap();
    store
        .transaction(move |db| {
            let mut state = db.run_state(OPENCODE)?.unwrap();
            state.id = ACTIVITY.into();
            db.admit_run(
                &state,
                db.run_snapshot(OPENCODE)?.as_ref(),
                &serde_json::json!({}),
                None,
            )?;
            for (index, payload) in sample.into_iter().enumerate() {
                let kind = EventKind::from_adapter(payload["type"].as_str().unwrap());
                db.event_at(ACTIVITY, &kind, "", Some(&payload), index as i64)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut runs = Vec::new();
    for run in [CODEX, CLAUDE, OPENCODE, ACTIVITY] {
        let id = run.to_owned();
        let (events, entries) = store
            .read(move |db| {
                let events = crate::live::ui_events(db.event_page(&id, 0, 1000)?);
                Ok((events, window(db, &id, 0)?))
            })
            .await
            .unwrap();
        runs.push(serde_json::json!({ "runId": run, "events": events, "entries": entries }));
    }
    let corpus = serde_json::to_string_pretty(&serde_json::json!({ "runs": runs })).unwrap() + "\n";
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/timeline-corpus.json"
    );
    if std::env::var_os("CAIRN_UPDATE_FIXTURES").is_some() {
        std::fs::write(path, &corpus).unwrap();
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), corpus);
}

fn replay(events: &[(EventKind, &str, Value)]) -> Vec<TimelineEntry> {
    let stored = events
        .iter()
        .enumerate()
        .map(|(index, (kind, text, payload))| StoredEvent {
            id: index as i64 + 1,
            run_id: "run".into(),
            created_at: index as i64,
            kind: kind.clone(),
            text: (*text).to_owned(),
            payload: (!payload.is_null())
                .then(|| serde_json::value::RawValue::from_string(payload.to_string()).unwrap()),
            display_id: None,
            timeline: false,
        })
        .collect::<Vec<_>>();
    let mut replay = Replay::default();
    for event in &stored {
        replay.apply(event).unwrap();
    }
    replay
        .finish()
        .1
        .into_iter()
        .map(|card| {
            let source = &stored[card.source as usize - 1];
            hydrate(
                card,
                &Source {
                    created_at: source.created_at,
                    kind: source.kind.clone(),
                    text: source.text.clone(),
                    payload: source
                        .payload
                        .as_ref()
                        .map(|p| serde_json::from_str(p.get()).unwrap()),
                },
                "run",
            )
        })
        .collect()
}

#[test]
fn usage_limits_and_replies_repeating_a_failure_stay_in_the_run_log() {
    use serde_json::json;
    let reply = |text: &str| {
        (
            EventKind::ItemCompleted,
            "",
            json!({"item": {"id": "r", "type": "agent_message", "text": text}}),
        )
    };
    let cards = replay(&[
        (EventKind::ChatUser, "Go", Value::Null),
        (EventKind::TurnStarted, "", Value::Null),
        reply("You've hit your usage limit. Try again later."),
        (
            EventKind::TurnFailed,
            "",
            json!({"error": {"message": "You've hit your usage limit. Try again later.", "resetsAt": 1800000000000_i64}}),
        ),
        (
            EventKind::Error,
            "You've hit your usage limit. Try again later.",
            json!({"message": "You've hit your usage limit. Try again later."}),
        ),
        (EventKind::Status, "failed", Value::Null),
    ]);
    assert_eq!(shown(&cards), ["user: Go"]);
    let reply = cards.iter().find(|card| card.id == "message:1:r").unwrap();
    assert!(matches!(
        reply.content,
        TimelineContent::AgentMessage { echo: true, .. }
    ));
    // The runner repeats the failure without its reset: the latest card keeps it and is current.
    let quotas = cards
        .iter()
        .filter_map(|card| match &card.content {
            TimelineContent::Notice { notice } => notice.quota.as_ref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(quotas.len(), 2);
    assert!(!quotas[0].current && quotas[1].current);
    assert_eq!(quotas[1].resets_at, Some(1800000000000));
    assert_eq!(
        quotas[1].message,
        "You've hit your usage limit. Try again later."
    );

    // Claude Code says an API error as a reply; the failure card already reports it.
    let cards = replay(&[
        (EventKind::ChatUser, "Go", Value::Null),
        (EventKind::TurnStarted, "", Value::Null),
        (
            EventKind::ItemCompleted,
            "",
            json!({"item": {"id": "a", "type": "agent_message", "text": "API Error: 500"}}),
        ),
        (
            EventKind::TurnFailed,
            "API Error: 500",
            json!({"error": {"message": "API Error: 500"}}),
        ),
        (EventKind::ChatUser, "Again", Value::Null),
        (EventKind::TurnStarted, "", Value::Null),
        (EventKind::EngineRetry, "", json!({"attempt": 1})),
        (
            EventKind::TurnFailed,
            "Overloaded",
            json!({"error": {"message": "Overloaded"}}),
        ),
    ]);
    assert_eq!(
        shown(&cards),
        [
            "user: Go",
            "Failure Error: API Error: 500",
            "user: Again",
            "Failure Error: Overloaded"
        ]
    );
    // The engine gave up on its retries: they did not recover.
    let retry = cards
        .iter()
        .find(|card| notice_code(card) == Some(NoticeCode::EngineRetry))
        .unwrap();
    assert!(!notice(retry).recovered && !notice(retry).prominent);
}

#[test]
fn structured_usage_codes_hide_their_echo_unless_the_engine_retries() {
    use serde_json::json;
    let text = "Account usage is exhausted.";
    for code in [
        json!({"code": "usage_limit_reached"}),
        json!({"codexErrorInfo": "usageLimitExceeded"}),
    ] {
        let mut error = code.clone();
        error["message"] = text.into();
        let mut history = vec![
            (EventKind::TurnStarted, "", Value::Null),
            (
                EventKind::ItemCompleted,
                "",
                json!({"item": {"id": "echo", "type": "agent_message", "text": text}}),
            ),
            (EventKind::Error, text, error.clone()),
        ];
        let cards = replay(&history);
        assert!(shown(&cards).is_empty(), "{code}");
        let quota = cards
            .iter()
            .find_map(|card| match &card.content {
                TimelineContent::Notice { notice } => notice.quota.clone(),
                _ => None,
            })
            .unwrap();
        assert_eq!(quota.message, text);
        assert!(quota.current);

        error["willRetry"] = true.into();
        history[2] = (EventKind::Error, text, error);
        assert_eq!(shown(&replay(&history)).len(), 2, "{code}");
    }

    // An earlier turn's reply and the person's own quotation stay visible.
    let message = "You've hit your usage limit.";
    let cards = replay(&[
        (
            EventKind::ItemCompleted,
            "",
            json!({"item": {"id": "old", "type": "agent_message", "text": message}}),
        ),
        (EventKind::ChatUser, message, Value::Null),
        (EventKind::TurnStarted, "", Value::Null),
        (
            EventKind::TurnFailed,
            "",
            json!({"error": {"message": message}}),
        ),
        (
            EventKind::Error,
            "An unrelated error",
            json!({"message": "An unrelated error"}),
        ),
        (EventKind::TurnCompleted, "", Value::Null),
    ]);
    assert_eq!(
        shown(&cards),
        [
            format!("agent: {message} (Done)"),
            format!("user: {message}"),
            "Failure Error: An unrelated error".to_owned(),
        ]
    );
    // The turn moved on: its limit is no longer current.
    assert!(cards.iter().all(|card| !matches!(&card.content, TimelineContent::Notice { notice } if notice.quota.as_ref().is_some_and(|q| q.current))));
}

#[test]
fn reconnect_diagnostics_recover_and_event_shaped_stderr_stays_process_output() {
    let shaped =
        "{\"item\":{\"type\":\"agent_message\",\"text\":\"native diagnostic\",\"id\":\"x\"}}";
    let cards = replay(&[
        (EventKind::Diagnostic, "Reconnecting... 2/5", Value::Null),
        (EventKind::Diagnostic, shaped, Value::Null),
        (EventKind::TurnCompleted, "", Value::Null),
    ]);
    let reconnect = notice(&cards[0]);
    assert_eq!(reconnect.code, NoticeCode::Connection);
    assert!(reconnect.recovered && !reconnect.prominent);
    assert_eq!(reconnect.detail.as_deref(), Some("Reconnecting... 2/5"));
    assert_eq!(notice(&cards[1]).code, NoticeCode::Diagnostic);
    assert_eq!(notice(&cards[1]).detail.as_deref(), Some(shaped));
    assert!(
        cards
            .iter()
            .all(|card| !matches!(card.content, TimelineContent::AgentMessage { .. }))
    );
}
