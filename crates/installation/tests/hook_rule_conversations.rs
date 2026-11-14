//! Conversation keys, conversation actions and Cairn hooks (ADR-0075, « Clé de
//! conversation », « Valeurs mémorisées », « Hooks de Cairn » and « Budget du moteur de
//! règles »): from a plugin event to a keyed conversation, and from its turns back to
//! the rules of the key's creator.
mod common;

use axum::{body::Body, http::StatusCode};
use cairn_contracts::EventKind;
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    config::MAIN_AGENT_ID,
    rules::engine,
    validation::text,
};
use serde_json::{Value, json};

#[path = "fixtures/plugin_grants.rs"]
mod fixture;

use common::agent_fixture::AgentFixture;
use fixture::{Fixture, contribution, support};

const OWNER: &str = "fixture-owner";
const MEMBER: &str = "member-account";
const MAX_CHAIN_DEPTH: u64 = 8;

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

fn message(author: &str, text: &str) -> Value {
    json!({ "channel": "support", "author": author, "large": text })
}

fn key() -> Value {
    json!({ "type": "template", "text": "chat:{{author}}" })
}

fn action(id: &str, action: &Value) -> Value {
    json!({ "type": "action", "id": id, "action": action })
}

fn start(id: &str) -> Value {
    action(
        id,
        &json!({
            "type": "startAgent",
            "agentId": MAIN_AGENT_ID,
            "prompt": { "type": "formula", "expression": "large" },
            "key": key(),
            "author": { "type": "formula", "expression": "author" },
        }),
    )
}

fn steer(id: &str, key: &Value, message: &Value) -> Value {
    action(
        id,
        &json!({ "type": "steerConversation", "key": key, "message": message }),
    )
}

/// Starts a conversation for an author's first message, and steers it afterwards.
fn start_or_steer() -> Value {
    json!([{
        "type": "if",
        "id": "bound",
        "condition": { "type": "conversationExists", "key": key() },
        "then": [steer("steer", &key(), &json!({ "type": "formula", "expression": "large" }))],
        "otherwise": [start("start")],
    }])
}

async fn rule_on(
    f: &Fixture,
    account: &str,
    trigger: Value,
    steps: Value,
    limits: Value,
) -> String {
    let (status, rule) = call(
        f,
        account,
        "POST",
        "/api/hook-rules",
        json!({
            "definition": {
                "name": "Conversations",
                "trigger": trigger,
                "executionAgentId": MAIN_AGENT_ID,
                "steps": steps,
                "limits": limits,
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    text(&rule, "id").to_owned()
}

fn plugin(f: &Fixture) -> Value {
    json!({ "type": "plugin", "instanceId": f.instance, "hook": "message" })
}

fn cairn(hook: &str) -> Value {
    json!({ "type": "cairn", "hook": hook })
}

fn limits() -> Value {
    json!({ "triggeringsPerHour": 60, "conversationsPerHour": 10 })
}

async fn triggerings(f: &Fixture, rule: &str) -> Vec<Value> {
    let (status, listed) = call(
        f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{rule}/triggerings"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let mut details = Vec::new();
    for triggering in listed["triggerings"].as_array().unwrap() {
        let (_, detail) = call(
            f,
            OWNER,
            "GET",
            &format!("/api/hook-rule-triggerings/{}", text(triggering, "id")),
            Value::Null,
        )
        .await;
        details.push(detail);
    }
    details
}

async fn bound(f: &Fixture, creator: &str, key: &str) -> Option<String> {
    let (creator, key) = (creator.to_owned(), key.to_owned());
    f.s.store
        .read(move |db| {
            Ok(db
                .0
                .query_row(
                    "SELECT chat_id FROM hook_rule_conversations WHERE creator_account_id=?1 AND key=?2",
                    [creator, key],
                    |row| row.get::<_, String>(0),
                )
                .ok())
        })
        .await
        .unwrap()
}

async fn values(f: &Fixture, rule: &str) -> Value {
    let rule = rule.to_owned();
    f.s.store
        .read(move |db| {
            let mut values = serde_json::Map::new();
            let mut statement =
                db.0.prepare("SELECT name,value FROM hook_rule_values WHERE rule_id=?")?;
            for row in statement.query_map([rule], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })? {
                let (name, value) = row?;
                values.insert(name, serde_json::from_str(&value)?);
            }
            Ok(Value::Object(values))
        })
        .await
        .unwrap()
}

/// Starts the run of a conversation the rule engine queued, as the worker would.
async fn launched(f: &Fixture, chat: &str) -> String {
    f.s.chat_tick(&std::collections::HashSet::new())
        .await
        .unwrap();
    let detail = f.s.chat_detail(chat).await.unwrap();
    text(&detail["run"], "id").to_owned()
}

async fn event(f: &Fixture, run: &str, kind: &str, text: &str, payload: Option<Value>) {
    f.s.store
        .event(run, &EventKind::from_adapter(kind), text, payload)
        .await
        .unwrap();
}

/// A whole turn of a delivered message that ends with a reply.
async fn replied(f: &Fixture, run: &str, message: &str, reply: &str) {
    event(
        f,
        run,
        "chat.user",
        "",
        Some(json!({ "messageId": message })),
    )
    .await;
    event(f, run, "turn.started", "", None).await;
    let item = json!({ "item": { "type": "agent_message", "text": reply } });
    event(f, run, "item.completed", reply, Some(item)).await;
    event(f, run, "turn.completed", "", None).await;
}

/// Dates the last progress back, as if eight seconds had passed.
fn age_progress(
    db: &mut cairn_installation::store::Db<'_>,
    chat: &str,
) -> cairn_installation::error::Result<()> {
    db.0.execute(
        "UPDATE hook_rule_conversations SET watch=json_set(watch,'$.progressAt',json_extract(watch,'$.progressAt')-9000) WHERE chat_id=?",
        [chat],
    )?;
    Ok(())
}

fn remember(id: &str, name: &str, expression: &str) -> Value {
    action(
        id,
        &json!({
            "type": "storeValue", "scope": "rule", "name": name,
            "value": { "type": "formula", "expression": expression },
        }),
    )
}

fn result<'a>(steps: &'a [Value], node: &str) -> &'a Value {
    let step = steps
        .iter()
        .find(|step| step["nodeId"] == node)
        .unwrap_or_else(|| panic!("no step {node} in {steps:?}"));
    &step["result"]
}

async fn depth_of(f: &Fixture, rule: &str) -> Vec<u64> {
    let rule = rule.to_owned();
    f.s.store
        .read(move |db| {
            Ok(db
                .0
                .prepare("SELECT event FROM hook_rule_triggerings WHERE rule_id=? ORDER BY created_at,id")?
                .query_map([rule], |row| row.get::<_, String>(0))?
                .map(|event| {
                    let event: Value = serde_json::from_str(&event.unwrap()).unwrap();
                    event["cairn"]["depth"].as_u64().unwrap_or(0)
                })
                .collect())
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn messages_start_one_keyed_conversation_per_author_and_then_steer_it() {
    let f = granted().await;
    let rule = rule_on(&f, OWNER, plugin(&f), start_or_steer(), limits()).await;

    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.expect("bound");
    let started = triggerings(&f, &rule).await;
    let steps = started[0]["steps"].as_array().unwrap();
    assert_eq!(result(steps, "start")["conversationId"], chat);

    // The conversation records the external origin, and its creator as author.
    let origin = {
        let chat = chat.clone();
        f.s.store
            .read(move |db| {
                Ok(db.0.query_row(
                    "SELECT creator_account_id,author,origin FROM hook_rule_conversations WHERE chat_id=?",
                    [chat],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
                )?)
            })
            .await
            .unwrap()
    };
    assert_eq!(origin.0, OWNER);
    assert_eq!(origin.1, "alice");
    let external: Value = serde_json::from_str(&origin.2).unwrap();
    assert_eq!(external["hook"], "message");
    assert_eq!(external["instanceId"], f.instance);

    emit(&f, "second", message("alice", "and then")).await;
    emit(&f, "other", message("bob", "hi")).await;
    engine::tick(&f.s).await.unwrap();
    let messages = f.s.chat_detail(&chat).await.unwrap();
    let texts: Vec<_> = messages["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| text(message, "text").to_owned())
        .collect();
    assert_eq!(texts, ["hello", "and then"], "{messages}");
    let other = bound(&f, OWNER, "chat:bob").await.expect("bob's key");
    assert_ne!(other, chat);
}

#[tokio::test]
async fn starting_an_agent_with_a_bound_key_fails_and_keys_belong_to_their_creator() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let owner = rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let member = rule_on(&f, MEMBER, plugin(&f), json!([start("start")]), limits()).await;

    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let mine = bound(&f, OWNER, "chat:alice").await.unwrap();
    let theirs = bound(&f, MEMBER, "chat:alice").await.unwrap();
    assert_ne!(mine, theirs, "Two creators' keys name two conversations.");

    emit(&f, "again", message("alice", "hello again")).await;
    engine::tick(&f.s).await.unwrap();
    for rule in [owner, member] {
        let latest = &triggerings(&f, &rule).await[0];
        assert_eq!(latest["status"], "failed", "{latest}");
        assert!(text(latest, "error").contains("already bound"), "{latest}");
    }
    // Nothing was started by the failed action.
    let count =
        f.s.store
            .read(|db| {
                Ok(db
                    .0
                    .query_row("SELECT count(*) FROM hook_rule_conversations", [], |row| {
                        row.get::<_, i64>(0)
                    })?)
            })
            .await
            .unwrap();
    assert_eq!(count, 2);

    // Deleting the conversation frees its key.
    f.s.store
        .transaction(move |db| {
            db.0.execute("DELETE FROM conversations WHERE id=?", [mine])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(bound(&f, OWNER, "chat:alice").await.is_none());
}

#[tokio::test]
async fn cairn_hooks_report_the_reply_question_progress_and_end_of_keyed_turns() {
    let f = granted().await;
    let starter = rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let reply = rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([
            remember("text", "reply", "text"),
            remember("origin", "origin", "origin.kind"),
            remember("key", "key", "key"),
        ]),
        limits(),
    )
    .await;
    let ended = rule_on(
        &f,
        OWNER,
        cairn("ended"),
        json!([remember("outcome", "outcome", "outcome")]),
        limits(),
    )
    .await;
    let asked = rule_on(
        &f,
        OWNER,
        cairn("questionAsked"),
        json!([remember("question", "question", "title")]),
        limits(),
    )
    .await;
    let progress = rule_on(
        &f,
        OWNER,
        cairn("progress"),
        json!([remember("step", "step", "step")]),
        json!({ "triggeringsPerHour": 1, "conversationsPerHour": 1 }),
    )
    .await;

    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let start = triggerings(&f, &starter).await;
    let message_id = text(
        result(start[0]["steps"].as_array().unwrap(), "start"),
        "messageId",
    )
    .to_owned();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;

    event(
        &f,
        &run,
        "chat.user",
        "",
        Some(json!({ "messageId": message_id })),
    )
    .await;
    event(&f, &run, "turn.started", "", None).await;
    f.s.question_receive(
        &run,
        json!({
            "id": "e".repeat(64),
            "blocking": true,
            "fields": [{ "id": "color", "title": "Which color?", "options": [] }],
        }),
    )
    .await
    .unwrap();
    engine::tick(&f.s).await.unwrap();
    // Progress comes about every eight seconds of a working turn.
    f.s.store
        .transaction({
            let chat = chat.clone();
            move |db| age_progress(db, &chat)
        })
        .await
        .unwrap();
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &asked).await["question"], "Which color?");
    assert_eq!(values(&f, &progress).await["step"], "Thinking");

    let item = json!({ "item": { "type": "agent_message", "text": "Blue." } });
    event(&f, &run, "item.completed", "Blue.", Some(item)).await;
    event(&f, &run, "turn.completed", "", None).await;
    engine::tick(&f.s).await.unwrap();
    let remembered = values(&f, &reply).await;
    assert_eq!(remembered["reply"], "Blue.");
    assert_eq!(remembered["origin"], "rule");
    assert_eq!(remembered["key"], "chat:alice");
    assert_eq!(values(&f, &ended).await["outcome"], "completed");
    // A rule's start is one link of the chain; its hooks carry that depth.
    assert_eq!(depth_of(&f, &reply).await, [1]);

    // A person's message starts a new chain with their origin.
    f.s.chat_send(
        &chat,
        json!({ "id": uuid::Uuid::new_v4().to_string(), "text": "thanks" }),
    )
    .await
    .unwrap();
    let message = {
        let detail = f.s.chat_detail(&chat).await.unwrap();
        text(detail["messages"].as_array().unwrap().last().unwrap(), "id").to_owned()
    };
    replied(&f, &run, &message, "You're welcome.").await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &reply).await["reply"], "You're welcome.");
    assert_eq!(depth_of(&f, &reply).await, [1, 0]);
}

#[tokio::test]
async fn progress_does_not_count_against_the_triggering_cap() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let progress = rule_on(
        &f,
        OWNER,
        cairn("progress"),
        json!([remember("step", "step", "step")]),
        json!({ "triggeringsPerHour": 1, "conversationsPerHour": 1 }),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    event(&f, &run, "turn.started", "", None).await;
    engine::tick(&f.s).await.unwrap();
    for _ in 0..3 {
        f.s.store
            .transaction({
                let chat = chat.clone();
                move |db| age_progress(db, &chat)
            })
            .await
            .unwrap();
        engine::tick(&f.s).await.unwrap();
    }
    let history = triggerings(&f, &progress).await;
    assert_eq!(history.len(), 3, "{history:?}");
    assert!(history.iter().all(|t| t["status"] == "completed"));
    let (_, rule) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{progress}"),
        Value::Null,
    )
    .await;
    assert_eq!(rule["blocks"], json!([]), "{rule}");
}

#[tokio::test]
async fn one_progress_waits_at_a_time_per_conversation() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    // The progress rule waits, so its triggering stays pending.
    let progress = rule_on(
        &f,
        OWNER,
        cairn("progress"),
        json!([action(
            "pause",
            &json!({ "type": "wait", "seconds": { "type": "literal", "value": 600 } })
        )]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    event(&f, &run, "turn.started", "", None).await;
    engine::tick(&f.s).await.unwrap();
    for _ in 0..3 {
        f.s.store
            .transaction({
                let chat = chat.clone();
                move |db| age_progress(db, &chat)
            })
            .await
            .unwrap();
        engine::tick(&f.s).await.unwrap();
    }
    assert_eq!(triggerings(&f, &progress).await.len(), 1);
}

#[tokio::test]
async fn chains_followed_by_cairn_stop_at_their_documented_depth() {
    let f = granted().await;
    let (_, catalog) = call(&f, OWNER, "GET", "/api/hook-rules/catalog", Value::Null).await;
    assert_eq!(catalog["limits"]["maxChainDepth"], MAX_CHAIN_DEPTH);

    // A plugin payload cannot claim a depth: its fields stay under `payload`.
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let mut forged = message("alice", "hello");
    forged["cairn"] = json!({ "depth": MAX_CHAIN_DEPTH, "conversationId": "elsewhere" });
    emit(&f, "first", forged).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice")
        .await
        .expect("started at depth 0");

    // Every reply steers the conversation again: a loop only the depth stops.
    let looping = rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([steer(
            "again",
            &json!({ "type": "formula", "expression": "key" }),
            &json!({ "type": "literal", "value": "again" })
        )]),
        json!({ "triggeringsPerHour": 600, "conversationsPerHour": 100 }),
    )
    .await;
    let run = launched(&f, &chat).await;
    for turn in 0..MAX_CHAIN_DEPTH {
        let message = {
            let detail = f.s.chat_detail(&chat).await.unwrap();
            text(detail["messages"].as_array().unwrap().last().unwrap(), "id").to_owned()
        };
        replied(&f, &run, &message, &format!("turn {turn}")).await;
        engine::tick(&f.s).await.unwrap();
    }
    let history = triggerings(&f, &looping).await;
    assert_eq!(
        depth_of(&f, &looping).await,
        (1..=MAX_CHAIN_DEPTH).collect::<Vec<_>>()
    );
    let last = &history[0];
    assert_eq!(last["status"], "failed", "{last}");
    assert!(text(last, "error").contains("maximum depth"), "{last}");
    assert!(
        history[1..].iter().all(|t| t["status"] == "completed"),
        "{history:?}"
    );
}

#[tokio::test]
async fn author_predicates_and_quotas_read_the_creators_keys() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let quota = rule_on(
        &f,
        OWNER,
        plugin(&f),
        json!([{
            "type": "if",
            "id": "allowed",
            "condition": {
                "type": "all",
                "conditions": [
                    { "type": "authorQuota", "author": { "type": "formula", "expression": "author" }, "limit": 2, "windowMinutes": 60 },
                    {
                        "type": "authorStartedConversation",
                        "author": { "type": "formula", "expression": "author" },
                        "key": { "type": "literal", "value": "chat:alice" },
                    },
                ],
            },
            "then": [remember("yes", "allowed", "large")],
            "otherwise": [remember("no", "refused", "large")],
        }]),
        limits(),
    )
    .await;
    for (index, (author, text)) in [
        ("alice", "one"),
        ("bob", "bob"),
        ("alice", "two"),
        ("alice", "three"),
    ]
    .into_iter()
    .enumerate()
    {
        emit(&f, &format!("event-{index}"), message(author, text)).await;
        engine::tick(&f.s).await.unwrap();
    }
    let remembered = values(&f, &quota).await;
    // The first event ran before alice's conversation existed.
    assert_eq!(remembered["allowed"], "two");
    assert_eq!(remembered["refused"], "three");
    let refused: Vec<_> = triggerings(&f, &quota)
        .await
        .iter()
        .filter(|t| t["steps"][0]["nodeId"] == "no")
        .map(|t| t["steps"][0]["result"].clone())
        .collect();
    assert_eq!(refused, [json!("three"), json!("bob"), json!("one")]);
}

#[tokio::test]
async fn conversation_values_are_shared_by_key_and_reach_cairn_hooks() {
    let f = granted().await;
    rule_on(
        &f,
        OWNER,
        plugin(&f),
        json!([
            start("start"),
            action(
                "note",
                &json!({
                    "type": "storeValue", "scope": "conversation", "key": key(), "name": "thread",
                    "value": { "type": "formula", "expression": "channel" },
                })
            ),
        ]),
        limits(),
    )
    .await;
    let reply = rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([remember("thread", "thread", "payload.values.thread")]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    let message = {
        let detail = f.s.chat_detail(&chat).await.unwrap();
        text(&detail["messages"][0], "id").to_owned()
    };
    replied(&f, &run, &message, "done").await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &reply).await["thread"], "support");

    // They go with the conversation.
    f.s.store
        .transaction(move |db| {
            db.0.execute("DELETE FROM conversations WHERE id=?", [chat])?;
            Ok(())
        })
        .await
        .unwrap();
    let left =
        f.s.store
            .read(|db| {
                Ok(db.0.query_row(
                    "SELECT count(*) FROM hook_rule_conversation_values",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_paused_conversation_refuses_steering_and_answers_until_a_person_resumes_it() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), start_or_steer(), limits()).await;
    let answering = rule_on(
        &f,
        OWNER,
        cairn("questionAsked"),
        json!([action(
            "answer",
            &json!({
                "type": "answerQuestion",
                "key": { "type": "formula", "expression": "key" },
                "answer": { "type": "literal", "value": "Blue" },
            })
        )]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    event(&f, &run, "turn.started", "", None).await;
    let question = "e".repeat(64);
    f.s.question_receive(
        &run,
        json!({
            "id": question,
            "blocking": true,
            "fields": [{ "id": "color", "title": "Which color?", "options": [] }],
        }),
    )
    .await
    .unwrap();
    engine::tick(&f.s).await.unwrap();
    let answered = &triggerings(&f, &answering).await[0];
    assert_eq!(answered["status"], "completed", "{answered}");
    assert_eq!(answered["steps"][0]["result"]["questionId"], question);
    let detail = f.s.chat_detail(&chat).await.unwrap();
    assert!(
        detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["questionId"] == question.as_str()),
        "{detail}"
    );

    event(&f, &run, "turn.completed", "", None).await;
    f.s.chat_pause(&chat, true).await.unwrap();
    emit(&f, "second", message("alice", "are you there?")).await;
    engine::tick(&f.s).await.unwrap();
    let detail = f.s.chat_detail(&chat).await.unwrap();
    assert!(
        !detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["text"] == "are you there?"),
        "A person's pause holds rules back."
    );
}

#[tokio::test]
async fn cancelling_by_key_stops_the_turn_and_reports_its_end() {
    let f = granted().await;
    let canceller = rule_on(
        &f,
        OWNER,
        plugin(&f),
        json!([{
            "type": "if",
            "id": "bound",
            "condition": { "type": "conversationExists", "key": key() },
            "then": [action(
        "stop",
        &json!({ "type": "cancelConversation", "key": key() }))],
            "otherwise": [start("start")],
        }]),
        limits(),
    )
    .await;
    let ended = rule_on(
        &f,
        OWNER,
        cairn("ended"),
        json!([remember("outcome", "outcome", "outcome")]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    fixture::running_conversation(&f.s, &chat).await;
    event(&f, &run, "turn.started", "", None).await;
    engine::tick(&f.s).await.unwrap();

    emit(&f, "stop", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    let stopped = &triggerings(&f, &canceller).await[0];
    assert_eq!(
        stopped["steps"][0]["result"]["cancelled"], true,
        "{stopped}"
    );
    let state = f.s.store.run(&run).await.unwrap();
    assert!(state["cancelRequestedAt"].is_number(), "{state}");

    event(&f, &run, "status", "cancelled", None).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &ended).await["outcome"], "cancelled");
    // The end belongs to the cancelling rule's chain.
    assert_eq!(depth_of(&f, &ended).await, [1]);
}

#[tokio::test]
async fn losing_access_or_merging_the_agent_detaches_the_creators_keys() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let other =
        f.s.agent_fixture(json!({ "name": "Helper" }), None)
            .await
            .unwrap();
    let other = text(&other, "id").to_owned();
    common::relay_fixture::allow_member(&f.s, MEMBER, &other).await;
    let start_helper = action(
        "start",
        &json!({
            "type": "startAgent",
            "agentId": other,
            "prompt": { "type": "formula", "expression": "large" },
            "key": key(),
        }),
    );
    let rule = rule_on(
        &f,
        MEMBER,
        plugin(&f),
        json!([{
            "type": "if",
            "id": "bound",
            "condition": { "type": "conversationExists", "key": key() },
            "then": [steer("steer", &key(), &json!({ "type": "formula", "expression": "large" }))],
            "otherwise": [start_helper],
        }]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, MEMBER, "chat:alice").await.unwrap();

    // Merging the started agent into another detaches the keys of what it moves.
    f.s.merge_agents(
        serde_json::from_value(json!({
            "sourceId": other,
            "targetId": MAIN_AGENT_ID,
            "confirm": true,
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    assert!(bound(&f, MEMBER, "chat:alice").await.is_none());
    let moved = f.s.chat_detail(&chat).await.unwrap();
    assert_eq!(moved["agentId"], MAIN_AGENT_ID);
    let _ = rule;
}

#[tokio::test]
async fn the_conversation_cap_suspends_a_rule_that_starts_too_many() {
    let f = granted().await;
    let rule = rule_on(
        &f,
        OWNER,
        plugin(&f),
        json!([start("start")]),
        json!({ "triggeringsPerHour": 60, "conversationsPerHour": 1 }),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    emit(&f, "second", message("bob", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    assert!(bound(&f, OWNER, "chat:alice").await.is_some());
    assert!(bound(&f, OWNER, "chat:bob").await.is_none());
    let (_, current) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{rule}"),
        Value::Null,
    )
    .await;
    assert_eq!(current["blocks"][0]["kind"], "budgetExhausted", "{current}");
}

#[test]
fn version_33_adds_the_conversation_tables_and_reopens_without_changing_them() {
    let root = tempfile::tempdir().unwrap();
    drop(cairn_installation::store::Store::open(root.path()).unwrap());
    let db = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
    db.execute_batch(
        "DROP TABLE hook_rule_conversations;
         DROP TABLE hook_rule_conversation_values;
         DROP TABLE hook_rule_turn_origins;
         DROP TABLE hook_rule_author_triggerings;
         DROP TABLE hook_rule_conversation_counts;
         PRAGMA user_version=33;",
    )
    .unwrap();
    for _ in 0..2 {
        drop(cairn_installation::store::Store::open(root.path()).unwrap());
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            34
        );
        for table in [
            "hook_rule_conversations",
            "hook_rule_conversation_values",
            "hook_rule_turn_origins",
            "hook_rule_author_triggerings",
            "hook_rule_conversation_counts",
        ] {
            assert_eq!(
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
                0
            );
        }
    }
}
