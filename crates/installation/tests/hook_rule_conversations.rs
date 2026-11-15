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
use common::node_fixture::StoreNodeFixture;
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
                "steps": &steps,
                "limits": limits,
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rule} {steps}");
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

/// A whole turn of a delivered message that ends with a reply, in the order Claude's
/// adapter records it: no turn start.
async fn replied(f: &Fixture, run: &str, message: &str, reply: &str) {
    event(
        f,
        run,
        "chat.user",
        "",
        Some(json!({ "messageId": message })),
    )
    .await;
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
    let reached = chat.clone();
    let writers = f.s.store.read(move |db| {
        let origin: (String, String, Option<String>) = db.0.query_row(
            "SELECT reached_by,reached_rule_id,account_id FROM conversation_writers WHERE chat_id=?",
            [&reached], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let entries = db.0.prepare("SELECT writer,rule_id FROM conversation_entry_writers WHERE chat_id=?")?
            .query_map([&reached], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((origin, entries))
    }).await.unwrap();
    assert_eq!(writers.0, ("rule".to_owned(), rule.clone(), None));
    assert_eq!(writers.1, vec![("rule".to_owned(), rule.clone()); 2]);
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
    let message = person_sends(&f, &chat, "thanks").await;
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
    // Check the key before creating it in the same rule. Independent rules run
    // concurrently, so their creation order cannot determine the first result.
    let mut steps = json!([{
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
    }]);
    steps
        .as_array_mut()
        .unwrap()
        .extend(start_or_steer().as_array().unwrap().iter().cloned());
    let quota = rule_on(&f, OWNER, plugin(&f), steps, limits()).await;
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
fn version_34_adds_the_conversation_tables_and_reopens_without_changing_them() {
    let root = tempfile::tempdir().unwrap();
    drop(cairn_installation::store::Store::open(root.path()).unwrap());
    let db = rusqlite::Connection::open(root.path().join("manager.db")).unwrap();
    db.execute_batch(
        "DROP TABLE hook_rule_conversations;
         DROP TABLE hook_rule_conversation_values;
         DROP TABLE hook_rule_turn_origins;
         DROP TABLE hook_rule_chain_depths;
         DROP TABLE hook_rule_author_triggerings;
         DROP TABLE hook_rule_conversation_counts;
         PRAGMA user_version=34;",
    )
    .unwrap();
    for _ in 0..2 {
        drop(cairn_installation::store::Store::open(root.path()).unwrap());
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            35
        );
        for table in [
            "hook_rule_conversations",
            "hook_rule_conversation_values",
            "hook_rule_turn_origins",
            "hook_rule_chain_depths",
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

/// A person writes in the conversation from the apps.
async fn person_sends(f: &Fixture, chat: &str, text: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let (status, sent) = call(
        f,
        OWNER,
        "POST",
        &format!("/api/chats/{chat}/messages"),
        json!({ "message": { "id": id, "text": text } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    id
}

/// The message a rule's start delivered, as the first entry of the conversation.
async fn first_message(f: &Fixture, chat: &str) -> String {
    let detail = f.s.chat_detail(chat).await.unwrap();
    text(&detail["messages"][0], "id").to_owned()
}

async fn last_message(f: &Fixture, chat: &str) -> String {
    let detail = f.s.chat_detail(chat).await.unwrap();
    text(detail["messages"].as_array().unwrap().last().unwrap(), "id").to_owned()
}

#[tokio::test]
async fn claude_and_codex_turns_report_their_reply_end_and_origin() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), start_or_steer(), limits()).await;
    let reply = rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([
            remember("text", "reply", "text"),
            remember("origin", "origin", "origin.kind"),
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
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;

    // Claude's adapter records the session, the delivered message and the end only.
    let session = json!({ "thread_id": uuid::Uuid::new_v4().to_string() });
    event(&f, &run, "thread.started", "", Some(session)).await;
    replied(&f, &run, &first_message(&f, &chat).await, "Hi.").await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &reply).await["reply"], "Hi.");
    assert_eq!(values(&f, &ended).await["outcome"], "completed");

    // A failed Claude turn ends once the run stops.
    emit(&f, "second", message("alice", "again")).await;
    engine::tick(&f.s).await.unwrap();
    let delivered = json!({ "messageId": last_message(&f, &chat).await });
    event(&f, &run, "chat.user", "", Some(delivered)).await;
    let failure = json!({ "error": { "message": "Boom" } });
    event(&f, &run, "turn.failed", "Boom", Some(failure)).await;
    event(&f, &run, "status", "failed", None).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &ended).await["outcome"], "failed");

    // Codex may notify its turn before Cairn records the message that starts it.
    emit(&f, "third", message("alice", "once more")).await;
    engine::tick(&f.s).await.unwrap();
    event(&f, &run, "turn.started", "", None).await;
    let delivered = json!({ "messageId": last_message(&f, &chat).await });
    event(&f, &run, "chat.user", "", Some(delivered)).await;
    let item = json!({ "item": { "type": "agent_message", "text": "Third." } });
    event(&f, &run, "item.completed", "Third.", Some(item)).await;
    event(&f, &run, "turn.completed", "", None).await;
    engine::tick(&f.s).await.unwrap();
    let remembered = values(&f, &reply).await;
    assert_eq!(remembered["reply"], "Third.");
    assert_eq!(remembered["origin"], "rule");
    assert_eq!(depth_of(&f, &reply).await, [1, 1]);
}

#[tokio::test]
async fn a_question_asked_by_a_turn_that_ends_in_the_same_pass_is_answered() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
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
    let delivered = json!({ "messageId": first_message(&f, &chat).await });
    event(&f, &run, "chat.user", "", Some(delivered)).await;
    // The question tool returns at once and the agent ends its turn.
    let question = "a".repeat(64);
    f.s.question_receive(
        &run,
        json!({
            "id": question,
            "blocking": false,
            "fields": [{ "id": "color", "title": "Which color?", "options": [] }],
        }),
    )
    .await
    .unwrap();
    let item = json!({ "item": { "type": "agent_message", "text": "Waiting for you." } });
    event(&f, &run, "item.completed", "", Some(item)).await;
    event(&f, &run, "turn.completed", "", None).await;
    engine::tick(&f.s).await.unwrap();

    let answered = triggerings(&f, &answering).await;
    assert_eq!(answered.len(), 1, "{answered:?}");
    assert_eq!(answered[0]["status"], "completed", "{answered:?}");
    assert_eq!(answered[0]["steps"][0]["result"]["questionId"], question);
}

#[tokio::test]
async fn installation_and_unattributed_input_stay_in_the_rules_chain() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let reply = rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([remember("text", "reply", "text")]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    let token = fixture::running_conversation(&f.s, &chat).await;
    let item = json!({ "item": { "type": "agent_message", "text": "One." } });
    event(&f, &run, "item.completed", "", Some(item)).await;
    event(&f, &run, "turn.completed", "", None).await;
    engine::tick(&f.s).await.unwrap();

    // The agent writes to its own conversation through `cairn_installation`.
    let sent = f
        .mcp(
            "/mcp-installation",
            &token,
            "send_chat_message",
            json!({
                "chatId": chat,
                "message": { "id": uuid::Uuid::new_v4().to_string(), "text": "Follow up" },
            }),
        )
        .await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    replied(&f, &run, &last_message(&f, &chat).await, "Two.").await;
    engine::tick(&f.s).await.unwrap();

    // An entry Cairn did not attribute stays in the conversation's chain too.
    let unknown = json!({ "messageId": uuid::Uuid::new_v4().to_string() });
    event(&f, &run, "chat.user", "", Some(unknown)).await;
    let item = json!({ "item": { "type": "agent_message", "text": "Three." } });
    event(&f, &run, "item.completed", "", Some(item)).await;
    event(&f, &run, "turn.completed", "", None).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &reply).await["reply"], "Three.");
    assert_eq!(depth_of(&f, &reply).await, [1, 1, 1]);

    // Only a person starts a new chain.
    let message = person_sends(&f, &chat, "thanks").await;
    replied(&f, &run, &message, "Four.").await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(depth_of(&f, &reply).await, [1, 1, 1, 0]);
}

#[tokio::test]
async fn only_the_rules_own_cancellation_lets_it_steer_a_paused_conversation() {
    let f = granted().await;
    let steering = rule_on(
        &f,
        OWNER,
        plugin(&f),
        json!([{
            "type": "if",
            "id": "bound",
            "condition": { "type": "conversationExists", "key": key() },
            "then": [{
                "type": "if",
                "id": "stop",
                "condition": { "type": "formula", "expression": "large == \"stop\"" },
                "then": [action("cancel", &json!({ "type": "cancelConversation", "key": key() }))],
                "otherwise": [steer("steer", &key(), &json!({ "type": "formula", "expression": "large" }))],
            }],
            "otherwise": [start("start")],
        }]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    fixture::running_conversation(&f.s, &chat).await;

    // The rule cancels, then steers its own cancellation away.
    emit(&f, "stop", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    emit(&f, "resume", message("alice", "go on")).await;
    engine::tick(&f.s).await.unwrap();
    let latest = &triggerings(&f, &steering).await[0];
    assert_eq!(latest["status"], "completed", "{latest}");
    assert_eq!(f.s.chat_detail(&chat).await.unwrap()["paused"], false);

    // A person's later pause holds rules back, on the same run.
    set_status(&f, &run, cairn_contracts::RunStatus::Running).await;
    emit(&f, "stop-again", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.s.chat_detail(&chat).await.unwrap()["paused"], true);
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    f.s.chat_pause(&chat, false).await.unwrap();
    f.s.chat_pause(&chat, true).await.unwrap();
    emit(&f, "after-person", message("alice", "still there?")).await;
    engine::tick(&f.s).await.unwrap();
    let latest = &triggerings(&f, &steering).await[0];
    assert_eq!(latest["status"], "failed", "{latest}");
    assert!(text(latest, "error").contains("paused"), "{latest}");

    // So does a person's entry after a rule cancelled.
    f.s.chat_pause(&chat, false).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Running).await;
    emit(&f, "stop-third", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(f.s.chat_detail(&chat).await.unwrap()["paused"], true);
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    person_sends(&f, &chat, "I take it from here").await;
    emit(&f, "after-entry", message("alice", "hello?")).await;
    engine::tick(&f.s).await.unwrap();
    let latest = &triggerings(&f, &steering).await[0];
    assert_eq!(latest["status"], "failed", "{latest}");

    // A failed run gives rules no way around a person's pause either.
    f.s.chat_pause(&chat, false).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Failed).await;
    f.s.chat_pause(&chat, true).await.unwrap();
    emit(&f, "after-failure", message("alice", "anyone?")).await;
    engine::tick(&f.s).await.unwrap();
    let latest = &triggerings(&f, &steering).await[0];
    assert_eq!(latest["status"], "failed", "{latest}");
    assert!(text(latest, "error").contains("paused"), "{latest}");
}

async fn set_status(f: &Fixture, run: &str, status: cairn_contracts::RunStatus) {
    f.s.store
        .change_run(
            run,
            cairn_installation::runs::RunChanges {
                status: Some(status),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

/// A native node where the owner may or may not run natively.
async fn native_node(f: &Fixture, accounts: &Value) -> Value {
    let node = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "name": "Shared desktop",
        "local": false,
        "accepting": true,
        "revoked": false,
        "nativeEnabled": true,
        "nativeAccounts": accounts,
        "lastSeen": cairn_installation::config::now(),
        "executionReady": true,
        "runtimeId": "development",
        "nativeModeObserved": { "mode": "protected", "generation": null, "status": "applied" },
        "capabilities": {
            "nativeModeProtocol": 1,
            "backend": "native",
            "ublk": true,
            "os": "linux",
            "arch": "x86_64",
            "cpu": 4,
            "memoryMiB": 8192,
            "diskMiB": 100000,
            "diskTotalMiB": 100000,
        },
    });
    f.s.store.put_node_fixture(node.clone()).await.unwrap();
    f.s.store
        .set(
            "native-authority",
            json!({
                "observedAt": cairn_installation::config::now(),
                "accounts": { OWNER: { "email": "owner@example.test", "accessId": OWNER } },
            }),
            None,
        )
        .await
        .unwrap();
    node
}

fn start_or_steer_agent(agent: &str) -> Value {
    json!([{
        "type": "if",
        "id": "bound",
        "condition": { "type": "conversationExists", "key": key() },
        "then": [steer("steer", &key(), &json!({ "type": "formula", "expression": "large" }))],
        "otherwise": [action("start", &json!({
            "type": "startAgent",
            "agentId": agent,
            "prompt": { "type": "formula", "expression": "large" },
            "key": key(),
        }))],
    }])
}

#[tokio::test]
async fn native_conversations_need_the_versions_and_the_creators_native_authority() {
    let f = granted().await;
    let mut node = native_node(&f, &json!([])).await;
    let agent =
        f.s.agent_fixture(
            json!({ "name": "Native", "access": { "backend": "native", "nodes": [node["id"]] } }),
            None,
        )
        .await
        .unwrap();
    let agent = text(&agent, "id").to_owned();
    // Saved while its creator held no native authority.
    let unauthorized = rule_on(
        &f,
        OWNER,
        plugin(&f),
        json!([{
            "type": "if",
            "id": "bound",
            "condition": { "type": "conversationExists", "key": key() },
            "then": [action("note", &json!({
                "type": "storeValue", "scope": "conversation", "key": key(), "name": "seen",
                "value": { "type": "literal", "value": true },
            }))],
            "otherwise": [],
        }]),
        limits(),
    )
    .await;
    node["nativeAccounts"] = json!([OWNER]);
    f.s.store.put_node_fixture(node.clone()).await.unwrap();
    let authorized = rule_on(
        &f,
        OWNER,
        plugin(&f),
        start_or_steer_agent(&agent),
        limits(),
    )
    .await;

    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice")
        .await
        .expect("started natively");
    let stored = f.s.conversation_boundary(&chat).await.unwrap();
    assert_eq!(
        stored["nativeAuthorityNodes"],
        json!([node["id"]]),
        "{stored}"
    );
    assert_eq!(stored["nativeProvenanceUnknown"], false);

    emit(&f, "second", message("alice", "again")).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &authorized).await[0]["status"], "completed");
    let refused = &triggerings(&f, &unauthorized).await[0];
    assert_eq!(refused["status"], "failed", "{refused}");
    assert!(
        text(refused, "error").contains("saved without native authority"),
        "{refused}"
    );

    // Its creator no longer holds native authority on the node.
    node["nativeAccounts"] = json!([]);
    f.s.store.put_node_fixture(node.clone()).await.unwrap();
    emit(&f, "third", message("alice", "still?")).await;
    emit(&f, "other", message("bob", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let history = triggerings(&f, &authorized).await;
    for refused in &history[..2] {
        assert_eq!(refused["status"], "failed", "{refused}");
        assert!(
            text(refused, "error").contains("native authority"),
            "{refused}"
        );
    }
    assert!(bound(&f, OWNER, "chat:bob").await.is_none());
}

#[tokio::test]
async fn an_automatic_agent_placed_natively_needs_native_authority_to_be_reached() {
    let f = granted().await;
    let mut node = native_node(&f, &json!([OWNER])).await;
    let agent =
        f.s.agent_fixture(
            json!({ "name": "Automatic", "access": { "backend": "auto", "nodes": [node["id"]] } }),
            None,
        )
        .await
        .unwrap();
    let agent = text(&agent, "id").to_owned();
    let rule = rule_on(
        &f,
        OWNER,
        plugin(&f),
        start_or_steer_agent(&agent),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    // The run was placed on the native node.
    let mut snapshot = f.s.store.run(&run).await.unwrap()["snapshot"].clone();
    snapshot["agent"]["access"]["backend"] = "native".into();
    f.s.store
        .change_run(
            &run,
            cairn_installation::runs::RunChanges {
                snapshot: Some(serde_json::from_value(snapshot).unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    node["nativeAccounts"] = json!([]);
    f.s.store.put_node_fixture(node).await.unwrap();
    emit(&f, "second", message("alice", "again")).await;
    engine::tick(&f.s).await.unwrap();
    let refused = &triggerings(&f, &rule).await[0];
    assert_eq!(refused["status"], "failed", "{refused}");
    assert!(text(refused, "error").contains("native"), "{refused}");
}

#[tokio::test]
async fn losing_access_to_the_conversations_agent_stops_its_actions_and_hooks() {
    let f = granted().await;
    common::relay_fixture::allow_member(&f.s, MEMBER, MAIN_AGENT_ID).await;
    let helper =
        f.s.agent_fixture(json!({ "name": "Helper" }), None)
            .await
            .unwrap();
    let helper = text(&helper, "id").to_owned();
    common::relay_fixture::allow_member(&f.s, MEMBER, &helper).await;
    let when_acting = |id: &str, act: Value| {
        json!([{
            "type": "if",
            "id": format!("{id}_when"),
            "condition": { "type": "formula", "expression": "large == \"act\"" },
            "then": [action(id, &act)],
            "otherwise": [],
        }])
    };
    rule_on(
        &f,
        MEMBER,
        plugin(&f),
        json!([{
            "type": "if",
            "id": "bound",
            "condition": { "type": "conversationExists", "key": key() },
            "then": [],
            "otherwise": [action("start", &json!({
                "type": "startAgent",
                "agentId": helper,
                "prompt": { "type": "formula", "expression": "large" },
                "key": key(),
            }))],
        }]),
        limits(),
    )
    .await;
    let mut acting = Vec::new();
    for (id, act) in [
        (
            "steer",
            json!({ "type": "steerConversation", "key": key(), "message": { "type": "literal", "value": "hi" } }),
        ),
        (
            "cancel",
            json!({ "type": "cancelConversation", "key": key() }),
        ),
        (
            "answer",
            json!({ "type": "answerQuestion", "key": key(), "answer": { "type": "literal", "value": "Blue" } }),
        ),
        (
            "store",
            json!({
                "type": "storeValue", "scope": "conversation", "key": key(), "name": "seen",
                "value": { "type": "literal", "value": true },
            }),
        ),
    ] {
        acting.push(rule_on(&f, MEMBER, plugin(&f), when_acting(id, act), limits()).await);
    }
    let reply = rule_on(
        &f,
        MEMBER,
        cairn("replyReady"),
        json!([remember("text", "reply", "text")]),
        limits(),
    )
    .await;

    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, MEMBER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    replied(&f, &run, &first_message(&f, &chat).await, "One.").await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &reply).await.len(), 1);

    let (agent, account) = (helper.clone(), MEMBER.to_owned());
    f.s.store
        .transaction(move |db| {
            db.0.execute(
                "DELETE FROM agent_account_access WHERE agent_id=?1 AND account_id=?2",
                [agent, account],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    emit(&f, "act", message("alice", "act")).await;
    engine::tick(&f.s).await.unwrap();
    for rule in &acting {
        let refused = &triggerings(&f, rule).await[0];
        assert_eq!(refused["status"], "failed", "{refused}");
        assert!(
            text(refused, "error").contains("no longer has access"),
            "{refused}"
        );
    }

    // Its hooks no longer reach the creator's rules either.
    f.s.chat_send(
        &chat,
        json!({ "id": uuid::Uuid::new_v4().to_string(), "text": "still there?" }),
    )
    .await
    .unwrap();
    replied(&f, &run, &last_message(&f, &chat).await, "Two.").await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &reply).await.len(), 1);
}

#[tokio::test]
async fn a_progress_triggering_does_not_act_once_its_rule_is_exhausted() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let progress = rule_on(
        &f,
        OWNER,
        cairn("progress"),
        json!([remember("step", "step", "step")]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    let delivered = json!({ "messageId": first_message(&f, &chat).await });
    event(&f, &run, "chat.user", "", Some(delivered)).await;
    event(&f, &run, "turn.started", "", None).await;
    engine::tick(&f.s).await.unwrap();
    // The triggering is admitted, then the rule's budget runs out before it acts.
    let chat_id = chat.clone();
    f.s.store
        .transaction(move |db| {
            age_progress(db, &chat_id)?;
            db.0.execute(
                "CREATE TEMP TRIGGER exhaust AFTER INSERT ON hook_rule_triggerings \
                 BEGIN UPDATE hook_rules SET budget_exhausted_at=1 WHERE id=NEW.rule_id; END",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    engine::tick(&f.s).await.unwrap();
    // Refused before its first action, it leaves no history and remembers nothing.
    let history = triggerings(&f, &progress).await;
    assert!(
        history.iter().all(|t| t["status"] != "completed"),
        "{history:?}"
    );
    assert!(values(&f, &progress).await.get("step").is_none());
}

#[tokio::test]
async fn cairn_hooks_keep_their_cadence_during_a_continuous_flow_of_completions() {
    use std::sync::atomic::Ordering;
    let f = granted().await;
    // Alice's messages post and wait for the test; Bob's start a keyed conversation.
    rule_on(
        &f,
        OWNER,
        plugin(&f),
        json!([{
            "type": "if",
            "id": "who",
            "condition": { "type": "formula", "expression": "author == \"alice\"" },
            "then": [action("post", &json!({
                "type": "callTool", "instanceId": f.instance, "tool": "grant_probe_post",
                "input": { "channel": { "type": "literal", "value": "support" } },
            }))],
            "otherwise": [start("start")],
        }]),
        json!({ "triggeringsPerHour": 200, "conversationsPerHour": 10 }),
    )
    .await;
    let reply = rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([remember("text", "reply", "text")]),
        limits(),
    )
    .await;
    let progress = rule_on(
        &f,
        OWNER,
        cairn("progress"),
        json!([remember("step", "step", "step")]),
        limits(),
    )
    .await;
    emit(&f, "bob", message("bob", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:bob").await.unwrap();
    let run = launched(&f, &chat).await;
    let delivered = json!({ "messageId": first_message(&f, &chat).await });
    event(&f, &run, "chat.user", "", Some(delivered)).await;
    engine::tick(&f.s).await.unwrap();

    f.hold_posts.store(true, Ordering::SeqCst);
    let flow = 20;
    for index in 0..flow {
        emit(&f, &format!("flow-{index}"), message("alice", "post")).await;
    }
    let service = f.s.clone();
    let worker = tokio::spawn(async move { engine::tick(&service).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), f.entered.notified())
        .await
        .unwrap();
    // The turn is due a progress report, then completes, while the flow continues.
    f.s.store
        .transaction({
            let chat = chat.clone();
            move |db| age_progress(db, &chat)
        })
        .await
        .unwrap();
    let mut completed = false;
    let mut released = 0;
    while released < flow {
        let remembered_progress = values(&f, &progress).await.get("step").is_some();
        if remembered_progress && !completed {
            let item = json!({ "item": { "type": "agent_message", "text": "Done." } });
            event(&f, &run, "item.completed", "", Some(item)).await;
            event(&f, &run, "turn.completed", "", None).await;
            completed = true;
        }
        if completed && values(&f, &reply).await.get("reply").is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        f.release.notify_one();
        released += 1;
    }
    let reported_during_flow = released < flow;
    f.hold_posts.store(false, Ordering::SeqCst);
    while f.posts.load(Ordering::SeqCst) > released {
        f.release.notify_one();
        released += 1;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    worker.await.unwrap().unwrap();
    assert!(
        reported_during_flow,
        "Completions every 300 ms held back the progress or the reply of a keyed turn."
    );
    assert_eq!(values(&f, &reply).await["reply"], "Done.");
}

/// Starts Bob's conversation, then cancels or steers it on Alice's messages: "stop"
/// cancels, anything else steers.
async fn cancel_or_steer(f: &Fixture) -> String {
    rule_on(
        f,
        OWNER,
        plugin(f),
        json!([{
            "type": "if",
            "id": "bound",
            "condition": { "type": "conversationExists", "key": { "type": "literal", "value": "chat:bob" } },
            "then": [{
                "type": "if",
                "id": "stop",
                "condition": { "type": "formula", "expression": "large == \"stop\"" },
                "then": [action("cancel", &json!({
                    "type": "cancelConversation",
                    "key": { "type": "literal", "value": "chat:bob" },
                }))],
                "otherwise": [steer(
                    "steer",
                    &json!({ "type": "literal", "value": "chat:bob" }),
                    &json!({ "type": "formula", "expression": "large" }),
                )],
            }],
            "otherwise": [start("start")],
        }]),
        limits(),
    )
    .await
}

#[tokio::test]
async fn a_rules_cancellation_never_takes_over_a_persons_pause_or_stop() {
    let f = granted().await;
    let rule = cancel_or_steer(&f).await;
    emit(&f, "first", message("bob", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:bob").await.unwrap();
    let run = launched(&f, &chat).await;
    fixture::running_conversation(&f.s, &chat).await;

    // A person pauses while the turn works; a rule then stops the turn.
    f.s.chat_pause(&chat, true).await.unwrap();
    emit(&f, "stop", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &rule).await[0]["status"], "completed");
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    emit(&f, "steer", message("alice", "go on")).await;
    engine::tick(&f.s).await.unwrap();
    let refused = &triggerings(&f, &rule).await[0];
    assert_eq!(refused["status"], "failed", "{refused}");
    assert!(text(refused, "error").contains("paused"), "{refused}");
    assert_eq!(f.s.chat_detail(&chat).await.unwrap()["paused"], true);

    // A person's Stop still stopping the turn when a rule cancels it stays theirs.
    f.s.chat_pause(&chat, false).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Running).await;
    let (status, stopped) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/chats/{chat}/stop"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    emit(&f, "stop-again", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    emit(&f, "steer-again", message("alice", "go on")).await;
    engine::tick(&f.s).await.unwrap();
    let refused = &triggerings(&f, &rule).await[0];
    assert_eq!(refused["status"], "failed", "{refused}");
    assert_eq!(f.s.chat_detail(&chat).await.unwrap()["paused"], true);

    // The rules' own pause stays theirs when a rule cancels it again while it stops.
    f.s.chat_pause(&chat, false).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Running).await;
    emit(&f, "own-stop", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    emit(&f, "own-stop-again", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    emit(&f, "own-steer", message("alice", "go on")).await;
    engine::tick(&f.s).await.unwrap();
    let steered = &triggerings(&f, &rule).await[0];
    assert_eq!(steered["status"], "completed", "{steered}");
    assert_eq!(f.s.chat_detail(&chat).await.unwrap()["paused"], false);
}

#[tokio::test]
async fn a_turn_whose_run_stops_without_reporting_it_ends_as_its_run() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let ended = rule_on(
        &f,
        OWNER,
        cairn("ended"),
        json!([
            remember("outcome", "outcome", "outcome"),
            remember(
                "error",
                "error",
                "has(payload.error) ? payload.error : \"\""
            ),
        ]),
        limits(),
    )
    .await;
    let progress = rule_on(
        &f,
        OWNER,
        cairn("progress"),
        json!([remember("step", "step", "step")]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;

    // A wakeup opens a turn on a queued run, which is cancelled before it starts:
    // nothing records a status event.
    let wakeup = json!({ "at": cairn_installation::config::now() });
    event(
        &f,
        &run,
        "chat.wakeup",
        "Resuming after scheduled wait",
        Some(wakeup),
    )
    .await;
    set_status(&f, &run, cairn_contracts::RunStatus::Queued).await;
    engine::tick(&f.s).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &ended).await["outcome"], "cancelled");
    // A run that no longer works reports no progress.
    for _ in 0..2 {
        f.s.store
            .transaction({
                let chat = chat.clone();
                move |db| age_progress(db, &chat)
            })
            .await
            .unwrap();
        engine::tick(&f.s).await.unwrap();
    }
    assert!(triggerings(&f, &progress).await.is_empty());

    // A run that fails without a failed turn ends it with the run's error.
    let delivered = json!({ "messageId": first_message(&f, &chat).await });
    event(&f, &run, "chat.user", "", Some(delivered)).await;
    f.s.store
        .change_run(
            &run,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Failed),
                error: Some(Some("The agent could not start.".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    engine::tick(&f.s).await.unwrap();
    let remembered = values(&f, &ended).await;
    assert_eq!(remembered["outcome"], "failed");
    assert_eq!(remembered["error"], "The agent could not start.");
    assert_eq!(triggerings(&f, &ended).await.len(), 2);
}

#[tokio::test]
async fn a_status_only_failure_reports_its_error() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let ended = rule_on(
        &f,
        OWNER,
        cairn("ended"),
        json!([
            remember("outcome", "outcome", "outcome"),
            remember("error", "error", "payload.error"),
        ]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    let delivered = json!({ "messageId": first_message(&f, &chat).await });
    event(&f, &run, "chat.user", "", Some(delivered)).await;
    // The worker fails a run over its time limit with only a status event.
    let details = json!({ "error": { "message": "Run exceeded its time limit." } });
    set_status(&f, &run, cairn_contracts::RunStatus::Failed).await;
    event(&f, &run, "status", "failed", Some(details)).await;
    engine::tick(&f.s).await.unwrap();
    let remembered = values(&f, &ended).await;
    assert_eq!(remembered["outcome"], "failed");
    assert_eq!(remembered["error"], "Run exceeded its time limit.");
}

#[tokio::test]
async fn a_cairn_hook_over_the_recorded_data_budget_is_counted_as_not_admitted() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let reply = rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([remember("text", "reply", "truncated")]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    // Remembered values at their limit, each escaping to six bytes per character.
    let escaped = "\u{1}".repeat(10_000);
    let stored = chat.clone();
    f.s.store
        .transaction(move |db| {
            for name in ["a", "b", "c", "d"] {
                db.0.execute(
                    "INSERT INTO hook_rule_conversation_values VALUES(?1,?2,?3,0)",
                    rusqlite::params![stored, name, Value::String(escaped.clone()).to_string()],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let run = launched(&f, &chat).await;
    // The longest reply kept, made of characters JSON escapes.
    let long = "\u{1}".repeat(128 * 1024);
    replied(&f, &run, &first_message(&f, &chat).await, &long).await;
    engine::tick(&f.s).await.unwrap();

    let (status, listed) = call(
        &f,
        OWNER,
        "GET",
        &format!("/api/hook-rules/{reply}/triggerings"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["triggerings"], json!([]), "{listed}");
    assert_eq!(listed["notAdmitted"], 1, "{listed}");
}

#[tokio::test]
async fn new_native_nodes_cannot_expand_a_saved_rules_conversation_grant() {
    let f = granted().await;
    let first = native_node(&f, &json!([OWNER])).await;
    let mut later = native_node(&f, &json!([])).await;
    let agent =
        f.s.agent_fixture(
            json!({
                "name": "Native ceiling",
                "access": { "backend": "native", "nodes": [first["id"], later["id"]] },
            }),
            None,
        )
        .await
        .unwrap();
    let rule = rule_on(
        &f,
        OWNER,
        plugin(&f),
        start_or_steer_agent(text(&agent, "id")),
        limits(),
    )
    .await;
    later["nativeAccounts"] = json!([OWNER]);
    f.s.store.put_node_fixture(later).await.unwrap();
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let saved = f.s.conversation_boundary(&chat).await.unwrap();
    assert_eq!(
        saved["nativeAuthorityNodes"],
        json!([first["id"]]),
        "{saved}"
    );
    emit(&f, "second", message("alice", "again")).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &rule).await[0]["status"], "completed");
    let saved = f.s.conversation_boundary(&chat).await.unwrap();
    assert_eq!(
        saved["nativeAuthorityNodes"],
        json!([first["id"]]),
        "{saved}"
    );
}

#[tokio::test]
async fn queued_agent_input_cannot_lower_an_unobserved_rules_chain() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    let token = fixture::running_conversation(&f.s, &chat).await;
    let looping = rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([steer(
            "again",
            &json!({ "type": "formula", "expression": "key" }),
            &json!({ "type": "literal", "value": "again" }),
        )]),
        json!({ "triggeringsPerHour": 600, "conversationsPerHour": 100 }),
    )
    .await;
    for turn in 1..MAX_CHAIN_DEPTH {
        replied(
            &f,
            &run,
            &last_message(&f, &chat).await,
            &format!("turn {turn}"),
        )
        .await;
        engine::tick(&f.s).await.unwrap();
    }
    let next = uuid::Uuid::new_v4().to_string();
    let sent = f
        .mcp(
            "/mcp-installation",
            &token,
            "send_chat_message",
            json!({
                "chatId": chat,
                "message": { "id": next, "text": "Queued before the watcher catches up" },
            }),
        )
        .await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    let checked = next.clone();
    let depth: u32 =
        f.s.store
            .read(move |db| {
                Ok(db.0.query_row(
                    "SELECT depth FROM hook_rule_turn_origins WHERE message_id=?",
                    [checked],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(u64::from(depth), MAX_CHAIN_DEPTH);
    // The same ceiling crosses an installation-created conversation with no
    // rule key and hence no hook watcher of its own.
    let created = f
        .mcp(
            "/mcp-installation",
            &token,
            "create_chat",
            json!({ "agentId": MAIN_AGENT_ID }),
        )
        .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    let child = text(&created["result"]["structuredContent"]["result"], "id").to_owned();
    let sent = f.mcp("/mcp-installation", &token, "send_chat_message", json!({
        "chatId": child,
        "message": { "id": uuid::Uuid::new_v4().to_string(), "text": "Continue this chain" },
    })).await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    let child_token = fixture::running_conversation(&f.s, &child).await;
    let continued = uuid::Uuid::new_v4().to_string();
    let sent = f
        .mcp(
            "/mcp-installation",
            &child_token,
            "send_chat_message",
            json!({
                "chatId": child,
                "message": { "id": continued, "text": "Continue inside the unkeyed conversation" },
            }),
        )
        .await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    let checked = continued;
    f.s.store
        .read(move |db| {
            let depth: u32 = db.0.query_row(
                "SELECT depth FROM hook_rule_turn_origins WHERE message_id=?",
                [checked],
                |row| row.get(0),
            )?;
            assert_eq!(u64::from(depth), MAX_CHAIN_DEPTH);
            Ok(())
        })
        .await
        .unwrap();
    replied(&f, &run, &next, "Another turn").await;
    engine::tick(&f.s).await.unwrap();
    let history = triggerings(&f, &looping).await;
    assert_eq!(history[0]["status"], "failed", "{:?}", history[0]);
    assert!(text(&history[0], "error").contains("maximum depth"));
}

#[tokio::test]
async fn a_public_members_message_resets_the_chain_and_takes_back_the_pause() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    let checked = chat.clone();
    f.s.store.transaction(move |db| {
        db.0.execute("UPDATE hook_rule_conversations SET watch=json_set(watch,'$.depth',8),cancelled_run=?2 WHERE chat_id=?1", rusqlite::params![checked,run])?;
        Ok(())
    }).await.unwrap();
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
    let message = uuid::Uuid::new_v4().to_string();
    let sent = f
        .mcp(
            "/mcp",
            &token,
            "send_chat_message",
            json!({
                "chatId": chat,
                "message": { "id": message, "text": "A person takes over" },
            }),
        )
        .await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    f.s.store
        .read(move |db| {
            let (depth, origin): (u32, String) = db.0.query_row(
                "SELECT depth,origin FROM hook_rule_turn_origins WHERE message_id=?",
                [message],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(depth, 0);
            let origin: Value = serde_json::from_str(&origin)?;
            assert_eq!(origin["kind"], "person");
            assert_eq!(origin["accountId"], MEMBER);
            let cancelled: Option<String> = db.0.query_row(
                "SELECT cancelled_run FROM hook_rule_conversations WHERE chat_id=?",
                [chat],
                |row| row.get(0),
            )?;
            assert_eq!(cancelled, None);
            Ok(())
        })
        .await
        .unwrap();
}
