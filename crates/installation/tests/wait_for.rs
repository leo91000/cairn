mod common;
#[path = "wait_for/pr_checks.rs"]
mod pr_checks;

use cairn_installation::{
    chats::wait_for,
    config::{MAIN_AGENT_ID, id, now},
    plugins::{conditions, connections},
    service::Service,
    validation::text,
};
use common::github::{self, Github};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tempfile::TempDir;

const SHA: &str = "1111111111111111111111111111111111111111";

struct Fixture {
    root: TempDir,
    s: Arc<Service>,
    github: Github,
    instance: String,
    chat: String,
    run: String,
    token: String,
    project: String,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_origin("https://github.com/fixture/repo.git").await
    }

    async fn with_origin(origin: &str) -> Self {
        Self::with_identity(origin, None).await
    }

    async fn with_identity(
        origin: &str,
        identity: Option<cairn_installation::auth::InstallationIdentity>,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        common::managed_codex_home(&root.path().join("home"));
        let github = Github::start(root.path()).await;
        let s = Service::new(cairn_installation::config::Config {
            runner_url: "http://unused-runner:4311".into(),
            ..common::config(root.path())
        })
        .await
        .unwrap();
        github.attach(&s);
        s.agent(
            json!({ "name": "Main agent", "access": { "installation": true } }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
        let instance =
            github::instance(&s, "Shared", "fixture-shared-token", &[MAIN_AGENT_ID]).await;
        let project = id();
        let path = root.path().join("project");
        std::fs::create_dir(&path).unwrap();
        s.store
            .put(
                "projects",
                json!({
                        "id": project,
                        "name": "Wait fixture",
                        "path": path,
                        "sourceMode": "remote",
                        "baseBranch": "main",
                "origin": origin,
                    }),
            )
            .await
            .unwrap();
        let chat = s.chat_create(json!({})).await.unwrap();
        let chat = text(&chat, "id").to_owned();
        let message = id();
        s.chat_send_as(
            &chat,
            json!({ "id": message, "text": "Finish after the result" }),
            identity.as_ref(),
        )
        .await
        .unwrap();
        s.chat_tick(&HashSet::new()).await.unwrap();
        let run = s.chat_detail(&chat).await.unwrap()["run"].clone();
        let run_id = text(&run, "id").to_owned();
        s.store
            .patch_run(&run_id, json!({ "status": "running" }))
            .await
            .unwrap();
        s.chat_acknowledge(&run_id, &message).await.unwrap();
        let config = s.mcps.run_configuration(&s, &run).await.unwrap();
        let token = text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
        s.store
            .set(
                &format!("run-checkpoint:{run_id}"),
                json!({ "completed": true, "launched": true }),
                None,
            )
            .await
            .unwrap();
        let f = Self {
            root,
            s,
            github,
            instance,
            chat,
            run: run_id,
            token,
            project,
        };
        f.github_state("in_progress", None, 0);
        f
    }

    async fn mission() -> Self {
        let mut f = Self::new().await;
        let task = f.s.task(json!({ "name": "Waiting mission", "prompt": "Finish after CI", "agentId": MAIN_AGENT_ID }), None).await.unwrap();
        let run =
            f.s.enqueue(text(&task, "id"), "manual", None)
                .await
                .unwrap();
        f.run = text(&run, "id").to_owned();
        let run =
            f.s.store
                .patch_run(&f.run, json!({ "status": "running" }))
                .await
                .unwrap();
        let config = f.s.mcps.run_configuration(&f.s, &run).await.unwrap();
        f.token = text(&config["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
        f.s.store
            .set(
                &format!("run-checkpoint:{}", f.run),
                json!({
                    "completed": true, "launched": true, "remainingMs": 90_000,
                }),
                None,
            )
            .await
            .unwrap();
        // Block admission while exercising the real worker's durable wait tick.
        f.s.store
            .set("deployment-lease", json!({ "id": "fixture" }), None)
            .await
            .unwrap();
        f
    }

    async fn park_mission(&self) {
        self.s
            .store
            .patch_run(
                &self.run,
                json!({
                    "status": "waiting", "recoveryPending": false,
                    "sessionId": "saved-session", "workspace": "/saved/workspace",
                }),
            )
            .await
            .unwrap();
    }

    async fn mission_tick(&self) -> Value {
        self.s.worker.tick(&self.s).await.unwrap();
        self.s.store.run(&self.run).await.unwrap()
    }

    /// The same installation after a restart, still talking to the local GitHub.
    async fn restart(&self) -> Arc<Service> {
        let restarted = Service::new(self.s.config.clone()).await.unwrap();
        self.github.attach(&restarted);
        restarted
    }

    fn github_condition(&self) -> Value {
        json!({ "type": "github_workflow", "projectId": self.project, "runId": 42, "headSha": SHA })
    }

    fn github_state(&self, status: &str, conclusion: Option<&str>, delay: u64) {
        self.github.state(&json!({
            "delay": delay,
            "response": { "id": 42, "head_sha": SHA, "status": status, "conclusion": conclusion, "run_attempt": 1 },
        }));
    }

    /// GitHub requests made for conditions, not for instance forms.
    fn calls(&self) -> Vec<Value> {
        self.github.api_calls()
    }

    async fn target(&self) -> Value {
        let task = self.s.task(json!({ "name": "Other work", "prompt": "Check result", "worktree": false, "agentId": MAIN_AGENT_ID }), None).await.unwrap();
        self.s
            .enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap()
    }

    async fn schedule(&self, condition: Value) -> Value {
        wait_for::schedule(
            &self.s,
            &self.token,
            &json!({ "condition": condition, "reason": "Wait for result", "timeoutSeconds": 420 }),
        )
        .await
        .unwrap()
    }

    async fn finish(&self) {
        self.s.store.patch_run(&self.run, json!({
            "status": "succeeded", "recoveryPending": false, "sessionId": "saved-session", "workspace": "/saved/workspace",
        })).await.unwrap();
    }

    async fn tick(&self) -> Value {
        self.s.chat_tick(&HashSet::new()).await.unwrap();
        self.s.chat_detail(&self.chat).await.unwrap()
    }

    async fn due(&self) {
        let mut chat = self.s.get("chats", &self.chat).await.unwrap();
        chat["wakeup"]["wakeAt"] = (now() - 1000).into();
        self.s.store.put("chats", chat).await.unwrap();
    }

    async fn next_poll(&self) {
        let mut chat = self.s.get("chats", &self.chat).await.unwrap();
        chat["wakeup"]["nextCheckAt"] = 0.into();
        self.s.store.put("chats", chat).await.unwrap();
    }
}

#[tokio::test]
async fn run_wait_survives_restart_and_only_resumes_after_target_completion() {
    let f = Fixture::new().await;
    let target = f.target().await;
    let input = json!({ "type": "run_finished", "runId": target["id"] });
    let wait = f.schedule(input.clone()).await;
    assert_eq!(wait, f.schedule(input).await);
    assert_eq!(wait["deferred"], true);
    f.finish().await;
    assert_eq!(f.tick().await["run"]["status"], "succeeded");
    let restarted = f.restart().await;
    restarted.chat_tick(&HashSet::new()).await.unwrap();
    assert_eq!(
        restarted.chat_detail(&f.chat).await.unwrap()["wakeup"],
        wait["wakeup"]
    );
    f.s.store
        .patch_run(
            text(&target, "id"),
            json!({ "status": "succeeded", "finishedAt": now() }),
        )
        .await
        .unwrap();
    restarted
        .chat_tick(&HashSet::from([f.run.clone()]))
        .await
        .unwrap();
    assert_eq!(
        restarted.chat_detail(&f.chat).await.unwrap()["run"]["status"],
        "succeeded"
    );
    restarted.chat_tick(&HashSet::new()).await.unwrap();
    let resumed = restarted.chat_detail(&f.chat).await.unwrap();
    assert_eq!(resumed["run"]["status"], "queued");
    assert_eq!(resumed["run"]["sessionId"], "saved-session");
    assert_eq!(resumed["run"]["workspace"], "/saved/workspace");
    assert!(text(&resumed["run"]["chatExecution"], "text").contains(text(&target, "id")));
    assert!(resumed["wakeup"].is_null());
    assert_eq!(resumed["messages"].as_array().unwrap().len(), 1);
    assert_eq!(f.calls().len(), 0, "Internal conditions never query GitHub");
}

#[tokio::test]
async fn installation_access_can_wait_for_another_agents_run() {
    let f = Fixture::new().await;
    let agent =
        f.s.agent(json!({ "name": "Delegated worker" }), None)
            .await
            .unwrap();
    let task =
        f.s.task(
            json!({
                "name": "Delegated work",
                "prompt": "Check result",
                "worktree": false,
                "agentId": agent["id"]
            }),
            None,
        )
        .await
        .unwrap();
    let target =
        f.s.enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap();
    let wait = f
        .schedule(json!({ "type": "run_finished", "runId": target["id"] }))
        .await;
    assert_eq!(wait["deferred"], true);
    f.finish().await;
    f.s.store
        .patch_run(text(&target, "id"), json!({ "status": "succeeded" }))
        .await
        .unwrap();
    let resumed = f.tick().await;
    assert_eq!(resumed["run"]["status"], "queued");
    assert!(text(&resumed["run"]["chatExecution"], "text").contains(text(&target, "id")));
}

#[tokio::test]
async fn already_finished_sources_return_results_without_scheduling_or_ending_the_turn() {
    for status in ["succeeded", "failed", "cancelled", "interrupted"] {
        let f = Fixture::new().await;
        let target = f.target().await;
        f.s.store
            .patch_run(text(&target, "id"), json!({ "status": status }))
            .await
            .unwrap();
        wait_for::schedule(
            &f.s,
            &f.token,
            &json!({ "condition": { "type": "delay", "seconds": 420 }, "reason": "Old delay" }),
        )
        .await
        .unwrap();
        let result = f
            .schedule(json!({ "type": "run_finished", "runId": target["id"] }))
            .await;
        assert_eq!(result["deferred"], false);
        assert_eq!(result["observation"]["status"], status);
        assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
        assert_eq!(f.s.store.run(&f.run).await.unwrap()["status"], "running");
    }
    let f = Fixture::new().await;
    f.github_state("completed", Some("failure"), 0);
    let result = f.schedule(f.github_condition()).await;
    assert_eq!(result["deferred"], false);
    assert_eq!(result["observation"]["conclusion"], "failure");
    assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
    // The additive marker and reason live on the event; the public observation is unchanged.
    assert!(result["observation"].get("immediate").is_none());
    assert!(result["observation"].get("reason").is_none());
    let run = f.run.clone();
    let ready =
        f.s.store
            .read(move |db| {
                Ok(db
                    .events(&run, 0, 100)?
                    .into_iter()
                    .filter(|event| event["type"] == "chat.wait_ready")
                    .map(|event| (event["text"].clone(), event["payload"].clone()))
                    .collect::<Vec<_>>())
            })
            .await
            .unwrap();
    let mut announced = result["observation"].clone();
    announced["immediate"] = json!(true);
    announced["reason"] = json!("Wait for result");
    assert_eq!(
        ready,
        [(json!("Wait condition already completed"), announced)]
    );
}

#[tokio::test]
async fn github_monitor_pins_identity_and_wakes_on_failure_without_polling_the_llm() {
    let f = Fixture::new().await;
    let wait = f.schedule(f.github_condition()).await;
    let repeated = wait_for::schedule(&f.s, &f.token, &json!({
        "condition": { "type": "github_workflow", "projectId": f.project, "runId": 42.0, "headSha": SHA },
        "timeoutSeconds": 420.0, "reason": "Wait for result",
    })).await.unwrap();
    assert_eq!(wait, repeated);
    assert_eq!(f.calls().len(), 1);
    f.finish().await;
    conditions::poll_once(&f.s).await.unwrap();
    assert_eq!(f.calls().len(), 2);
    assert_eq!(f.tick().await["run"]["status"], "succeeded");
    conditions::poll_once(&f.s).await.unwrap();
    assert_eq!(
        f.calls().len(),
        2,
        "Polling respects the persisted interval"
    );
    f.github_state("completed", Some("cancelled"), 0);
    f.next_poll().await;
    let restarted = f.restart().await;
    conditions::poll_once(&restarted).await.unwrap();
    let result = restarted.chat_detail(&f.chat).await.unwrap()["wakeup"]["observation"].clone();
    assert_eq!(result["conclusion"], "cancelled");
    assert_eq!(result["headSha"], SHA);
    restarted.chat_tick(&HashSet::new()).await.unwrap();
    assert!(
        text(
            &f.s.store.run(&f.run).await.unwrap()["chatExecution"],
            "text"
        )
        .contains("cancelled")
    );
    assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
    for call in f.calls() {
        assert_eq!(call["path"], "repos/fixture/repo/actions/runs/42");
        assert_eq!(call["token"], "fixture-shared-token");
    }
    assert_eq!(wait["wakeup"]["display"]["short"], "GitHub workflow");
    assert_eq!(result["display"]["tone"], "warning");
    assert_eq!(result["pluginId"], "github");
}

#[tokio::test]
async fn github_monitor_only_probes_resumable_wakeups() {
    let f = Fixture::new().await;
    f.schedule(f.github_condition()).await;
    f.finish().await;
    let chat = f.s.get("chats", &f.chat).await.unwrap();
    let run = f.s.store.run(&f.run).await.unwrap();

    for (field, value) in [
        ("paused", json!(true)),
        ("sessionRestartRequested", json!(true)),
        ("lifecycle", json!("trash")),
    ] {
        let mut blocked = chat.clone();
        blocked[field] = value;
        f.s.store.put("chats", blocked.clone()).await.unwrap();
        conditions::poll_once(&f.s).await.unwrap();
        assert_eq!(f.calls().len(), 1, "chat guard: {field}");
        assert_eq!(f.s.get("chats", &f.chat).await.unwrap(), blocked);
    }
    f.s.store.put("chats", chat.clone()).await.unwrap();

    for (field, value) in [
        ("status", json!("running")),
        ("recoveryPending", json!(true)),
        ("cancelRequestedAt", json!(1)),
        ("workspaceCleanedAt", json!(1)),
        ("chatExecution", json!({ "messageId": "another-turn" })),
    ] {
        f.s.store
            .patch_run(&f.run, json!({ (field): value }))
            .await
            .unwrap();
        conditions::poll_once(&f.s).await.unwrap();
        assert_eq!(f.calls().len(), 1, "run guard: {field}");
        assert_eq!(f.s.get("chats", &f.chat).await.unwrap(), chat);
        f.s.store
            .patch_run(&f.run, json!({ (field): run[field] }))
            .await
            .unwrap();
    }

    // The original, still-resumable wait remains observable after the guards clear.
    f.github_state("completed", Some("success"), 0);
    conditions::poll_once(&f.s).await.unwrap();
    assert_eq!(f.calls().len(), 2);
    assert_eq!(
        f.s.get("chats", &f.chat).await.unwrap()["wakeup"]["observation"]["conclusion"],
        "success"
    );
}

#[tokio::test]
async fn github_monitor_rechecks_session_restart_after_the_probe() {
    let f = Fixture::new().await;
    f.schedule(f.github_condition()).await;
    f.finish().await;
    f.github_state("completed", Some("success"), 0);
    let state_path = f.root.path().join("github-state.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    state["waitForRelease"] = true.into();
    std::fs::write(state_path, state.to_string()).unwrap();
    let s = f.s.clone();
    let observer = tokio::spawn(async move { conditions::poll_once(&s).await });
    common::eventually(
        Duration::from_secs(3),
        Duration::from_millis(10),
        || async { (f.calls().len() == 2).then_some(()) },
    )
    .await;

    // Preserve the wakeup deliberately to isolate the eligibility guard from
    // new-session's normal cancellation of the wakeup and pause of the chat.
    let mut chat = f.s.get("chats", &f.chat).await.unwrap();
    chat["sessionRestartRequested"] = true.into();
    f.s.store.put("chats", chat.clone()).await.unwrap();
    std::fs::write(f.root.path().join("github-release"), "").unwrap();
    observer.await.unwrap().unwrap();
    assert_eq!(f.s.get("chats", &f.chat).await.unwrap(), chat);
    assert_eq!(f.calls().len(), 2);
}

#[tokio::test]
async fn pending_conditions_time_out_and_pause_holds_the_same_deadline() {
    for github in [false, true] {
        let f = Fixture::new().await;
        let condition = if github {
            f.github_condition()
        } else {
            json!({ "type": "run_finished", "runId": f.target().await["id"] })
        };
        f.schedule(condition).await;
        f.finish().await;
        f.s.chat_pause(&f.chat, true).await.unwrap();
        f.due().await;
        assert_eq!(f.tick().await["run"]["status"], "succeeded");
        f.s.chat_pause(&f.chat, false).await.unwrap();
        let result = f.tick().await;
        assert_eq!(result["run"]["status"], "queued");
        assert!(text(&result["run"]["chatExecution"], "text").contains("timed_out"));
        assert!(result["wakeup"].is_null());
    }
}

#[tokio::test]
async fn late_github_results_cannot_restore_a_wait_replaced_by_user_input() {
    let f = Fixture::new().await;
    f.schedule(f.github_condition()).await;
    f.finish().await;
    f.github_state("completed", Some("success"), 350);
    let s = f.s.clone();
    let observer = tokio::spawn(async move { conditions::poll_once(&s).await });
    common::eventually(
        Duration::from_secs(3),
        Duration::from_millis(10),
        || async { (f.calls().len() == 2).then_some(()) },
    )
    .await;
    f.s.chat_send(&f.chat, json!({ "id": id(), "text": "Change the plan" }))
        .await
        .unwrap();
    observer.await.unwrap().unwrap();
    assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
    let detail = f.tick().await;
    assert_eq!(detail["run"]["chatExecution"]["text"], "Change the plan");
}

#[tokio::test]
async fn transient_github_errors_preserve_the_wait_and_never_expose_cli_output() {
    let f = Fixture::new().await;
    f.schedule(f.github_condition()).await;
    f.finish().await;
    std::fs::write(f.root.path().join("github-state.json"), "{\"error\":true}").unwrap();
    conditions::poll_once(&f.s).await.unwrap();
    let failed = f.s.chat_detail(&f.chat).await.unwrap();
    assert!(failed["wakeup"].is_object());
    assert!(failed["wakeup"]["lastError"].is_string());
    assert!(!failed.to_string().contains("fixture-secret"));
    assert_eq!(f.tick().await["run"]["status"], "succeeded");
    // GitHub answers again, without a result yet: the conversation no longer reports an outage.
    f.github_state("in_progress", None, 0);
    f.next_poll().await;
    conditions::poll_once(&f.s).await.unwrap();
    let recovered = f.s.chat_detail(&f.chat).await.unwrap();
    assert!(recovered["wakeup"].is_object());
    assert!(recovered["wakeup"].get("lastError").is_none());
    f.github_state("completed", Some("success"), 0);
    f.next_poll().await;
    conditions::poll_once(&f.s).await.unwrap();
    assert_eq!(f.tick().await["run"]["status"], "queued");
}

#[tokio::test]
async fn permissions_origins_and_workflow_commit_mismatches_are_rechecked() {
    let f = Fixture::new().await;
    let mut invalid = f.github_condition();
    invalid["headSha"] = "2222222222222222222222222222222222222222".into();
    assert!(
        wait_for::schedule(
            &f.s,
            &f.token,
            &json!({ "condition": invalid, "timeoutSeconds": 420, "reason": "Wrong commit" })
        )
        .await
        .is_err()
    );
    assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
    f.schedule(f.github_condition()).await;
    f.finish().await;
    let calls = f.calls().len();
    let mut project = f.s.get("projects", &f.project).await.unwrap();
    project["origin"] = "https://github.com/another/repository.git".into();
    f.s.store.put("projects", project).await.unwrap();
    conditions::poll_once(&f.s).await.unwrap();
    assert_eq!(f.calls().len(), calls);
    assert_eq!(
        f.s.chat_detail(&f.chat).await.unwrap()["wakeup"]["observation"]["state"],
        "unavailable"
    );

    // Plugin grants are live: withdrawing the instance ends the wait without a query.
    let f = Fixture::new().await;
    f.schedule(f.github_condition()).await;
    f.finish().await;
    connections::set_enabled(&f.s, "github", &f.instance, &json!({ "enabled": false }))
        .await
        .unwrap();
    conditions::poll_once(&f.s).await.unwrap();
    assert_eq!(f.calls().len(), 1);
    assert_eq!(
        f.s.chat_detail(&f.chat).await.unwrap()["wakeup"]["observation"]["state"],
        "unavailable"
    );
    let resumed = f.tick().await;
    assert_eq!(resumed["run"]["status"], "queued");
    assert!(text(&resumed["run"]["chatExecution"], "text").contains("unavailable"));
}

#[tokio::test]
async fn delay_replaces_condition_waits_and_private_tokens_follow_agent_identity() {
    let f = Fixture::new().await;
    connections::remove(&f.s, "github", &f.instance)
        .await
        .unwrap();
    github::instance(&f.s, "Main's own", "fixture-only-token", &[MAIN_AGENT_ID]).await;
    let wait = f.schedule(f.github_condition()).await;
    assert_eq!(f.calls()[0]["token"], "fixture-only-token");
    let replaced = wait_for::schedule(
        &f.s,
        &f.token,
        &json!({ "condition": { "type": "delay", "seconds": 420 }, "reason": "Wait for result" }),
    )
    .await
    .unwrap()["wakeup"]
        .clone();
    assert!(replaced["condition"].is_null());
    assert_ne!(replaced["id"], wait["wakeup"]["id"]);

    let target = f.target().await;
    let mut other = target.clone();
    other["snapshot"]["agent"]["id"] = id().into();
    f.s.store
        .patch_run(
            text(&target, "id"),
            json!({ "snapshot": other["snapshot"] }),
        )
        .await
        .unwrap();
    let result = wait_for::schedule(
        &f.s,
        &f.token,
        &json!({
            "condition": { "type": "run_finished", "runId": target["id"] },
            "timeoutSeconds": 420,
            "reason": "Observe another agent",
        }),
    )
    .await;
    assert_eq!(result.unwrap()["deferred"], true);
}

#[tokio::test]
async fn deferred_target_conversations_are_not_finished_and_cycles_are_rejected() {
    let f = Fixture::new().await;
    let other = f.s.chat_create(json!({})).await.unwrap();
    let other_id = text(&other, "id");
    f.s.chat_send(
        other_id,
        json!({ "id": id(), "text": "Other conversation" }),
    )
    .await
    .unwrap();
    f.s.chat_tick(&HashSet::new()).await.unwrap();
    let target = f.s.chat_detail(other_id).await.unwrap()["run"].clone();
    f.s.chat_acknowledge(
        text(&target, "id"),
        text(&target["chatExecution"], "messageId"),
    )
    .await
    .unwrap();
    let mut other = f.s.get("chats", other_id).await.unwrap();
    other["wakeup"] = json!({
        "id": id(), "messageId": target["chatExecution"]["messageId"],
        "condition": { "type": "run_finished", "runId": f.run },
        "wakeAt": now() + 420000,
    });
    f.s.store.put("chats", other.clone()).await.unwrap();
    f.s.store
        .patch_run(text(&target, "id"), json!({ "status": "succeeded" }))
        .await
        .unwrap();
    let input = json!({ "condition": { "type": "run_finished", "runId": target["id"] }, "timeoutSeconds": 420, "reason": "Join other work" });
    assert_eq!(
        wait_for::schedule(&f.s, &f.token, &input)
            .await
            .unwrap_err()
            .status,
        409
    );
    other["wakeup"]["condition"] = Value::Null;
    f.s.store.put("chats", other).await.unwrap();
    assert_eq!(
        wait_for::schedule(&f.s, &f.token, &input).await.unwrap()["deferred"],
        true
    );
}

#[tokio::test]
async fn input_validation_self_waits_and_pending_steering_cannot_register_wakeups() {
    let f = Fixture::new().await;
    for condition in [
        json!({ "type": "http", "url": "http://localhost/" }),
        json!({ "type": "run_finished", "runId": f.run }),
        json!({ "type": "run_finished", "runId": "not-a-uuid" }),
        json!({ "type": "github_workflow", "projectId": id(), "runId": 42, "headSha": SHA }),
        json!({ "type": "github_workflow", "projectId": f.project, "runId": 42, "headSha": "main" }),
    ] {
        assert!(
            wait_for::schedule(
                &f.s,
                &f.token,
                &json!({ "condition": condition, "reason": "Wait", "timeoutSeconds": 420 })
            )
            .await
            .is_err()
        );
    }
    for seconds in [json!(0), json!(86401), json!(1.5), json!("420")] {
        assert!(wait_for::schedule(&f.s, &f.token, &json!({ "condition": f.github_condition(), "reason": "Wait", "timeoutSeconds": seconds })).await.is_err());
    }
    f.s.chat_send(
        &f.chat,
        json!({ "id": id(), "text": "Fresh input", "mode": "steer" }),
    )
    .await
    .unwrap();
    assert!(
        wait_for::schedule(
            &f.s,
            &f.token,
            &json!({ "condition": f.github_condition(), "reason": "Wait", "timeoutSeconds": 420 })
        )
        .await
        .is_err()
    );
    assert_eq!(f.calls().len(), 0);
    assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
}

#[tokio::test]
async fn queued_follow_ups_do_not_prevent_registering_a_wait() {
    let f = Fixture::new().await;
    f.github_state("in_progress", None, 0);
    f.s.chat_send(
        &f.chat,
        json!({ "id": id(), "text": "Afterwards, tidy up" }),
    )
    .await
    .unwrap();
    let result = wait_for::schedule(
        &f.s,
        &f.token,
        &json!({ "condition": f.github_condition(), "reason": "Wait for CI", "timeoutSeconds": 420 }),
    )
    .await
    .unwrap();
    assert_eq!(result["deferred"], true);
    f.finish().await;

    let held = f.tick().await;
    assert_eq!(held["run"]["status"], "succeeded");
    assert!(held["wakeup"]["condition"].is_object());
}

#[tokio::test]
async fn newly_queued_input_during_the_initial_probe_cannot_be_held_by_a_wait() {
    for replacing_wakeup in [false, true] {
        let f = Fixture::new().await;
        let earlier = id();
        f.s.chat_send(
            &f.chat,
            json!({ "id": earlier, "text": "Earlier follow-up" }),
        )
        .await
        .unwrap();
        if replacing_wakeup {
            wait_for::schedule(
                &f.s,
                &f.token,
                &json!({ "condition": { "type": "delay", "seconds": 420 }, "reason": "Earlier wait" }),
            )
            .await
            .unwrap();
        }
        f.github_state("in_progress", None, 350);
        let s = f.s.clone();
        let token = f.token.clone();
        let condition = f.github_condition();
        let request = tokio::spawn(async move {
            wait_for::schedule(
                &s,
                &token,
                &json!({
                    "condition": condition,
                    "reason": "Old plan",
                    "timeoutSeconds": 420,
                }),
            )
            .await
        });
        common::eventually(
            Duration::from_secs(3),
            Duration::from_millis(10),
            || async { (!f.calls().is_empty()).then_some(()) },
        )
        .await;
        f.s.chat_send(
            &f.chat,
            json!({ "id": id(), "text": "New instruction during the probe" }),
        )
        .await
        .unwrap();

        assert_eq!(request.await.unwrap().unwrap_err().status, 409);
        assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
        f.finish().await;
        assert_eq!(f.tick().await["run"]["chatExecution"]["messageId"], earlier);
    }
}

#[tokio::test]
async fn delivered_steering_input_during_the_initial_probe_cannot_be_overwritten() {
    let f = Fixture::new().await;
    f.github_state("in_progress", None, 350);
    let s = f.s.clone();
    let token = f.token.clone();
    let condition = f.github_condition();
    let request = tokio::spawn(async move {
        wait_for::schedule(
            &s,
            &token,
            &json!({
                "condition": condition, "reason": "Old plan", "timeoutSeconds": 420,
            }),
        )
        .await
    });
    common::eventually(
        Duration::from_secs(3),
        Duration::from_millis(10),
        || async { (!f.calls().is_empty()).then_some(()) },
    )
    .await;
    let message = id();
    f.s.chat_send(
        &f.chat,
        json!({ "id": message, "text": "Change the plan", "mode": "steer" }),
    )
    .await
    .unwrap();
    f.s.chat_acknowledge(&f.run, &message).await.unwrap();
    assert_eq!(request.await.unwrap().unwrap_err().status, 409);
    assert!(f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].is_null());
}

#[tokio::test]
async fn watchers_share_requests_only_for_the_same_granted_project_origin() {
    for origin_changed in [false, true] {
        let f = Fixture::new().await;
        f.schedule(f.github_condition()).await;
        f.finish().await;
        if origin_changed {
            let mut project = f.s.get("projects", &f.project).await.unwrap();
            project["origin"] = "git@github.com:fixture/repo.git".into();
            f.s.store.put("projects", project).await.unwrap();
        }
        let second = f.s.chat_create(json!({})).await.unwrap();
        let chat = text(&second, "id");
        let message = id();
        f.s.chat_send(
            chat,
            json!({ "id": message, "text": "Another conversation" }),
        )
        .await
        .unwrap();
        f.s.chat_tick(&HashSet::from([f.run.clone()]))
            .await
            .unwrap();
        let mut run = f.s.chat_detail(chat).await.unwrap()["run"].clone();
        run =
            f.s.store
                .patch_run(text(&run, "id"), json!({ "status": "running" }))
                .await
                .unwrap();
        f.s.chat_acknowledge(text(&run, "id"), &message)
            .await
            .unwrap();
        let config = f.s.mcps.run_configuration(&f.s, &run).await.unwrap();
        let second_wait = wait_for::schedule(
            &f.s,
            text(&config["env"], "CAIRN_MCP_RUN_TOKEN"),
            &json!({
                "condition": f.github_condition(), "reason": "Also waiting", "timeoutSeconds": 600,
            }),
        )
        .await
        .unwrap();
        f.s.store
            .patch_run(text(&run, "id"), json!({ "status": "succeeded" }))
            .await
            .unwrap();
        assert_eq!(f.calls().len(), 2);
        f.github_state("completed", Some("success"), 0);
        conditions::poll_once(&f.s).await.unwrap();
        assert_eq!(f.calls().len(), 3);
        let first = f.s.chat_detail(&f.chat).await.unwrap()["wakeup"].clone();
        let second = f.s.chat_detail(chat).await.unwrap()["wakeup"].clone();
        if origin_changed {
            assert_eq!(first["observation"]["state"], "unavailable");
            assert_eq!(second["observation"]["state"], "completed");
        } else {
            assert_eq!(first["observation"], second["observation"]);
        }
        assert_ne!(first["id"], second["id"]);
        assert_eq!(second["wakeAt"], second_wait["wakeup"]["wakeAt"]);
    }
}

#[tokio::test]
async fn all_existing_cancellation_paths_also_invalidate_condition_waits() {
    for action in ["message", "cancel", "trash", "failure", "new_session"] {
        let f = Fixture::new().await;
        f.schedule(f.github_condition()).await;
        f.finish().await;
        match action {
            "message" => {
                f.s.chat_send(&f.chat, json!({ "id": id(), "text": "Change of plan" }))
                    .await
                    .unwrap();
            }
            "cancel" => f.s.worker.cancel(&f.s, &f.run).await.unwrap(),
            "trash" => {
                f.s.chat_trash(&f.chat, true).await.unwrap();
                assert_eq!(f.s.chat_restore(&f.chat).await.unwrap_err().status, 410);
            }
            "failure" => {
                f.s.chat_pause(&f.chat, true).await.unwrap();
                f.s.store
                    .patch_run(&f.run, json!({ "status": "failed" }))
                    .await
                    .unwrap();
            }
            "new_session" => {
                let mut chat = f.s.get("chats", &f.chat).await.unwrap();
                chat["restoredAt"] = now().into();
                f.s.store.put("chats", chat).await.unwrap();
                f.s.store
                    .patch_run(&f.run, json!({ "status": "failed" }))
                    .await
                    .unwrap();
                f.s.chat_new_session(&f.chat, true).await.unwrap();
            }
            _ => unreachable!(),
        }
        conditions::poll_once(&f.s).await.unwrap();
        assert!(f.tick().await["wakeup"].is_null(), "{action}");
        assert_eq!(
            f.calls().len(),
            1,
            "{action}: a cancelled wait must not query GitHub"
        );
    }
}

#[tokio::test]
async fn git_suffixes_and_installation_grant_flags_cannot_change_the_target_scope() {
    let f = Fixture::with_origin("https://github.com/fixture/repo.git.git").await;
    f.schedule(f.github_condition()).await;
    assert_eq!(
        f.calls()[0]["path"],
        "repos/fixture/repo.git/actions/runs/42"
    );
    let target = f.target().await;
    let key = format!(
        "mcp-grant:{}",
        cairn_installation::auth::hex_digest(&f.token)
    );
    let mut grant = f.s.store.kv(&key).await.unwrap().unwrap();
    grant["installation"] = false.into();
    f.s.store.set(&key, grant, None).await.unwrap();
    let result = wait_for::schedule(
        &f.s,
        &f.token,
        &json!({
            "condition": { "type": "run_finished", "runId": target["id"] },
            "reason": "Narrower grant", "timeoutSeconds": 420,
        }),
    )
    .await;
    assert_eq!(result.unwrap_err().status, 401);
}

#[tokio::test]
async fn projects_registered_with_ssh_remotes_can_wait_for_their_ci() {
    // Cairn stores SSH remotes without their user: `git@github.com:o/r.git` becomes
    // `github.com:o/r.git` and `ssh://git@github.com/…` becomes `ssh://github.com/…`.
    for origin in [
        "github.com:fixture/repo.git",
        "ssh://github.com/fixture/repo.git",
        "git@github.com:fixture/repo.git",
    ] {
        let f = Fixture::with_origin(origin).await;
        let wait = f.schedule(f.github_condition()).await;
        assert_eq!(wait["deferred"], true, "{origin}: {wait}");
        assert_eq!(
            f.calls()[0]["path"],
            "repos/fixture/repo/actions/runs/42",
            "{origin}"
        );
    }
    let f = Fixture::with_origin("https://gitlab.com/fixture/repo.git").await;
    let result = wait_for::schedule(
        &f.s,
        &f.token,
        &json!({ "condition": f.github_condition(), "reason": "Elsewhere", "timeoutSeconds": 420 }),
    )
    .await;
    assert_eq!(result.unwrap_err().status, 409);
    assert!(f.calls().is_empty());
}

#[tokio::test]
async fn scoped_agent_token_removal_never_falls_back_to_shared_credentials() {
    let f = Fixture::new().await;
    let agent =
        f.s.agent(
            json!({
                "name": "Scoped watcher",
                "access": { "projects": [f.project], "installation": true },
            }),
            None,
        )
        .await
        .unwrap();
    let dedicated =
        github::instance(&f.s, "Scoped", "fixture-only-token", &[text(&agent, "id")]).await;
    let chat =
        f.s.chat_create(json!({ "agentId": agent["id"] }))
            .await
            .unwrap();
    let chat_id = text(&chat, "id");
    let message = id();
    f.s.chat_send(
        chat_id,
        json!({ "id": message, "text": "Wait for allowed CI" }),
    )
    .await
    .unwrap();
    f.s.chat_tick(&HashSet::new()).await.unwrap();
    let run = f.s.chat_detail(chat_id).await.unwrap()["run"].clone();
    f.s.store
        .patch_run(text(&run, "id"), json!({ "status": "running" }))
        .await
        .unwrap();
    f.s.chat_acknowledge(text(&run, "id"), &message)
        .await
        .unwrap();
    let config = f.s.mcps.run_configuration(&f.s, &run).await.unwrap();
    let result = wait_for::schedule(
        &f.s,
        text(&config["env"], "CAIRN_MCP_RUN_TOKEN"),
        &json!({
            "condition": f.github_condition(), "reason": "Scoped CI", "timeoutSeconds": 420,
        }),
    )
    .await
    .unwrap();
    assert_eq!(result["deferred"], true);
    assert_eq!(f.calls()[0]["token"], "fixture-only-token");
    assert!(!result.to_string().contains("fixture-only-token"));
    f.s.store
        .patch_run(text(&run, "id"), json!({ "status": "succeeded" }))
        .await
        .unwrap();
    connections::remove(&f.s, "github", &dedicated)
        .await
        .unwrap();
    conditions::poll_once(&f.s).await.unwrap();
    assert_eq!(f.calls().len(), 1);
    assert_eq!(
        f.s.chat_detail(chat_id).await.unwrap()["wakeup"]["observation"]["state"],
        "unavailable"
    );
}

fn milliseconds(samples: &mut [f64]) -> Value {
    samples.sort_by(f64::total_cmp);
    json!({
        "meanMs": samples.iter().sum::<f64>() / samples.len() as f64,
        "medianMs": samples[samples.len() / 2],
        "p95Ms": samples[(samples.len() * 95).div_ceil(100) - 1],
        "samples": samples.len(),
    })
}

fn process_cpu_ms() -> f64 {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
        0
    );
    time.tv_sec as f64 * 1000.0 + time.tv_nsec as f64 / 1_000_000.0
}

fn child_cpu_ms() -> f64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) },
        0
    );
    let millis = |time: libc::timeval| time.tv_sec as f64 * 1000.0 + time.tv_usec as f64 / 1000.0;
    millis(usage.ru_utime) + millis(usage.ru_stime)
}

#[tokio::test]
#[ignore = "reproducible local metadata measurements; run explicitly without other tests"]
async fn measure_wait_metadata() {
    for mode in ["delay", "at", "run_finished", "github_pr_checks"] {
        let f = Fixture::new().await;
        let target = f.target().await;
        if mode == "github_pr_checks" {
            pr_checks::pending(&f);
        }
        let mut writes = Vec::new();
        let cpu_start = process_cpu_ms();
        let child_cpu_start = child_cpu_ms();
        for i in 0..100 {
            let reason = format!("Measurement {i}");
            let started = std::time::Instant::now();
            if !matches!(mode, "delay" | "at") {
                let condition = if mode == "run_finished" {
                    json!({ "type": "run_finished", "runId": target["id"] })
                } else {
                    json!({ "type": "github_pr_checks", "projectId": f.project, "prNumber": 7, "headSha": SHA })
                };
                wait_for::schedule(
                    &f.s,
                    &f.token,
                    &json!({
                        "condition": condition,
                        "reason": reason,
                        "timeoutSeconds": 420,
                    }),
                )
                .await
                .unwrap();
            } else {
                let condition = if mode == "delay" {
                    json!({ "type": "delay", "seconds": 420 })
                } else {
                    json!({ "type": "at", "date": chrono::DateTime::from_timestamp_millis(now() + 420_000).unwrap().to_rfc3339() })
                };
                wait_for::schedule(
                    &f.s,
                    &f.token,
                    &json!({ "condition": condition, "reason": reason }),
                )
                .await
                .unwrap();
            }
            writes.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let write_cpu_ms = process_cpu_ms() - cpu_start;
        let child_cpu_ms = child_cpu_ms() - child_cpu_start;
        let chat = f.s.get("chats", &f.chat).await.unwrap();
        let mut warm = Vec::new();
        for _ in 0..500 {
            let started = std::time::Instant::now();
            std::hint::black_box(f.s.get("chats", &f.chat).await.unwrap());
            warm.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let mut reopened = Vec::new();
        let mut pool_open = Vec::new();
        for _ in 0..20 {
            let started = std::time::Instant::now();
            let store = cairn_installation::store::Store::open(&f.s.config.data_dir).unwrap();
            pool_open.push(started.elapsed().as_secs_f64() * 1000.0);
            let started = std::time::Instant::now();
            std::hint::black_box(store.get("chats", &f.chat).await.unwrap());
            reopened.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let index_bytes =
            f.s.store
                .read(|db| {
                    Ok(db
                        .0
                        .query_row(
                            "SELECT SUM(pgsize) FROM dbstat WHERE name='events_user_input'",
                            [],
                            |row| row.get::<_, Option<i64>>(0),
                        )
                        .ok()
                        .flatten())
                })
                .await
                .unwrap();
        println!(
            "WAIT_FOR_MEASUREMENT {}",
            json!({
                "mode": mode,
                "chatEncodedBytes": serde_json::to_vec(&chat).unwrap().len(),
                "wakeupEncodedBytes": serde_json::to_vec(&chat["wakeup"]).unwrap().len(),
                "write": milliseconds(&mut writes),
                "writeProcessCpuMs": write_cpu_ms,
                "writeChildCpuMs": child_cpu_ms,
                "externalProbeRequests": f.calls().len(),
                "warmRead": milliseconds(&mut warm),
                "firstReadFreshPool": milliseconds(&mut reopened),
                "freshPoolOpen": milliseconds(&mut pool_open),
                "userInputIndexBytes": index_bytes,
                "availableCpuThreads": std::thread::available_parallelism().unwrap().get(),
            })
        );
    }
}

#[tokio::test]
async fn dormant_plugin_probes_keep_the_requesters_membership_generation() {
    use cairn_installation::auth::{InstallationIdentity, InstallationRole};
    use std::sync::atomic::{AtomicBool, Ordering};

    for during_request in [false, true] {
        let f = Fixture::with_identity(
            "https://github.com/fixture/repo.git",
            Some(InstallationIdentity::trusted(
                InstallationRole::Owner,
                "fixture-owner",
            )),
        )
        .await;
        f.schedule(f.github_condition()).await;
        f.finish().await;
        let revoked = Arc::new(AtomicBool::new(false));
        let state = revoked.clone();
        let authority = axum::Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(move || {
                let state = state.clone();
                async move {
                    axum::Json(cairn_protocol::TaskAuthorPolicy {
                        owner: cairn_protocol::TaskAuthorGrant {
                            account_id: "fixture-owner".into(),
                            access_id: if state.load(Ordering::SeqCst) {
                                "replacement-grant"
                            } else {
                                "fixture-owner"
                            }
                            .into(),
                        },
                        members: Vec::new(),
                    })
                }
            }),
        );
        let (origin, authority) = common::serve_locally(authority).await;
        let path = f.s.config.data_dir.join("installation-relay/identity.json");
        let mut identity: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        identity["origin"] = origin.into();
        std::fs::write(path, serde_json::to_vec(&identity).unwrap()).unwrap();
        f.github.state(&json!({
            "waitForRelease": during_request,
            "response": { "id": 42, "head_sha": SHA, "status": "completed", "conclusion": "success", "run_attempt": 1 },
        }));
        if during_request {
            let service = f.s.clone();
            let observer = tokio::spawn(async move { conditions::poll_once(&service).await });
            common::eventually(
                Duration::from_secs(3),
                Duration::from_millis(10),
                || async { (f.calls().len() == 2).then_some(()) },
            )
            .await;
            revoked.store(true, Ordering::SeqCst);
            std::fs::write(f.root.path().join("github-release"), "").unwrap();
            observer.await.unwrap().unwrap();
        } else {
            revoked.store(true, Ordering::SeqCst);
            conditions::poll_once(&f.s).await.unwrap();
        }
        if during_request {
            // An unhandled capability refusal is a failed worker invocation.
            // It must deliver no provider result; the next poll then observes
            // the revoked source before making another provider request.
            let pending = f.s.get("chats", &f.chat).await.unwrap();
            assert!(pending["wakeup"]["observation"].is_null());
            f.next_poll().await;
            conditions::poll_once(&f.s).await.unwrap();
        }
        let chat = f.s.get("chats", &f.chat).await.unwrap();
        assert_eq!(chat["wakeup"]["observation"]["state"], "unavailable");
        assert_eq!(f.calls().len(), if during_request { 2 } else { 1 });
        authority.abort();
    }
}

#[tokio::test]
async fn cancelling_the_shared_probe_representative_keeps_other_waits_pending() {
    use cairn_installation::{
        auth::{InstallationIdentity, InstallationRole},
        plugins::registry,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    let identity = InstallationIdentity::trusted(InstallationRole::Owner, "fixture-owner");
    let f = Fixture::with_identity(
        "https://github.com/fixture/repo.git",
        Some(identity.clone()),
    )
    .await;
    let package = registry::install(&f.s, &json!({
        "manifest": {
            "id": "probe-fixture", "name": "Probe fixture", "version": "1", "apiVersion": 2,
            "capabilities": ["storage"],
            "settings": [{"key":"ready", "title":"Ready", "type":"boolean", "default":false}],
            "agentAccess": {"description":"Read a synthetic condition."},
            "conditions": [{"name":"ready", "description":"Wait for a synthetic result.", "inputSchema":{"type":"object","properties":{},"additionalProperties":false}}],
        },
        "source": "return {condition = function(event) if not event.instances[1].settings.ready then return {state = 'waiting'} end; cairn.storage.set('result-ready', true); return {state = 'completed'} end}",
    })).await.unwrap();
    registry::approve(&f.s, "probe-fixture", "1", text(&package, "digest"))
        .await
        .unwrap();
    let instance = connections::save(
        &f.s,
        "probe-fixture",
        &json!({
            "name":"Probe", "enabled":true, "agentIds":[MAIN_AGENT_ID], "settings":{"ready":false},
        }),
        None,
    )
    .await
    .unwrap();
    let condition = json!({"type":"probe-fixture_ready"});
    f.schedule(condition.clone()).await;
    f.finish().await;
    let second = f.s.chat_create(json!({})).await.unwrap();
    let second_chat = text(&second, "id").to_owned();
    let message = id();
    f.s.chat_send_as(
        &second_chat,
        json!({"id":message,"text":"Independently wait for the same result"}),
        Some(&identity),
    )
    .await
    .unwrap();
    f.s.chat_tick(&HashSet::from([f.run.clone()]))
        .await
        .unwrap();
    let run = f.s.chat_detail(&second_chat).await.unwrap()["run"].clone();
    let run =
        f.s.store
            .patch_run(text(&run, "id"), json!({"status":"running"}))
            .await
            .unwrap();
    f.s.chat_acknowledge(text(&run, "id"), &message)
        .await
        .unwrap();
    let config = f.s.mcps.run_configuration(&f.s, &run).await.unwrap();
    wait_for::schedule(
        &f.s,
        text(&config["env"], "CAIRN_MCP_RUN_TOKEN"),
        &json!({
            "condition":condition, "reason":"Independent wait", "timeoutSeconds":420,
        }),
    )
    .await
    .unwrap();
    f.s.store
        .patch_run(text(&run, "id"), json!({"status":"succeeded"}))
        .await
        .unwrap();
    connections::save(
        &f.s,
        "probe-fixture",
        &json!({
            "name":"Probe", "enabled":true, "settings":{"ready":true},
        }),
        Some(text(&instance, "id")),
    )
    .await
    .unwrap();
    // The stable durable catalog order determines the representative of the
    // identical conditions. Cancelling it must not cancel its independent peer.
    let representative =
        f.s.store
            .list("chats")
            .await
            .unwrap()
            .into_iter()
            .find(|chat| chat["id"] == f.chat || chat["id"] == second_chat)
            .unwrap();
    let representative_id = text(&representative, "id").to_owned();
    let other = if representative_id == f.chat {
        second_chat
    } else {
        f.chat.clone()
    };
    let held = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    let (service, holding, entering, releasing) =
        (f.s.clone(), held.clone(), entered.clone(), released.clone());
    let authority = axum::Router::new().route(
        "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
        axum::routing::get(move || {
            let (service, held, entered, released) = (
                service.clone(),
                holding.clone(),
                entering.clone(),
                releasing.clone(),
            );
            async move {
                let marker = service
                    .store
                    .list("plugin-storage")
                    .await
                    .unwrap()
                    .iter()
                    .any(|record| record["key"] == "result-ready");
                if marker && !held.swap(true, Ordering::SeqCst) {
                    // storage.set has completed. The next lookup belongs to the
                    // invocation's final recheck, outside worker-error mapping.
                    entered.notify_one();
                    released.notified().await;
                }
                axum::Json(cairn_protocol::TaskAuthorPolicy {
                    owner: cairn_protocol::TaskAuthorGrant {
                        account_id: "fixture-owner".into(),
                        access_id: "fixture-owner".into(),
                    },
                    members: Vec::new(),
                })
            }
        }),
    );
    let (origin, authority) = common::serve_locally(authority).await;
    let path = f.s.config.data_dir.join("installation-relay/identity.json");
    let mut identity: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    identity["origin"] = origin.into();
    std::fs::write(path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let service = f.s.clone();
    let observer = tokio::spawn(async move { conditions::poll_once(&service).await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    f.s.chat_pause(&representative_id, true).await.unwrap();
    released.notify_one();
    observer.await.unwrap().unwrap();
    let pending = f.s.get("chats", &other).await.unwrap();
    assert!(
        pending["wakeup"]["observation"].is_null(),
        "Another wait was permanently resolved: {}",
        pending["wakeup"]
    );
    conditions::poll_once(&f.s).await.unwrap();
    let resolved = f.s.get("chats", &other).await.unwrap();
    assert_eq!(
        resolved["wakeup"]["observation"]["state"], "completed",
        "Remaining wait: {}",
        resolved["wakeup"]
    );
    assert_eq!(
        f.s.get("chats", &representative_id).await.unwrap()["paused"],
        true
    );
    authority.abort();
}

#[tokio::test]
async fn mission_plugin_wait_survives_restart_and_resumes_on_the_pinned_result() {
    let f = Fixture::mission().await;
    let chats_before = f.s.store.list("chats").await.unwrap().len();
    let wait = f.schedule(f.github_condition()).await;
    assert_eq!(wait, f.schedule(f.github_condition()).await);
    f.park_mission().await;
    assert_eq!(f.mission_tick().await["status"], "waiting");
    let restarted = f.restart().await;
    restarted.worker.tick(&restarted).await.unwrap();
    assert_eq!(
        restarted.store.run(&f.run).await.unwrap()["wakeup"],
        wait["wakeup"]
    );
    f.github_state("completed", Some("failure"), 0);
    cairn_installation::plugins::conditions::poll_once(&restarted)
        .await
        .unwrap();
    assert_eq!(
        f.calls().len(),
        2,
        "The dormant mission must query its provider"
    );
    assert_eq!(
        restarted.store.list("chats").await.unwrap().len(),
        chats_before
    );
    restarted.worker.tick(&restarted).await.unwrap();
    let resumed = restarted.store.run(&f.run).await.unwrap();
    assert_eq!(resumed["status"], "queued");
    assert_eq!(
        resumed["waitContinuation"]["observation"]["conclusion"],
        "failure"
    );
    assert_eq!(resumed["missionTurnId"], wait["wakeup"]["id"]);
    assert!(resumed["wakeup"].is_null());
    let checkpoint = restarted
        .store
        .kv(&format!("run-checkpoint:{}", f.run))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint["remainingMs"], 90_000);
    assert_eq!(checkpoint["completed"], false);
    // Even a retained credential from the previous turn cannot act in the next one.
    restarted
        .store
        .patch_run(&f.run, json!({ "status": "running" }))
        .await
        .unwrap();
    assert!(
        wait_for::schedule(
            &restarted,
            &f.token,
            &json!({
                "condition": { "type": "delay", "seconds": 1 }, "reason": "Stale turn",
            })
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn mission_condition_observation_cannot_cross_a_replaced_turn() {
    let f = Fixture::mission().await;
    let wait = f.schedule(f.github_condition()).await;
    f.park_mission().await;
    f.github_state("completed", Some("success"), 0);
    let path = f.root.path().join("github-state.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["waitForRelease"] = true.into();
    std::fs::write(path, state.to_string()).unwrap();
    let s = f.s.clone();
    let observer = tokio::spawn(async move { conditions::poll_once(&s).await });
    common::eventually(
        Duration::from_secs(3),
        Duration::from_millis(10),
        || async { (f.calls().len() == 2).then_some(()) },
    )
    .await;

    // Keep the wakeup to isolate the issuing-turn fence from normal cancellation.
    let next_turn = id();
    f.s.store
        .patch_run(&f.run, json!({ "missionTurnId": next_turn }))
        .await
        .unwrap();
    std::fs::write(f.root.path().join("github-release"), "").unwrap();
    observer.await.unwrap().unwrap();
    let pending = f.s.store.run(&f.run).await.unwrap();
    assert_eq!(pending["status"], "waiting");
    assert_eq!(pending["missionTurnId"], next_turn);
    assert_eq!(pending["wakeup"], wait["wakeup"]);
    assert!(pending["wakeup"]["observation"].is_null());
}

#[tokio::test]
async fn mission_wait_is_cancellable_and_schedule_pause_only_affects_future_runs() {
    let f = Fixture::mission().await;
    let wait = wait_for::schedule(
        &f.s,
        &f.token,
        &json!({
            "condition": { "type": "delay", "seconds": 60 }, "reason": "Wait for the result",
        }),
    )
    .await
    .unwrap();
    f.park_mission().await;
    let run = f.s.store.run(&f.run).await.unwrap();
    let mut task = f.s.get("tasks", text(&run, "taskId")).await.unwrap();
    task["enabled"] = false.into();
    f.s.store.put("tasks", task).await.unwrap();
    assert_eq!(f.mission_tick().await["wakeup"], wait["wakeup"]);
    f.s.worker.cancel(&f.s, &f.run).await.unwrap();
    let cancelled = f.mission_tick().await;
    assert_eq!(cancelled["status"], "cancelled");
    assert!(cancelled["wakeup"].is_null());
}

#[tokio::test]
async fn missions_share_dependency_cycle_detection_with_conversations() {
    let f = Fixture::mission().await;
    let target = f.target().await;
    f.s.store
        .patch_run(
            text(&target, "id"),
            json!({
                "wakeup": { "condition": { "type": "run_finished", "runId": f.run } },
            }),
        )
        .await
        .unwrap();
    let error = wait_for::schedule(
        &f.s,
        &f.token,
        &json!({
            "condition": { "type": "run_finished", "runId": target["id"] },
            "reason": "Cyclic dependency", "timeoutSeconds": 60,
        }),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("cycle"), "{error:?}");
}

#[tokio::test]
async fn mission_dependency_wait_observes_completion_and_not_another_missions_wait() {
    let f = Fixture::mission().await;
    let target = f.target().await;
    f.s.store
        .patch_run(text(&target, "id"), json!({ "status": "waiting" }))
        .await
        .unwrap();
    let wait = f
        .schedule(json!({ "type": "run_finished", "runId": target["id"] }))
        .await;
    assert_eq!(wait["deferred"], true);
    f.park_mission().await;
    assert_eq!(f.mission_tick().await["status"], "waiting");
    f.s.store
        .patch_run(
            text(&target, "id"),
            json!({ "status": "failed", "finishedAt": now() }),
        )
        .await
        .unwrap();
    let resumed = f.mission_tick().await;
    assert_eq!(resumed["status"], "queued");
    assert_eq!(
        resumed["waitContinuation"]["observation"]["status"],
        "failed"
    );
}
