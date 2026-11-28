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

fn start_or_cancel() -> Value {
    json!([{
        "type": "if", "id": "bound",
        "condition": { "type": "conversationExists", "key": key() },
        "then": [action("stop", &json!({ "type": "cancelConversation", "key": key() }))],
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
    if kind == "chat.user"
        && let Some(message) = payload.as_ref().and_then(|p| p["messageId"].as_str())
    {
        let (checked_run, checked_message) = (run.to_owned(), message.to_owned());
        let recorded: bool = f.s.store.read(move |db| Ok(db.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages m JOIN conversations c ON c.id=m.chat_id WHERE c.run_id=?1 AND m.id=?2)",
            rusqlite::params![checked_run,checked_message], |row|row.get(0),
        )?)).await.unwrap();
        if recorded {
            // Real controller delivery records chat.user together with the
            // active causal frame; retain raw events only for unknown input.
            f.s.chat_acknowledge(run, message).await.unwrap();
            return;
        }
    }
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
    let canceller = rule_on(&f, OWNER, plugin(&f), start_or_cancel(), limits()).await;
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
    let checked = run.clone();
    let marker: u32 = f.s.store.read(move |db| Ok(db.0.query_row(
        "SELECT json_extract(payload,'$.hookRuleDepth') FROM events WHERE run_id=? AND type='status' AND json_type(payload,'$.hookRuleDepth')='integer' ORDER BY id DESC LIMIT 1",
        [checked], |row|row.get(0),
    )?)).await.unwrap();
    assert_eq!(marker, 1);

    event(&f, &run, "status", "cancelled", None).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &ended).await["outcome"], "cancelled");
    // The end belongs to the cancelling rule's chain.
    assert_eq!(depth_of(&f, &ended).await, [1]);
}

#[tokio::test]
async fn cancelling_by_key_stops_a_held_environment_after_the_turn_finished() {
    let f = granted().await;
    let canceller = rule_on(&f, OWNER, plugin(&f), start_or_cancel(), limits()).await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    fixture::running_conversation(&f.s, &chat).await;
    set_status(&f, &run, cairn_contracts::RunStatus::Succeeded).await;
    f.s.store
        .save_checkpoint_boundary(
            &run,
            json!({
                "completed": true,
                "launched": true,
                "heldEnvironment": {
                    "attempt": "held-attempt",
                    "generation": "held-generation",
                    "expiresAt": cairn_installation::config::now() + 60_000,
                },
            }),
        )
        .await
        .unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    f.s.worker
        .active
        .lock()
        .await
        .insert(run.clone(), cancellation.clone());

    emit(&f, "stop", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();

    let stopped = &triggerings(&f, &canceller).await[0];
    assert_eq!(
        stopped["steps"][0]["result"]["cancelled"], true,
        "{stopped}"
    );
    assert_eq!(stopped["steps"][0]["result"]["runId"], run, "{stopped}");
    let state = f.s.store.run(&run).await.unwrap();
    assert!(state["cancelRequestedAt"].is_number(), "{state}");
    assert!(cancellation.is_cancelled());
    f.s.worker.active.lock().await.remove(&run);
}

#[tokio::test]
async fn cancelling_a_retry_keeps_the_cancelling_hooks_chain_depth() {
    use common::run_fixture::RunFixture;

    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let ended = rule_on(
        &f,
        OWNER,
        cairn("ended"),
        json!([remember("end", "end", "outcome")]),
        limits(),
    )
    .await;
    let canceller = rule_on(
        &f,
        OWNER,
        cairn("progress"),
        json!([action(
            "stop",
            &json!({
                "type": "cancelConversation",
                "key": { "type": "formula", "expression": "key" },
            })
        )]),
        limits(),
    )
    .await;
    emit(&f, "start", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    event(
        &f,
        &run,
        "chat.user",
        "",
        Some(json!({ "messageId": first_message(&f, &chat).await })),
    )
    .await;
    event(&f, &run, "turn.started", "", None).await;
    engine::tick(&f.s).await.unwrap();

    // A retry retains this depth-1 turn. Cancelling its queued retry writes a
    // terminal status synchronously, before any worker can observe the stop.
    f.s.store
        .change_run_fixture(
            &run,
            json!({
                "status": "queued",
                "retry": {
                    "attempt": 1, "limit": 5,
                    "nextAttemptAt": cairn_installation::config::now() + 60_000,
                    "cause": { "kind": "connection", "message": "Temporary disconnect" },
                },
            }),
        )
        .await
        .unwrap();
    f.s.store
        .transaction(move |db| age_progress(db, &chat))
        .await
        .unwrap();
    engine::tick(&f.s).await.unwrap();
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &canceller).await[0]["status"], "completed");
    assert_eq!(depth_of(&f, &canceller).await, [1]);
    assert_eq!(values(&f, &ended).await["end"], "cancelled");
    assert_eq!(depth_of(&f, &ended).await, [2]);
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
async fn a_rule_cannot_resume_even_its_own_cancellation_without_a_person() {
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

    // Even the rule's own cancellation requires a person's resumption.
    emit(&f, "stop", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    emit(&f, "resume", message("alice", "go on")).await;
    engine::tick(&f.s).await.unwrap();
    let latest = &triggerings(&f, &steering).await[0];
    assert_eq!(latest["status"], "failed", "{latest}");
    assert!(text(latest, "error").contains("paused"), "{latest}");
    assert_eq!(f.s.chat_detail(&chat).await.unwrap()["paused"], true);
    f.s.chat_pause(&chat, false).await.unwrap();
    emit(&f, "person-resumed", message("alice", "go on")).await;
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

    // A failed turn no longer pauses the conversation, yet rules still wait for a person.
    f.s.chat_pause(&chat, false).await.unwrap();
    emit(&f, "failed-unpaused", message("alice", "still there?")).await;
    engine::tick(&f.s).await.unwrap();
    let latest = &triggerings(&f, &steering).await[0];
    assert_eq!(latest["status"], "failed", "{latest}");
    assert!(
        text(latest, "error").contains("last turn failed"),
        "{latest}"
    );
}

async fn set_status(f: &Fixture, run: &str, status: cairn_contracts::RunStatus) {
    f.s.store
        .change_run(
            run,
            cairn_installation::runs::RunChanges {
                status: Some(status),
                recovery_pending: Some(false),
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

    // Repeated cancellation still needs a person to resume it.
    f.s.chat_pause(&chat, false).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Running).await;
    emit(&f, "own-stop", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    emit(&f, "own-stop-again", message("alice", "stop")).await;
    engine::tick(&f.s).await.unwrap();
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    emit(&f, "own-steer", message("alice", "go on")).await;
    engine::tick(&f.s).await.unwrap();
    let refused = &triggerings(&f, &rule).await[0];
    assert_eq!(refused["status"], "failed", "{refused}");
    assert_eq!(f.s.chat_detail(&chat).await.unwrap()["paused"], true);
    f.s.chat_pause(&chat, false).await.unwrap();
    emit(&f, "person-resumed", message("alice", "go on")).await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &rule).await[0]["status"], "completed");
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
async fn terminal_status_replaces_the_last_attempts_transient_failure() {
    use common::run_fixture::RunFixture;

    let (f, ended, _, run) = retrying_conversation().await;
    event(&f, &run, "turn.failed", "Still disconnected", None).await;
    let final_error = "Run exceeded its time limit.";
    f.s.store
        .change_run_fixture(
            &run,
            json!({
                "status": "failed", "error": final_error, "recoveryPending": false,
            }),
        )
        .await
        .unwrap();
    event(
        &f,
        &run,
        "status",
        "failed",
        Some(json!({
            "error": { "message": final_error },
        })),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &ended).await["error"], final_error);
}

#[tokio::test]
async fn terminal_run_state_replaces_the_last_attempts_transient_failure() {
    use common::run_fixture::RunFixture;

    let (f, ended, _, run) = retrying_conversation().await;
    event(&f, &run, "turn.failed", "Still disconnected", None).await;
    let final_error = "Working files were preserved because finalization failed.";
    f.s.store
        .change_run_fixture(
            &run,
            json!({
                "status": "failed", "error": final_error, "recoveryPending": false,
            }),
        )
        .await
        .unwrap();
    // Recovery can settle the run without a terminal provider/status event.
    engine::tick(&f.s).await.unwrap();
    assert_eq!(values(&f, &ended).await["error"], final_error);
}

async fn retrying_conversation() -> (Fixture, String, String, String) {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let ended = rule_on(
        &f,
        OWNER,
        cairn("ended"),
        json!([remember(
            "error",
            "error",
            "has(payload.error) ? payload.error : \"\""
        )]),
        limits(),
    )
    .await;
    emit(&f, "start", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    event(
        &f,
        &run,
        "chat.user",
        "",
        Some(json!({ "messageId": first_message(&f, &chat).await })),
    )
    .await;
    event(&f, &run, "turn.started", "", None).await;
    event(&f, &run, "turn.failed", "Temporary disconnect", None).await;
    event(
        &f,
        &run,
        "retry.scheduled",
        "Recovering the connection",
        None,
    )
    .await;
    (f, ended, chat, run)
}

#[tokio::test]
async fn expired_recovery_keeps_its_own_error_after_a_later_resume() {
    use common::run_fixture::RunFixture;

    let (f, ended, chat, run) = retrying_conversation().await;
    let now = cairn_installation::config::now();
    f.s.store.change_run_fixture(&run, json!({
        "status": "queued", "recoveryPending": false,
        "retry": {
            "attempt": 1, "limit": 5, "nextAttemptAt": now - 1,
            "startedAt": now - 600_001, "deadlineAt": now - 1,
            "cause": { "kind": "connection", "message": "Temporary disconnect", "startup": true },
        },
    })).await.unwrap();
    // Exercise the worker's real expiry producer; it must not launch an adapter.
    f.s.worker.tick(&f.s).await.unwrap();
    let failed = f.s.store.run(&run).await.unwrap();
    assert_eq!(failed["status"], "failed");
    let expired_error = text(&failed, "error").to_owned();
    assert!(expired_error.contains("10-minute limit"), "{failed}");

    // Reuse the run for a later attempt before the hook observer reads either
    // boundary. Its current error must never replace the historical expiry.
    let token = common::relay_fixture::grant(&f.s, &["read", "write", "run", "manage"]).await;
    let resumed = f
        .mcp("/mcp", &token, "resume_chat", json!({ "chatId": chat }))
        .await;
    assert_ne!(resumed["result"]["isError"], true, "{resumed}");
    let later_error = "A later attempt could not authenticate.";
    f.s.store
        .change_run_fixture(
            &run,
            json!({
                "status": "failed", "error": later_error, "recoveryPending": false,
            }),
        )
        .await
        .unwrap();
    event(
        &f,
        &run,
        "status",
        "failed",
        Some(json!({
            "error": { "message": later_error },
        })),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    let history = triggerings(&f, &ended).await;
    assert_eq!(history.len(), 2, "{history:?}");
    let original = history
        .iter()
        .find(|entry| entry["event"]["payload"]["origin"]["kind"] == "rule")
        .unwrap();
    assert_eq!(original["event"]["payload"]["error"], expired_error);
    let resumed = history
        .iter()
        .find(|entry| entry["event"]["payload"]["origin"]["kind"] == "resume")
        .unwrap();
    assert_eq!(resumed["event"]["payload"]["error"], later_error);
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
    fixture::running_conversation(&f.s, &chat).await;
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
    set_status(&f, &run, cairn_contracts::RunStatus::Succeeded).await;
    let token = fixture::running_conversation(&f.s, &chat).await;
    let run = text(&f.s.chat_detail(&chat).await.unwrap()["run"], "id").to_owned();
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
async fn a_public_members_message_resets_the_chain() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    launched(&f, &chat).await;
    let checked = chat.clone();
    f.s.store.transaction(move |db| {
        db.0.execute("UPDATE hook_rule_conversations SET watch=json_set(watch,'$.depth',8) WHERE chat_id=?1", rusqlite::params![checked])?;
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
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn manual_compaction_does_not_emit_reply_or_end_hooks_even_when_observed_late() {
    for observe_during in [false, true] {
        let f = granted().await;
        rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
        let reply = rule_on(
            &f,
            OWNER,
            cairn("replyReady"),
            json!([remember("reply", "reply", "text")]),
            limits(),
        )
        .await;
        let ended = rule_on(
            &f,
            OWNER,
            cairn("ended"),
            json!([remember("end", "outcome", "outcome")]),
            limits(),
        )
        .await;
        emit(&f, "first", message("alice", "hello")).await;
        engine::tick(&f.s).await.unwrap();
        let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
        let run = launched(&f, &chat).await;
        replied(&f, &run, &last_message(&f, &chat).await, "Before compact").await;
        engine::tick(&f.s).await.unwrap();
        event(
            &f,
            &run,
            "status",
            "Compacting the conversation context",
            Some(json!({ "chatOperation": "compact" })),
        )
        .await;
        event(&f, &run, "turn.started", "", None).await;
        if observe_during {
            engine::tick(&f.s).await.unwrap();
        }
        event(&f, &run, "turn.completed", "", None).await;
        // A new real turn starts before the observer reads compaction completion.
        let next = person_sends(&f, &chat, "Another request").await;
        replied(&f, &run, &next, "After compact").await;
        engine::tick(&f.s).await.unwrap();
        assert_eq!(
            triggerings(&f, &reply).await.len(),
            2,
            "observe_during={observe_during}"
        );
        assert_eq!(
            triggerings(&f, &ended).await.len(),
            2,
            "observe_during={observe_during}"
        );
        assert_eq!(values(&f, &reply).await["reply"], "After compact");
    }
}

#[tokio::test]
async fn more_than_one_page_of_pending_questions_is_reported_once_across_restart() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let asked = rule_on(
        &f,
        OWNER,
        cairn("questionAsked"),
        json!([remember("question", "question", "questionId")]),
        json!({
            "triggeringsPerHour": 600, "conversationsPerHour": 100,
        }),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    event(
        &f,
        &run,
        "chat.user",
        "",
        Some(json!({ "messageId": last_message(&f, &chat).await })),
    )
    .await;
    for index in 0..101 {
        f.s.question_receive(
            &run,
            json!({
                "id": format!("{index:064x}"), "blocking": false,
                "fields": [{ "id": "answer", "title": format!("Question {index}"), "options": [] }],
            }),
        )
        .await
        .unwrap();
    }
    // All questions belong to the old turn, even when the observer has to page
    // through them after a person's next turn is already present in the log.
    event(&f, &run, "turn.completed", "", None).await;
    let next = person_sends(&f, &chat, "A new chain after these questions").await;
    replied(&f, &run, &next, "Next answer").await;
    for _ in 0..3 {
        engine::tick(&f.s).await.unwrap();
    }
    let checked = asked.clone();
    let count: u32 =
        f.s.store
            .read(move |db| {
                Ok(db.0.query_row(
                    "SELECT count(*) FROM hook_rule_receipts WHERE rule_id=?",
                    [checked],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(count, 101);
    let checked = asked.clone();
    f.s.store.read(move |db| {
        let wrong: u32 = db.0.query_row(
            "SELECT count(*) FROM hook_rule_triggerings WHERE rule_id=? AND (json_extract(event,'$.cairn.depth')!=1 OR json_extract(event,'$.payload.origin.kind')!='rule')",
            [checked], |row| row.get(0),
        )?;
        assert_eq!(wrong, 0);
        Ok(())
    }).await.unwrap();
    let restarted = cairn_installation::service::Service::new(cairn_installation::config::Config {
        runner_url: "http://runner:4311".into(),
        ..common::config(f._root.path())
    })
    .await
    .unwrap();
    engine::tick(&restarted).await.unwrap();
    let count: u32 = restarted
        .store
        .read(move |db| {
            Ok(db.0.query_row(
                "SELECT count(*) FROM hook_rule_receipts WHERE rule_id=?",
                [asked],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 101);
}

#[tokio::test]
async fn answered_and_withdrawn_questions_still_emit_question_asked_once() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let asked = rule_on(
        &f,
        OWNER,
        cairn("questionAsked"),
        json!([remember("question", "question", "questionId")]),
        limits(),
    )
    .await;
    emit(&f, "start", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    event(
        &f,
        &run,
        "chat.user",
        "",
        Some(json!({ "messageId": first_message(&f, &chat).await })),
    )
    .await;
    let answered = "a".repeat(64);
    let withdrawn = "b".repeat(64);
    for id in [&answered, &withdrawn] {
        f.s.question_receive(
            &run,
            json!({
                "id": id, "blocking": false,
                "fields": [{ "id": "color", "title": "Which color?", "options": [] }],
            }),
        )
        .await
        .unwrap();
    }
    f.s.question_answer(
        &chat,
        &answered,
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "answers": { "color": { "selected": [], "comment": "Blue" } },
        }),
    )
    .await
    .unwrap();
    f.s.question_cancel(&chat, &withdrawn).await.unwrap();
    for _ in 0..2 {
        engine::tick(&f.s).await.unwrap();
    }
    assert_eq!(triggerings(&f, &asked).await.len(), 2);
    assert_eq!(depth_of(&f, &asked).await, [1, 1]);
}

#[tokio::test]
async fn question_origin_follows_event_order_even_with_identical_or_reversed_timestamps() {
    for question_at in [1_000, 999] {
        let f = granted().await;
        rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
        let asked = rule_on(
            &f,
            OWNER,
            cairn("questionAsked"),
            json!([remember("origin", "origin", "origin.kind")]),
            limits(),
        )
        .await;
        emit(&f, "start", message("alice", "hello")).await;
        engine::tick(&f.s).await.unwrap();
        let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
        let run = launched(&f, &chat).await;
        replied(&f, &run, &first_message(&f, &chat).await, "Old turn").await;
        let next = person_sends(&f, &chat, "A new human turn").await;
        event(
            &f,
            &run,
            "chat.user",
            "",
            Some(json!({ "messageId": next })),
        )
        .await;
        event(&f, &run, "turn.started", "", None).await;
        f.s.question_receive(
            &run,
            json!({
                "id": "e".repeat(64), "blocking": false,
                "fields": [{ "id": "color", "title": "Which color?", "options": [] }],
            }),
        )
        .await
        .unwrap();
        // Wall-clock values can tie or move backwards. Their values must not
        // replace the transactional order of the actual creation and turns.
        f.s.store
            .transaction(move |db| {
                db.0.execute("UPDATE events SET created_at=1000 WHERE run_id=?", [&run])?;
                db.0.execute(
                    "UPDATE chat_questions SET created_at=?2 WHERE chat_id=?1",
                    rusqlite::params![chat, question_at],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        engine::tick(&f.s).await.unwrap();
        assert_eq!(depth_of(&f, &asked).await, [0]);
        assert_eq!(values(&f, &asked).await["origin"], "person");
    }
}

#[tokio::test]
async fn explicit_resume_reports_each_startup_failure_and_stop_without_delivery() {
    use common::run_fixture::RunFixture;

    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let ended = rule_on(
        &f,
        OWNER,
        cairn("ended"),
        json!([
            remember("outcome", "outcome", "outcome"),
            remember("origin", "origin", "origin.kind"),
        ]),
        limits(),
    )
    .await;
    emit(&f, "start", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    f.s.store.change_run_fixture(&run, json!({
        "status": "failed", "error": "Initial authentication failed.", "recoveryPending": false,
    })).await.unwrap();
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &ended).await.len(), 1);
    let token = common::relay_fixture::grant(&f.s, &["read", "write", "run", "manage"]).await;
    let resumed = f
        .mcp("/mcp", &token, "resume_chat", json!({ "chatId": chat }))
        .await;
    assert_ne!(resumed["result"]["isError"], true, "{resumed}");
    assert_eq!(f.s.store.run(&run).await.unwrap()["status"], "queued");
    engine::tick(&f.s).await.unwrap();
    f.s.store.change_run_fixture(&run, json!({
        "status": "failed", "error": "Authentication failed again.", "recoveryPending": false,
    })).await.unwrap();
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &ended).await.len(), 2);
    assert_eq!(values(&f, &ended).await["origin"], "resume");

    let resumed = f
        .mcp("/mcp", &token, "resume_chat", json!({ "chatId": chat }))
        .await;
    assert_ne!(resumed["result"]["isError"], true, "{resumed}");
    // The recovery owner released its slot before provider startup. Stopping
    // this queued execution takes the ordinary synchronous cancellation path.
    f.s.store
        .change_run_fixture(&run, json!({ "recoveryPending": false }))
        .await
        .unwrap();
    let (status, stopped) = call(
        &f,
        OWNER,
        "POST",
        &format!("/api/chats/{chat}/stop"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    for _ in 0..2 {
        engine::tick(&f.s).await.unwrap();
    }
    assert_eq!(triggerings(&f, &ended).await.len(), 3);
    assert_eq!(values(&f, &ended).await["outcome"], "cancelled");
    assert_eq!(values(&f, &ended).await["origin"], "resume");
    assert_eq!(depth_of(&f, &ended).await, [1, 1, 1]);
}

#[tokio::test]
async fn explicit_start_author_tolerates_absence_but_not_evaluation_errors() {
    for (expression, status) in [("payload.missing", "completed"), ("1 / 0", "failed")] {
        let f = granted().await;
        let mut node = start("start");
        node["action"]["author"] = json!({ "type": "formula", "expression": expression });
        let rule = rule_on(&f, OWNER, plugin(&f), json!([node]), limits()).await;
        emit(&f, "start", message("alice", "hello")).await;
        engine::tick(&f.s).await.unwrap();
        let history = triggerings(&f, &rule).await;
        if status == "completed" {
            assert_eq!(history[0]["status"], status, "{:?}", history[0]);
        } else {
            assert!(history.is_empty(), "{history:?}");
            let (_, listed) = call(
                &f,
                OWNER,
                "GET",
                &format!("/api/hook-rules/{rule}/triggerings"),
                Value::Null,
            )
            .await;
            assert_eq!(listed["withoutEffect"], 1, "{listed}");
        }
        assert_eq!(
            bound(&f, OWNER, "chat:alice").await.is_some(),
            status == "completed"
        );
    }
}

#[tokio::test]
async fn explicitly_resuming_compaction_does_not_emit_conversation_hooks() {
    use common::run_fixture::RunFixture;

    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let ended = rule_on(
        &f,
        OWNER,
        cairn("ended"),
        json!([remember("outcome", "outcome", "outcome")]),
        limits(),
    )
    .await;
    let reply = rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([remember("reply", "reply", "text")]),
        limits(),
    )
    .await;
    emit(&f, "start", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    replied(
        &f,
        &run,
        &first_message(&f, &chat).await,
        "Before maintenance",
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    event(
        &f,
        &run,
        "status",
        "Compacting the conversation context",
        Some(json!({ "chatOperation": "compact" })),
    )
    .await;
    let mut execution = f.s.store.run(&run).await.unwrap()["chatExecution"].clone();
    execution["operation"] = "compact".into();
    f.s.store.change_run_fixture(&run, json!({
        "chatExecution": execution, "status": "failed", "error": "Compaction startup failed.", "recoveryPending": false,
    })).await.unwrap();
    let token = common::relay_fixture::grant(&f.s, &["read", "write", "run", "manage"]).await;
    let resumed = f
        .mcp("/mcp", &token, "resume_chat", json!({ "chatId": chat }))
        .await;
    assert_ne!(resumed["result"]["isError"], true, "{resumed}");
    f.s.store.change_run_fixture(&run, json!({
        "status": "failed", "error": "Compaction startup failed again.", "recoveryPending": false,
    })).await.unwrap();
    for _ in 0..2 {
        engine::tick(&f.s).await.unwrap();
    }
    assert_eq!(triggerings(&f, &ended).await.len(), 1);
    assert_eq!(triggerings(&f, &reply).await.len(), 1);
    assert_eq!(values(&f, &reply).await["reply"], "Before maintenance");
}

#[tokio::test]
async fn an_admitted_turn_reports_progress_only_after_its_worker_starts() {
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
    emit(&f, "start", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    assert_eq!(f.s.store.run(&run).await.unwrap()["status"], "queued");
    engine::tick(&f.s).await.unwrap();
    let checked = chat.clone();
    f.s.store
        .transaction(move |db| age_progress(db, &checked))
        .await
        .unwrap();
    engine::tick(&f.s).await.unwrap();
    assert!(triggerings(&f, &progress).await.is_empty());

    fixture::running_conversation(&f.s, &chat).await;
    engine::tick(&f.s).await.unwrap();
    assert!(triggerings(&f, &progress).await.is_empty());
    f.s.store
        .transaction(move |db| age_progress(db, &chat))
        .await
        .unwrap();
    engine::tick(&f.s).await.unwrap();
    assert_eq!(triggerings(&f, &progress).await.len(), 1);
    assert_eq!(values(&f, &progress).await["step"], "Thinking");
}

#[tokio::test]
async fn a_first_turn_ending_before_delivery_reports_its_admitted_cause_once() {
    use common::run_fixture::RunFixture;

    for outcome in ["failed", "cancelled"] {
        let f = granted().await;
        rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
        let ended = rule_on(
            &f,
            OWNER,
            cairn("ended"),
            json!([
                remember("outcome", "outcome", "outcome"),
                remember("origin", "origin", "origin.kind"),
            ]),
            limits(),
        )
        .await;
        emit(&f, "start", message("alice", "hello")).await;
        engine::tick(&f.s).await.unwrap();
        let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
        let run = launched(&f, &chat).await;
        let checked = run.clone();
        let delivered: u32 = f.s.store.read(move |db| Ok(db.0.query_row(
            "SELECT count(*) FROM events WHERE run_id=? AND type IN ('chat.user','turn.started','chat.wakeup')",
            [checked], |row| row.get(0),
        )?)).await.unwrap();
        assert_eq!(delivered, 0);
        if outcome == "failed" {
            // Startup can fail before a session or any provider event exists.
            f.s.store.change_run_fixture(&run, json!({
                "status": "failed", "error": "Account authentication service is unavailable.",
                "recoveryPending": false,
            })).await.unwrap();
        } else {
            let (status, stopped) = call(
                &f,
                OWNER,
                "POST",
                &format!("/api/chats/{chat}/stop"),
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{stopped}");
            assert_eq!(f.s.store.run(&run).await.unwrap()["status"], "cancelled");
        }
        for _ in 0..2 {
            engine::tick(&f.s).await.unwrap();
        }
        let history = triggerings(&f, &ended).await;
        assert_eq!(history.len(), 1, "{history:?}");
        assert_eq!(values(&f, &ended).await["outcome"], outcome);
        assert_eq!(values(&f, &ended).await["origin"], "rule");
        assert_eq!(depth_of(&f, &ended).await, [1]);
        if outcome == "failed" {
            assert_eq!(
                history[0]["event"]["payload"]["error"],
                "Account authentication service is unavailable."
            );
        }
    }
}

#[tokio::test]
async fn reply_ready_includes_the_proposed_plan_of_a_human_plan_turn() {
    use common::run_fixture::RunFixture;

    for commentary in [false, true] {
        let f = granted().await;
        common::managed_codex_home(&f.s.config.home);
        let (_, settings) = call(&f, OWNER, "GET", "/api/settings/chat-plan", Value::Null).await;
        let (status, enabled) = call(
            &f,
            OWNER,
            "PUT",
            "/api/settings/chat-plan",
            json!({
                "enabled": true, "revision": settings["revision"],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{enabled}");
        rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
        let reply = rule_on(
            &f,
            OWNER,
            cairn("replyReady"),
            json!([remember("reply", "reply", "text")]),
            limits(),
        )
        .await;
        emit(&f, "start", message("alice", "hello")).await;
        engine::tick(&f.s).await.unwrap();
        let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
        let run = launched(&f, &chat).await;
        fixture::running_conversation(&f.s, &chat).await;
        replied(
            &f,
            &run,
            &first_message(&f, &chat).await,
            "The first answer",
        )
        .await;
        engine::tick(&f.s).await.unwrap();
        set_status(&f, &run, cairn_contracts::RunStatus::Succeeded).await;
        f.s.store
            .change_run_fixture(
                &run,
                json!({
                    "sessionId": "plan-session", "workspace": "/saved/workspace",
                }),
            )
            .await
            .unwrap();
        f.s.store.save_checkpoint_boundary(&run, json!({
            "completed": true, "launched": true, "prepared": { "workspace": "/saved/workspace" },
        })).await.unwrap();
        let message = uuid::Uuid::new_v4().to_string();
        let (status, sent) = call(
            &f,
            OWNER,
            "POST",
            &format!("/api/chats/{chat}/messages"),
            json!({
                "message": { "id": message, "text": "Plan the next steps", "plan": true },
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{sent}");
        assert_eq!(launched(&f, &chat).await, run);
        assert_eq!(
            f.s.store.run(&run).await.unwrap()["chatExecution"]["plan"],
            true
        );
        event(
            &f,
            &run,
            "chat.user",
            "",
            Some(json!({ "messageId": message })),
        )
        .await;
        event(&f, &run, "turn.started", "", None).await;
        if commentary {
            event(
                &f,
                &run,
                "item.completed",
                "Preparing the approach.",
                Some(json!({
                    "item": { "type": "agent_message", "text": "Preparing the approach." },
                })),
            )
            .await;
        }
        let plan = "First inspect the existing flow, then propose a reversible change.";
        let item = cairn_installation::chat_process::chat_item(
            json!({ "type": "plan", "text": plan }),
            true,
        );
        assert_eq!(item["type"], "proposed_plan");
        event(
            &f,
            &run,
            "item.completed",
            plan,
            Some(json!({ "item": item })),
        )
        .await;
        // An empty lifecycle item contributes no visible text and must not
        // mask the complete approach, regardless of surrounding commentary.
        event(
            &f,
            &run,
            "item.started",
            "",
            Some(json!({
                "item": { "type": "proposed_plan", "text": "" },
            })),
        )
        .await;
        if commentary {
            event(
                &f,
                &run,
                "item.completed",
                "Ready for your approval.",
                Some(json!({
                    "item": { "type": "agent_message", "text": "Ready for your approval." },
                })),
            )
            .await;
        }
        // Child proposals stay visible in the transcript, but they are not the
        // conversation's final answer, even when they arrive after its proposal.
        let mut child = cairn_installation::chat_process::chat_item(
            json!({ "type": "plan", "text": "Partial child approach" }),
            true,
        );
        child["parent_id"] = "child-agent".into();
        event(
            &f,
            &run,
            "item.completed",
            "Partial child approach",
            Some(json!({ "item": child })),
        )
        .await;
        event(&f, &run, "turn.completed", "", None).await;
        engine::tick(&f.s).await.unwrap();
        assert_eq!(values(&f, &reply).await["reply"], plan);
        assert_eq!(depth_of(&f, &reply).await, [1, 0]);
    }
}

#[tokio::test]
async fn cancelling_an_inactive_turn_also_stops_its_pending_environment_continuation() {
    use common::run_fixture::RunFixture;

    for cancel in [false, true] {
        let f = granted().await;
        rule_on(&f, OWNER, plugin(&f), start_or_cancel(), limits()).await;
        emit(&f, "start", message("alice", "hello")).await;
        engine::tick(&f.s).await.unwrap();
        let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
        let run = launched(&f, &chat).await;
        let environment = f.s.store.run(&run).await.unwrap();

        // A self move has retained its old environment and continuation while
        // the interrupted worker settles. Its terminal status precedes detach.
        f.s.store
            .change_run_fixture(&run, json!({ "status": "cancelled" }))
            .await
            .unwrap();
        f.s.store
            .transaction({
                let run = run.clone();
                move |db| {
                    db.put(
                        "environment-expirations",
                        &json!({
                            "id": run, "runId": run, "expiredAt": cairn_installation::config::now(),
                            "cause": "node-change", "continueTurn": true, "cancelled": false,
                            "environment": environment,
                        }),
                    )
                }
            })
            .await
            .unwrap();
        if cancel {
            emit(&f, "cancel", message("alice", "stop before resuming")).await;
            engine::tick(&f.s).await.unwrap();
        }
        f.s.detach_stopped_environments().await.unwrap();
        let current = f.s.store.run(&run).await.unwrap();
        assert_eq!(
            current["status"],
            if cancel { "cancelled" } else { "queued" }
        );
        // Cancelling an already inactive turn does not pause later new work.
        assert_eq!(f.s.chat_detail(&chat).await.unwrap()["paused"], false);
    }
}

#[tokio::test]
async fn cancelling_an_inactive_turn_also_stops_its_pending_engine_switch() {
    use common::run_fixture::RunFixture;

    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), start_or_cancel(), limits()).await;
    emit(&f, "start", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    let current = f.s.store.run(&run).await.unwrap();
    let request = uuid::Uuid::new_v4().to_string();
    let pending = cairn_installation::chats::engine_switch::EngineSwitch {
        chat_id: chat.clone(),
        request_id: request.clone(),
        run_id: run.clone(),
        turn_id: text(&current["chatExecution"], "messageId").to_owned(),
        provider: "claude".into(),
        model: String::new(),
        pending: true,
        error: None,
        retry_at: None,
        receipt_expires_at: cairn_installation::config::now() + 60_000,
        current_expires_at: None,
    };
    f.s.store
        .transaction(move |db| db.save_engine_switch(&pending))
        .await
        .unwrap();
    // The old worker has settled, but the switch has not yet advanced.
    f.s.store
        .change_run_fixture(&run, json!({ "status": "cancelled" }))
        .await
        .unwrap();
    emit(&f, "cancel", message("alice", "stop before switching")).await;
    engine::tick(&f.s).await.unwrap();
    let state =
        f.s.store
            .read(move |db| db.engine_switch(&chat))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(state.request_id, request);
    assert!(
        !state.pending,
        "A cancelled turn must not resume via an engine switch."
    );
    assert_eq!(f.s.store.run(&run).await.unwrap()["status"], "cancelled");
}

#[tokio::test]
async fn native_conversation_actions_require_authority_on_the_runs_actual_node() {
    use common::run_fixture::RunFixture;

    let f = granted().await;
    let mut first = native_node(&f, &json!([OWNER])).await;
    let other = native_node(&f, &json!([OWNER])).await;
    let agent =
        f.s.agent_fixture(
            json!({
                "name": "Two native nodes",
                "access": { "backend": "native", "nodes": [first["id"], other["id"]] },
            }),
            None,
        )
        .await
        .unwrap();
    rule_on(
        &f,
        OWNER,
        plugin(&f),
        start_or_steer_agent(text(&agent, "id")),
        limits(),
    )
    .await;
    emit(&f, "start", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    f.s.store
        .change_run_fixture(&run, json!({ "nodeId": first["id"] }))
        .await
        .unwrap();

    let mut actions = Vec::new();
    for operation in [
        json!({ "type": "cancelConversation", "key": key() }),
        json!({ "type": "storeValue", "scope": "conversation", "key": key(), "name": "seen", "value": { "type": "literal", "value": true } }),
    ] {
        actions.push(
            rule_on(
                &f,
                OWNER,
                plugin(&f),
                json!([action("operate", &operation)]),
                limits(),
            )
            .await,
        );
    }
    first["nativeAccounts"] = json!([]);
    f.s.store.put_node_fixture(first.clone()).await.unwrap();
    emit(
        &f,
        "operate",
        message("alice", "only the other node is authorized"),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    for rule in &actions {
        let history = triggerings(&f, rule).await;
        assert_eq!(history[0]["status"], "failed", "{:?}", history[0]);
        assert!(
            text(&history[0], "error").contains("native authority"),
            "{:?}",
            history[0]
        );
    }
    assert!(f.s.store.run(&run).await.unwrap()["cancelRequestedAt"].is_null());
    let count: i64 =
        f.s.store
            .read(move |db| {
                Ok(db.0.query_row(
                    "SELECT count(*) FROM hook_rule_conversation_values WHERE chat_id=?1",
                    [&chat],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(count, 0);

    // Restoring authority on the actual node permits both effects, even though
    // another authorized node remains available throughout the scenario.
    first["nativeAccounts"] = json!([OWNER]);
    f.s.store.put_node_fixture(first).await.unwrap();
    emit(
        &f,
        "restored",
        message("alice", "the actual node is authorized again"),
    )
    .await;
    engine::tick(&f.s).await.unwrap();
    for rule in actions {
        let history = triggerings(&f, &rule).await;
        assert_eq!(history[0]["status"], "completed", "{:?}", history[0]);
    }
    assert!(!f.s.store.run(&run).await.unwrap()["cancelRequestedAt"].is_null());
}

#[tokio::test]
async fn changing_the_profile_backend_does_not_remove_a_native_conversations_authority_check() {
    let f = granted().await;
    let mut node = native_node(&f, &json!([])).await;
    let agent = f.s.agent_fixture(json!({
        "name": "Originally native", "access": { "backend": "native", "nodes": [node["id"]] },
    }), None).await.unwrap();
    let agent = text(&agent, "id").to_owned();
    let mut unprivileged = Vec::new();
    for operation in [
        json!({ "type": "cancelConversation", "key": key() }),
        json!({ "type": "storeValue", "scope": "conversation", "key": key(), "name": "seen", "value": { "type": "literal", "value": true } }),
    ] {
        unprivileged.push(
            rule_on(
                &f,
                OWNER,
                plugin(&f),
                json!([action("operate", &operation)]),
                limits(),
            )
            .await,
        );
    }
    node["nativeAccounts"] = json!([OWNER]);
    f.s.store.put_node_fixture(node).await.unwrap();
    rule_on(
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
    assert_eq!(
        f.s.store.run(&run).await.unwrap()["snapshot"]["agent"]["access"]["backend"],
        "native"
    );
    f.s.agent_fixture(
        json!({ "access": { "backend": "firecracker", "nodes": null } }),
        Some(&agent),
    )
    .await
    .unwrap();
    emit(&f, "second", message("alice", "still native")).await;
    engine::tick(&f.s).await.unwrap();
    for rule in unprivileged {
        let history = triggerings(&f, &rule).await;
        assert_eq!(history[0]["status"], "failed", "{:?}", history[0]);
        assert!(
            text(&history[0], "error").contains("saved without native authority"),
            "{:?}",
            history[0]
        );
    }
}

#[tokio::test]
async fn queued_human_input_preserves_the_active_chain_until_its_own_turn_launches() {
    use common::run_fixture::RunFixture;
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    for author in ["alice", "bob", "carol", "dave"] {
        emit(&f, author, message(author, "hello")).await;
    }
    engine::tick(&f.s).await.unwrap();
    let source = bound(&f, OWNER, "chat:alice").await.unwrap();
    let targets = [
        bound(&f, OWNER, "chat:bob").await.unwrap(),
        bound(&f, OWNER, "chat:carol").await.unwrap(),
    ];
    let fresh = bound(&f, OWNER, "chat:dave").await.unwrap();
    let mut token = fixture::running_conversation(&f.s, &source).await;
    let mut run = text(&f.s.chat_detail(&source).await.unwrap()["run"], "id").to_owned();
    f.s.store
        .change_run_fixture(&run, json!({"sessionId":"source-session"}))
        .await
        .unwrap();
    f.s.store
        .save_checkpoint_boundary(
            &run,
            json!({"completed":true,"launched":true,"prepared":{"workspace":"/saved/workspace"}}),
        )
        .await
        .unwrap();
    rule_on(
        &f,
        OWNER,
        cairn("replyReady"),
        json!([steer(
            "again",
            &json!({"type":"formula","expression":"key"}),
            &json!({"type":"literal","value":"again"})
        )]),
        json!({"triggeringsPerHour":600,"conversationsPerHour":100}),
    )
    .await;
    for turn in 1..MAX_CHAIN_DEPTH {
        replied(
            &f,
            &run,
            &last_message(&f, &source).await,
            &format!("turn {turn}"),
        )
        .await;
        set_status(&f, &run, cairn_contracts::RunStatus::Succeeded).await;
        engine::tick(&f.s).await.unwrap();
        token = fixture::running_conversation(&f.s, &source).await;
        run = text(&f.s.chat_detail(&source).await.unwrap()["run"], "id").to_owned();
    }
    let created = f
        .mcp(
            "/mcp-installation",
            &token,
            "create_chat",
            json!({"agentId":MAIN_AGENT_ID}),
        )
        .await;
    assert_ne!(created["result"]["isError"], true, "{created}");
    let unkeyed = text(&created["result"]["structuredContent"]["result"], "id").to_owned();
    let sent = f.mcp("/mcp-installation",&token,"send_chat_message",json!({
        "chatId":unkeyed,"message":{"id":uuid::Uuid::new_v4().to_string(),"text":"An unkeyed chain"},
    })).await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    let unkeyed_token = fixture::running_conversation(&f.s, &unkeyed).await;
    // Reaching an ordinary chat must not attribute a merely queued input to
    // its still working human turn. It enters the chain on actual delivery.
    let ordinary =
        f.s.chat_create(json!({ "agentId": MAIN_AGENT_ID }))
            .await
            .unwrap();
    let ordinary = text(&ordinary, "id").to_owned();
    person_sends(&f, &ordinary, "An ordinary human request").await;
    let ordinary_token = fixture::running_conversation(&f.s, &ordinary).await;
    let queued = f.mcp("/mcp-installation",&token,"send_chat_message",json!({
        "chatId":ordinary,"message":{"id":uuid::Uuid::new_v4().to_string(),"text":"A future chain input"},
    })).await;
    assert_ne!(queued["result"]["isError"], true, "{queued}");
    let ordinary_message = uuid::Uuid::new_v4().to_string();
    let sent = f.mcp("/mcp-installation",&ordinary_token,"send_chat_message",json!({
        "chatId":fresh,"message":{"id":ordinary_message,"text":"The ordinary turn still acts"},
    })).await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    let depth: u32 =
        f.s.store
            .read(move |db| {
                Ok(db.0.query_row(
                    "SELECT depth FROM hook_rule_turn_origins WHERE message_id=?",
                    [ordinary_message],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(depth, 1);
    for ((chat, token), target) in [(source.clone(), token), (unkeyed, unkeyed_token)]
        .into_iter()
        .zip(targets)
    {
        person_sends(&f, &chat, "Queue a human request while the old turn works").await;
        let message = uuid::Uuid::new_v4().to_string();
        let sent = f
            .mcp(
                "/mcp-installation",
                &token,
                "send_chat_message",
                json!({
                    "chatId":target,"message":{"id":message,"text":"The previous turn still acts"},
                }),
            )
            .await;
        assert_ne!(sent["result"]["isError"], true, "{sent}");
        f.s.store
            .read(move |db| {
                let depth: u32 = db.0.query_row(
                    "SELECT depth FROM hook_rule_turn_origins WHERE message_id=?",
                    [message],
                    |row| row.get(0),
                )?;
                assert_eq!(u64::from(depth), MAX_CHAIN_DEPTH);
                Ok(())
            })
            .await
            .unwrap();
    }
    event(&f, &run, "turn.completed", "", None).await;
    set_status(&f, &run, cairn_contracts::RunStatus::Succeeded).await;
    let human_token = fixture::running_conversation(&f.s, &source).await;
    let message = uuid::Uuid::new_v4().to_string();
    let sent = f
        .mcp(
            "/mcp-installation",
            &human_token,
            "send_chat_message",
            json!({
                "chatId":fresh,"message":{"id":message,"text":"The new human turn now acts"},
            }),
        )
        .await;
    assert_ne!(sent["result"]["isError"], true, "{sent}");
    f.s.store
        .read(move |db| {
            let depth: u32 = db.0.query_row(
                "SELECT depth FROM hook_rule_turn_origins WHERE message_id=?",
                [message],
                |row| row.get(0),
            )?;
            assert_eq!(
                depth, 1,
                "the old observer must not override the new active frame"
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_cancel_cause_survives_observation_after_a_new_human_turn() {
    let f = granted().await;
    rule_on(&f, OWNER, plugin(&f), json!([start("start")]), limits()).await;
    let ended = rule_on(
        &f,
        OWNER,
        cairn("ended"),
        json!([remember("end", "end", "outcome")]),
        limits(),
    )
    .await;
    emit(&f, "first", message("alice", "hello")).await;
    engine::tick(&f.s).await.unwrap();
    let chat = bound(&f, OWNER, "chat:alice").await.unwrap();
    let run = launched(&f, &chat).await;
    replied(&f, &run, &last_message(&f, &chat).await, "Old turn").await;
    let human = person_sends(&f, &chat, "A new human turn").await;
    event(&f, &run, "chat.user", "", Some(json!({"messageId":human}))).await;
    // A delayed action can cancel this new turn before its input is observed.
    event(
        &f,
        &run,
        "status",
        "Stop requested by a rule",
        Some(json!({"hookRuleDepth":8})),
    )
    .await;
    event(&f, &run, "status", "cancelled", None).await;
    set_status(&f, &run, cairn_contracts::RunStatus::Cancelled).await;
    engine::tick(&f.s).await.unwrap();
    let depth:u32 = f.s.store.read(move |db| Ok(db.0.query_row(
        "SELECT json_extract(event,'$.cairn.depth') FROM hook_rule_triggerings WHERE rule_id=? AND json_extract(event,'$.payload.outcome')='cancelled'",
        [ended],|row|row.get(0),
    )?)).await.unwrap();
    assert_eq!(u64::from(depth), MAX_CHAIN_DEPTH);
}
