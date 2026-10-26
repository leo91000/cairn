use common::AgentFixture;
use common::PluginStoreFixture;
use common::StoreAgentFixture;

mod common;

use common::ProjectFixture;

use cairn_installation::{
    chats::wait_for,
    config::{MAIN_AGENT_ID, id},
    delegation, project_workspaces,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};
use tempfile::TempDir;

/// A conversation turn running for `agent`, with its workspace token.
struct Turn {
    chat: String,
    run: String,
    token: String,
}

struct Fixture {
    _root: TempDir,
    s: Arc<Service>,
}

impl Fixture {
    async fn new(delegation: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        common::managed_codex_home(&root.path().join("home"));
        let s = Service::new(cairn_installation::config::Config {
            runner_url: "http://unused-runner:4311".into(),
            ..common::config(root.path())
        })
        .await
        .unwrap();
        s.agent_fixture(
            json!({ "name": "Main agent", "access": { "delegation": delegation } }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
        Self { _root: root, s }
    }

    /// Starts the chat's queued turns, then marks `chat`'s turn running with a live grant.
    async fn running(&self, chat: &str) -> Turn {
        self.s.chat_tick(&HashSet::new()).await.unwrap();
        let detail = self.s.chat_detail(chat).await.unwrap();
        let run = detail["run"].clone();
        let run_id = text(&run, "id").to_owned();
        self.s
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Running),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let run = self.s.store.run(&run_id).await.unwrap();
        let message = text(&run["chatExecution"], "messageId").to_owned();
        self.s.chat_acknowledge(&run_id, &message).await.unwrap();
        let config = self.s.mcps.run_configuration(&self.s, &run).await.unwrap();
        self.s
            .store
            .set(
                &format!("run-checkpoint:{run_id}"),
                json!({ "completed": true, "launched": true }),
                None,
            )
            .await
            .unwrap();
        Turn {
            chat: chat.to_owned(),
            run: run_id,
            token: text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned(),
        }
    }

    async fn conversation(&self, agent: &str) -> Turn {
        let chat = self
            .s
            .chat_create(json!({ "agentId": agent }))
            .await
            .unwrap();
        let chat = text(&chat, "id").to_owned();
        self.s
            .chat_send(
                &chat,
                json!({ "id": id(), "text": "Refactor the send queue" }),
            )
            .await
            .unwrap();
        self.running(&chat).await
    }

    async fn tools(&self, turn: &Turn) -> Vec<String> {
        let listing = project_workspaces::rpc(&self.s, &turn.token, "tools/list", &json!({}))
            .await
            .unwrap();
        listing["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| text(tool, "name").to_owned())
            .collect()
    }

    /// The condition types `wait_for` offers this turn.
    async fn wait_conditions(&self, turn: &Turn) -> Vec<String> {
        let listing = project_workspaces::rpc(&self.s, &turn.token, "tools/list", &json!({}))
            .await
            .unwrap();
        let wait = listing["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "wait_for")
            .unwrap()
            .clone();
        wait["inputSchema"]["properties"]["conditions"]["items"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|condition| text(&condition["properties"]["type"], "const").to_owned())
            .collect()
    }

    async fn delegate(
        &self,
        turn: &Turn,
        input: Value,
    ) -> cairn_installation::error::Result<Value> {
        delegation::delegate(&self.s, &turn.token, &input).await
    }

    /// The requester's turn ends, as when it saved a wait.
    async fn finish(&self, turn: &Turn) {
        self.s
            .store
            .change_run(
                &turn.run,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Succeeded),
                    session_id: Some(Some("saved-session".into())),
                    workspace: Some(Some("/saved/workspace".into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn delegation_is_an_opt_in_agent_power() {
    let f = Fixture::new(false).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    assert!(!f.tools(&turn).await.contains(&"delegate_task".to_owned()));
    assert!(
        !f.wait_conditions(&turn)
            .await
            .contains(&"task_finished".to_owned())
    );
    let denied = f
        .delegate(&turn, json!({ "task": "Review the branch" }))
        .await
        .unwrap_err();
    assert_eq!(denied.status, 403);

    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    let tools = f.tools(&turn).await;
    for tool in ["delegate_task", "task_status", "task_cancel"] {
        assert!(tools.contains(&tool.to_owned()), "{tool} is offered");
    }
    assert_eq!(
        f.wait_conditions(&turn).await.last().unwrap(),
        "task_finished"
    );
    let run = f.s.store.run(&turn.run).await.unwrap();
    assert_eq!(run["snapshot"]["delegation"], "requester");
}

#[tokio::test]
async fn a_delegated_conversation_remembers_its_requester_and_runs_on_the_chosen_engine() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    let request = id();
    let task = json!({
        "task": "Review delegate/send-queue against main and report findings.",
        "title": "Review the send queue",
        "provider": "claude",
        "model": "claude-sonnet-5",
        "reasoning": "medium",
        "requestId": request,
    });
    let delegated = f.delegate(&turn, task.clone()).await.unwrap();
    assert_eq!(delegated["title"], "Review the send queue");
    assert_eq!(delegated["agentId"], MAIN_AGENT_ID);
    assert_eq!(delegated["provider"], "claude");
    assert_eq!(delegated["model"], "claude-sonnet-5");
    assert_eq!(delegated["finished"], false);
    let child = text(&delegated, "chatId").to_owned();

    // Retrying the same delegation returns the same conversation.
    let retried = f.delegate(&turn, task).await.unwrap();
    assert_eq!(retried["chatId"], delegated["chatId"]);

    // The requester's history shows the task once, where it was delegated.
    let run = turn.run.clone();
    let events =
        f.s.store
            .read(move |db| db.events(&run, 0, 500))
            .await
            .unwrap();
    let announced: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "chat.delegated")
        .collect();
    assert_eq!(announced.len(), 1);
    assert_eq!(announced[0]["payload"]["chatId"], child.as_str());
    assert_eq!(announced[0]["payload"]["provider"], "claude");

    let detail = f.s.chat_detail(&child).await.unwrap();
    assert_eq!(detail["requestedBy"]["chatId"], turn.chat.as_str());
    assert_eq!(detail["requestedBy"]["runId"], turn.run.as_str());
    assert_eq!(detail["requestedBy"]["requestId"], request.as_str());
    let requester = f.s.chat_detail(&turn.chat).await.unwrap();
    assert_eq!(detail["requester"]["chatId"], turn.chat.as_str());
    assert_eq!(detail["requester"]["title"], requester["title"]);
    assert!(requester["requester"].is_null());

    let delegated_turn = f.running(&child).await;
    let run = f.s.store.run(&delegated_turn.run).await.unwrap();
    assert_eq!(run["snapshot"]["agent"]["provider"], "claude");
    assert_eq!(run["snapshot"]["agent"]["model"], "claude-sonnet-5");
    assert_eq!(run["snapshot"]["agent"]["reasoning"], "medium");
    assert_eq!(run["snapshot"]["delegation"], "delegated");
    assert!(
        text(&run["chatExecution"], "text").contains("Review delegate/send-queue"),
        "The task is the delegated conversation's first message"
    );

    // Delegated conversations report back; they cannot delegate further.
    assert!(
        !f.tools(&delegated_turn)
            .await
            .contains(&"delegate_task".to_owned())
    );
    let nested = f
        .delegate(&delegated_turn, json!({ "task": "Delegate again" }))
        .await
        .unwrap_err();
    assert_eq!(nested.status, 403);
}

#[tokio::test]
async fn delegation_never_reaches_access_the_requesting_agent_lacks() {
    let f = Fixture::new(true).await;
    let project = id();
    let restricted =
        f.s.agent_fixture(
            json!({
                "name": "Reviewer",
                "access": { "projects": [], "skills": [], "mcps": [], "delegation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let turn = f.conversation(text(&restricted, "id")).await;
    let wider = f
        .delegate(
            &turn,
            json!({ "task": "Use the main agent", "agentId": MAIN_AGENT_ID }),
        )
        .await
        .unwrap_err();
    assert_eq!(wider.status, 403);
    assert!(wider.message.contains("access"));

    // An agent within the requester's access is allowed.
    let narrower =
        f.s.agent_fixture(
            json!({
                "name": "Read-only reviewer",
                "access": { "projects": [], "skills": [], "mcps": [], "sandbox": "read-only" },
            }),
            None,
        )
        .await
        .unwrap();
    let delegated = f
        .delegate(
            &turn,
            json!({ "task": "Review", "agentId": narrower["id"], "projectId": null }),
        )
        .await
        .unwrap();
    assert_eq!(delegated["agentId"], narrower["id"]);
    assert_ne!(delegated["projectId"], json!(project));

    // An engine the target agent cannot use fails the call rather than the turn.
    let invalid = f
        .delegate(
            &turn,
            json!({ "task": "Review", "provider": "claude", "model": "gpt-5.5" }),
        )
        .await
        .unwrap_err();
    assert_eq!(invalid.status, 400);
}

#[tokio::test]
async fn delegation_checks_node_plugin_and_exclusive_reservation_grants() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    let requester = f.s.agent_snapshot(MAIN_AGENT_ID).await.unwrap();
    let target =
        f.s.agent_fixture(json!({ "name": "Target" }), None)
            .await
            .unwrap();
    let node = id();
    let instance = id();
    f.s.store
        .put_instance_fixture(json!({ "id": instance, "pluginId": "fixture" }))
        .await
        .unwrap();
    let changes = [
        ("plugins", json!([instance])),
        ("exclusiveNodes", json!([node])),
        ("access", {
            let mut access = requester["access"].clone();
            access["nodes"] = json!([cairn_installation::nodes::LOCAL_NODE_ID, node]);
            access
        }),
        ("access", {
            let mut access = requester["access"].clone();
            access["nodes"] = Value::Null;
            access
        }),
    ];
    for (key, value) in changes {
        let mut wider = target.clone();
        wider[key] = value;
        f.s.store.put_agent_fixture(wider).await.unwrap();
        let denied = f
            .delegate(&turn, json!({ "task": "Review", "agentId": target["id"] }))
            .await
            .unwrap_err();
        assert_eq!(denied.status, 403, "{key}: {}", denied.message);
    }

    // Equal live resource grants permit another agent, without changing covers() globally.
    let mut current = requester;
    current["plugins"] = json!([instance]);
    current["exclusiveNodes"] = json!([node]);
    f.s.store.put_agent_fixture(current).await.unwrap();
    let mut allowed = target.clone();
    allowed["plugins"] = json!([instance]);
    allowed["exclusiveNodes"] = json!([node]);
    f.s.store.put_agent_fixture(allowed).await.unwrap();
    assert!(
        f.delegate(&turn, json!({ "task": "Review", "agentId": target["id"] }))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn omitted_engine_choices_are_recorded_before_the_task_starts() {
    let f = Fixture::new(true).await;
    f.s.agent_fixture(
        json!({
            "name": "Main agent",
            "provider": "codex",
            "model": "gpt-5.5",
            "reasoning": "high",
            "access": { "delegation": true },
        }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let turn = f.conversation(MAIN_AGENT_ID).await;
    let delegated = f
        .delegate(&turn, json!({ "task": "Review" }))
        .await
        .unwrap();
    assert_eq!(delegated["provider"], "codex");
    assert_eq!(delegated["model"], "gpt-5.5");
    let events =
        f.s.store
            .read(move |db| db.events(&turn.run, 0, 500))
            .await
            .unwrap();
    let announced = events
        .iter()
        .find(|event| event["type"] == "chat.delegated")
        .unwrap();
    assert_eq!(announced["payload"]["provider"], "codex");
    assert_eq!(announced["payload"]["model"], "gpt-5.5");
}

#[tokio::test]
async fn deleted_tasks_are_finished_in_status_and_waits() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    let delegated = f
        .delegate(&turn, json!({ "task": "Review" }))
        .await
        .unwrap();
    let child = text(&delegated, "chatId");
    let child_turn = f.running(child).await;
    f.finish(&child_turn).await;
    f.s.store
        .change_run(&child_turn.run,cairn_installation::runs::RunChanges {
            summary : Some(("A report that must disappear when deleted").to_owned()),
            boundary: Some(serde_json::json!({"outcome": { "status": "completed", "reason": "A private outcome" }})),
            ..Default::default()
        })
        .await
        .unwrap();
    f.s.chat_trash(child, true).await.unwrap();
    let status = delegation::status(&f.s, &turn.token, &json!({ "chatId": child }))
        .await
        .unwrap();
    assert_eq!(status["finished"], true);
    assert_eq!(status["lifecycle"], "trash");
    assert_eq!(status["report"], "");
    assert!(status["outcome"].is_null());
    assert_eq!(status["pendingQuestions"], 0);
    let listed = delegation::status(&f.s, &turn.token, &json!({}))
        .await
        .unwrap();
    assert_eq!(listed["tasks"][0]["preview"], "");
    assert!(listed["tasks"][0]["outcome"].is_null());
    let wait = wait_for::schedule(
        &f.s,
        &turn.token,
        &json!({
            "conditions": [{ "type": "task_finished", "chatId": child }],
            "reason": "Observe the deleted task",
            "timeoutSeconds": 60,
        }),
    )
    .await
    .unwrap();
    assert_eq!(wait["deferred"], false);
    assert_eq!(wait["observation"]["status"], "deleted");
}

#[tokio::test]
async fn the_requester_waits_for_its_task_and_reads_the_report() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    let delegated = f
        .delegate(&turn, json!({ "task": "Review the branch" }))
        .await
        .unwrap();
    let child = text(&delegated, "chatId").to_owned();
    let condition = json!({ "type": "task_finished", "chatId": child });
    let wait = wait_for::schedule(
        &f.s,
        &turn.token,
        &json!({ "conditions": [condition], "reason": "Wait for the review", "timeoutSeconds": 3600 }),
    )
    .await
    .unwrap();
    assert_eq!(wait["deferred"], true);
    f.finish(&turn).await;

    let delegated_turn = f.running(&child).await;
    let resumed = f.s.chat_detail(&turn.chat).await.unwrap();
    assert_eq!(resumed["run"]["status"], "succeeded", "Still waiting");

    f.s.store
        .change_run(&delegated_turn.run,cairn_installation::runs::RunChanges {
            status : Some(cairn_contracts::RunStatus::Succeeded),
            finished_at : Some(Some(cairn_installation::config::now())),
            summary : Some(("**2 findings**\n\n1. Retry can reorder a steered message.").to_owned()),
            boundary: Some(serde_json::json!({"outcome": { "status": "completed", "reason": "Reviewed", "evidence": ["diff"] }})),
            ..Default::default()
        })
        .await
        .unwrap();
    f.s.chat_tick(&HashSet::new()).await.unwrap();
    let resumed = f.s.chat_detail(&turn.chat).await.unwrap();
    assert_eq!(resumed["run"]["status"], "queued");
    let execution = text(&resumed["run"]["chatExecution"], "text");
    assert!(execution.contains(&child));
    assert!(execution.contains("task_finished"));

    // The report is read with the requester's next live turn.
    let turn = f.running(&turn.chat).await;
    let status = delegation::status(&f.s, &turn.token, &json!({ "chatId": child }))
        .await
        .unwrap();
    assert_eq!(status["finished"], true);
    assert_eq!(status["status"], "succeeded");
    assert_eq!(status["outcome"]["status"], "completed");
    assert!(text(&status, "report").contains("Retry can reorder"));
    let listed = delegation::status(&f.s, &turn.token, &json!({}))
        .await
        .unwrap();
    assert_eq!(listed["tasks"][0]["chatId"], child.as_str());
    assert!(text(&listed["tasks"][0], "preview").starts_with("2 findings"));
}

#[tokio::test]
async fn delegated_tasks_can_be_waited_on_as_all_or_any_without_installation_access() {
    for mode in ["all", "any"] {
        let f = Fixture::new(true).await;
        let turn = f.conversation(MAIN_AGENT_ID).await;
        let first = f
            .delegate(&turn, json!({ "task": "Review the backend" }))
            .await
            .unwrap();
        let second = f
            .delegate(&turn, json!({ "task": "Review the UI" }))
            .await
            .unwrap();
        let first_chat = text(&first, "chatId");
        let second_chat = text(&second, "chatId");
        let wait = wait_for::schedule(
            &f.s,
            &turn.token,
            &json!({
                "conditions": [
                    { "type": "task_finished", "chatId": first_chat },
                    { "type": "task_finished", "chatId": second_chat },
                ],
                "mode": mode,
                "reason": "Wait for independent reviews",
                "timeoutSeconds": 3600,
            }),
        )
        .await
        .unwrap();
        assert_eq!(wait["deferred"], true);
        f.finish(&turn).await;
        let first_turn = f.running(first_chat).await;
        let second_turn = f.running(second_chat).await;
        f.s.store
            .change_run(
                &first_turn.run,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Failed),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        wait_for::tick_groups(&f.s, &HashSet::new()).await.unwrap();
        let partial = f.s.chat_detail(&turn.chat).await.unwrap();
        assert_eq!(
            partial["wakeup"]["members"][0]["observation"]["status"],
            "failed"
        );
        assert!(partial["wakeup"]["members"][1]["observation"].is_null());
        f.s.chat_tick(&HashSet::new()).await.unwrap();
        if mode == "all" {
            assert_eq!(
                f.s.chat_detail(&turn.chat).await.unwrap()["run"]["status"],
                "succeeded"
            );
            f.finish(&second_turn).await;
            f.s.chat_tick(&HashSet::new()).await.unwrap();
        }
        let resumed = f.s.chat_detail(&turn.chat).await.unwrap();
        assert_eq!(resumed["run"]["status"], "queued");
        let execution = text(&resumed["run"]["chatExecution"], "text");
        assert!(execution.contains(first_chat) && execution.contains(second_chat));
        assert!(execution.contains(r#""status":"failed""#));
        if mode == "any" {
            assert!(execution.contains(r#""result":null"#));
            assert_eq!(
                f.s.store.run(&second_turn.run).await.unwrap()["status"],
                "running"
            );
        }
    }
}

#[tokio::test]
async fn tasks_belong_to_the_conversation_that_delegated_them() {
    let f = Fixture::new(true).await;
    let first = f.conversation(MAIN_AGENT_ID).await;
    let second = f.conversation(MAIN_AGENT_ID).await;
    let delegated = f
        .delegate(&first, json!({ "task": "Review the branch" }))
        .await
        .unwrap();
    let child = text(&delegated, "chatId");
    let read = delegation::status(&f.s, &second.token, &json!({ "chatId": child }))
        .await
        .unwrap_err();
    assert_eq!(read.status, 404);
    let cancel = delegation::cancel(&f.s, &second.token, &json!({ "chatId": child }))
        .await
        .unwrap_err();
    assert_eq!(cancel.status, 404);
    let wait = wait_for::schedule(
        &f.s,
        &second.token,
        &json!({
            "conditions": [{ "type": "task_finished", "chatId": child }],
            "reason": "Not mine",
            "timeoutSeconds": 60,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(wait.status, 404);

    let cancelled = delegation::cancel(&f.s, &first.token, &json!({ "chatId": child }))
        .await
        .unwrap();
    assert_eq!(cancelled["finished"], true);
    assert_eq!(f.s.chat_detail(child).await.unwrap()["paused"], true);
}

#[tokio::test]
async fn a_requester_has_a_bounded_number_of_working_tasks() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    for index in 0..delegation::MAX_ACTIVE {
        f.delegate(&turn, json!({ "task": format!("Task {index}") }))
            .await
            .unwrap();
    }
    let over = f
        .delegate(&turn, json!({ "task": "One too many" }))
        .await
        .unwrap_err();
    assert_eq!(over.status, 409);
}

#[tokio::test]
async fn withdrawing_the_first_message_finishes_an_unstarted_task_and_releases_its_slot() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    let mut children = Vec::new();
    for index in 0..delegation::MAX_ACTIVE {
        let task = f
            .delegate(&turn, json!({ "task": format!("Task {index}") }))
            .await
            .unwrap();
        children.push(text(&task, "chatId").to_owned());
    }
    let child = &children[0];
    let detail = f.s.chat_detail(child).await.unwrap();
    assert!(detail["run"].is_null());
    assert_eq!(detail["status"], "queued");
    let message = text(&detail["messages"][0], "id");
    f.s.chat_edit(child, message, None).await.unwrap();

    let status = delegation::status(&f.s, &turn.token, &json!({ "chatId": child }))
        .await
        .unwrap();
    assert_eq!(status["finished"], true);
    assert_eq!(status["status"], "idle");
    let wait = wait_for::schedule(
        &f.s,
        &turn.token,
        &json!({
            "conditions": [{ "type": "task_finished", "chatId": child }],
            "reason": "Observe the withdrawn task",
            "timeoutSeconds": 60,
        }),
    )
    .await
    .unwrap();
    assert_eq!(wait["deferred"], false);
    assert_eq!(wait["observation"]["status"], "idle");
    assert!(
        f.delegate(&turn, json!({ "task": "Replacement" }))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn preparation_failures_reach_the_requester_and_are_redacted_after_deletion() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    let project = id();
    f.s.project_fixture(
        json!({ "creationId": project, "name": "Review project", "path": f._root.path() }),
        None,
    )
    .await
    .unwrap();
    let agent =
        f.s.agent_fixture(
            json!({ "name": "Reviewer", "access": { "projects": [project] } }),
            None,
        )
        .await
        .unwrap();
    let delegated = f
        .delegate(
            &turn,
            json!({ "task": "Review the project", "agentId": agent["id"], "projectId": project }),
        )
        .await
        .unwrap();
    let child = text(&delegated, "chatId");
    f.s.agent_fixture(
        json!({ "name": "Reviewer", "access": { "projects": [] } }),
        agent["id"].as_str(),
    )
    .await
    .unwrap();
    f.s.chat_tick(&HashSet::new()).await.unwrap();
    let error =
        f.s.store
            .kv(&format!("chat-error:{child}"))
            .await
            .unwrap()
            .unwrap();
    assert!(!error.as_str().unwrap().is_empty());
    assert!(f.s.chat_detail(child).await.unwrap()["run"].is_null());

    let status = delegation::status(&f.s, &turn.token, &json!({ "chatId": child }))
        .await
        .unwrap();
    assert_eq!(status["finished"], true);
    assert_eq!(status["paused"], true);
    assert_eq!(status["error"], error);
    let listed = delegation::status(&f.s, &turn.token, &json!({}))
        .await
        .unwrap();
    assert_eq!(listed["tasks"][0]["error"], error);

    let wait_input = json!({
        "conditions": [{ "type": "task_finished", "chatId": child }],
        "reason": "Observe the preparation failure",
        "timeoutSeconds": 60,
    });
    let wait = wait_for::schedule(&f.s, &turn.token, &wait_input)
        .await
        .unwrap();
    assert_eq!(wait["deferred"], false);
    assert_eq!(wait["observation"]["status"], "queued");
    assert_eq!(wait["observation"]["paused"], true);
    assert_eq!(wait["observation"]["error"], error);

    f.s.chat_trash(child, true).await.unwrap();
    let deleted = delegation::status(&f.s, &turn.token, &json!({ "chatId": child }))
        .await
        .unwrap();
    assert!(deleted["error"].is_null());
    let wait = wait_for::schedule(&f.s, &turn.token, &wait_input)
        .await
        .unwrap();
    assert_eq!(wait["observation"]["status"], "deleted");
    assert!(wait["observation"]["error"].is_null());
}

#[tokio::test]
async fn run_failures_include_their_error_in_status_and_wait_results() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    let delegated = f
        .delegate(&turn, json!({ "task": "Review" }))
        .await
        .unwrap();
    let child = text(&delegated, "chatId");
    let child_turn = f.running(child).await;
    f.s.store
        .change_run(
            &child_turn.run,
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Failed),
                error: Some(Some("Provider request failed".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let status = delegation::status(&f.s, &turn.token, &json!({ "chatId": child }))
        .await
        .unwrap();
    assert_eq!(status["finished"], true);
    assert_eq!(status["error"], "Provider request failed");
    let wait = wait_for::schedule(
        &f.s,
        &turn.token,
        &json!({
            "conditions": [{ "type": "task_finished", "chatId": child }],
            "reason": "Observe the failure",
            "timeoutSeconds": 60,
        }),
    )
    .await
    .unwrap();
    assert_eq!(wait["deferred"], false);
    assert_eq!(wait["observation"]["status"], "failed");
    assert_eq!(wait["observation"]["error"], "Provider request failed");
}

#[tokio::test]
async fn removing_delegation_revokes_a_live_requester() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    f.s.agent_fixture(
        json!({ "name": "Main agent", "access": { "delegation": false } }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let denied = f
        .delegate(&turn, json!({ "task": "Review the branch" }))
        .await
        .unwrap_err();
    assert_eq!(denied.status, 403);
    assert!(!f.tools(&turn).await.contains(&"delegate_task".to_owned()));
}

#[tokio::test]
async fn older_profile_updates_keep_delegation() {
    let f = Fixture::new(true).await;
    // An older client sends access without the key it does not know.
    let saved =
        f.s.agent_fixture(
            json!({ "name": "Main agent", "access": { "installation": false } }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
    assert_eq!(saved["access"]["delegation"], true);
}

#[tokio::test]
async fn deleting_a_task_redacts_recorded_group_progress_and_final_results() {
    for finalized in [false, true] {
        let f = Fixture::new(true).await;
        let parent = f.conversation(MAIN_AGENT_ID).await;
        let first = f
            .delegate(&parent, json!({ "task": "Review backend" }))
            .await
            .unwrap();
        let second = f
            .delegate(&parent, json!({ "task": "Review UI" }))
            .await
            .unwrap();
        let first_chat = text(&first, "chatId");
        let second_chat = text(&second, "chatId");
        wait_for::schedule(
            &f.s,
            &parent.token,
            &json!({
                "conditions": [
                    { "type": "task_finished", "chatId": first_chat },
                    { "type": "task_finished", "chatId": second_chat },
                ],
                "mode": "all", "reason": "Collect reviews", "timeoutSeconds": 3600,
            }),
        )
        .await
        .unwrap();
        f.finish(&parent).await;
        let first_turn = f.running(first_chat).await;
        let second_turn = f.running(second_chat).await;
        let diagnostic = "Private diagnostic from a deleted task";
        f.s.store
            .change_run(
                &first_turn.run,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Failed),
                    error: Some(Some((diagnostic).to_owned())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        wait_for::tick_groups(&f.s, &HashSet::new()).await.unwrap();
        let partial = f.s.chat_detail(&parent.chat).await.unwrap();
        assert_eq!(
            partial["wakeup"]["members"][0]["observation"]["error"],
            diagnostic
        );
        if finalized {
            f.finish(&second_turn).await;
            wait_for::tick_groups(&f.s, &HashSet::new()).await.unwrap();
            assert_eq!(
                f.s.chat_detail(&parent.chat).await.unwrap()["wakeup"]["observation"]["state"],
                "completed"
            );
        }
        f.s.chat_trash(first_chat, true).await.unwrap();
        if !finalized {
            f.finish(&second_turn).await;
        }
        f.s.chat_tick(&HashSet::new()).await.unwrap();
        let resumed = f.s.chat_detail(&parent.chat).await.unwrap();
        assert_eq!(resumed["run"]["status"], "queued");
        let prompt = text(&resumed["run"]["chatExecution"], "text");
        assert!(prompt.contains(r#""status":"deleted""#));
        assert!(prompt.contains(r#""error":null"#));
        assert!(!prompt.contains(diagnostic));
    }
}
