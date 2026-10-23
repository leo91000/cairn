mod common;

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
        s.agent(
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
        let run = self
            .s
            .store
            .patch_run(&run_id, json!({ "status": "running" }))
            .await
            .unwrap();
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
        wait["inputSchema"]["properties"]["condition"]["oneOf"]
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
            .patch_run(
                &turn.run,
                json!({ "status": "succeeded", "sessionId": "saved-session", "workspace": "/saved/workspace" }),
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
        f.s.agent(
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
        f.s.agent(
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
        &json!({ "condition": condition, "reason": "Wait for the review", "timeoutSeconds": 3600 }),
    )
    .await
    .unwrap();
    assert_eq!(wait["deferred"], true);
    f.finish(&turn).await;

    let delegated_turn = f.running(&child).await;
    let resumed = f.s.chat_detail(&turn.chat).await.unwrap();
    assert_eq!(resumed["run"]["status"], "succeeded", "Still waiting");

    f.s.store
        .patch_run(
            &delegated_turn.run,
            json!({
                "status": "succeeded",
                "finishedAt": cairn_installation::config::now(),
                "summary": "**2 findings**\n\n1. Retry can reorder a steered message.",
                "outcome": { "status": "completed", "reason": "Reviewed", "evidence": ["diff"] },
            }),
        )
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
            "condition": { "type": "task_finished", "chatId": child },
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
async fn removing_delegation_revokes_a_live_requester() {
    let f = Fixture::new(true).await;
    let turn = f.conversation(MAIN_AGENT_ID).await;
    f.s.agent(
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
        f.s.agent(
            json!({ "name": "Main agent", "access": { "installation": false } }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
    assert_eq!(saved["access"]["delegation"], true);
}
