use super::*;
use crate::common::run_fixture::DbRunFixture;

impl Fixture {
    async fn group(&self, mode: &str, conditions: Vec<Value>) -> Result<Value, u16> {
        wait_for::schedule(
            &self.s,
            &self.token,
            &json!({
                "conditions": conditions, "mode": mode,
                "reason": "Join the parallel work", "timeoutSeconds": 420,
            }),
        )
        .await
        .map_err(|error| error.status)
    }

    async fn wakeup(&self) -> Value {
        self.s.chat_detail(&self.chat).await.unwrap()["wakeup"].clone()
    }

    async fn record_groups(&self) {
        wait_for::tick_groups(&self.s, &HashSet::new())
            .await
            .unwrap();
    }

    async fn set_status(&self, run: &Value, status: &str) {
        self.s
            .store
            .change_run(
                text(run, "id"),
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::parse(status).unwrap()),
                    finished_at: Some(Some(now())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    async fn ready_events(&self) -> Vec<Value> {
        let run = self.run.clone();
        self.s
            .store
            .read(move |db| {
                Ok(db
                    .events_fixture(&run, 0, 100)?
                    .into_iter()
                    .filter(|event| event["type"] == "chat.wait_ready")
                    .map(|event| event["payload"].clone())
                    .collect())
            })
            .await
            .unwrap()
    }
}

fn run_condition(run: &Value) -> Value {
    json!({ "type": "run_finished", "runId": run["id"] })
}

#[tokio::test]
async fn all_records_each_result_and_resumes_once_every_condition_has_one() {
    let f = Fixture::new().await;
    let (first, second) = (f.target().await, f.target().await);
    let conditions = vec![run_condition(&first), run_condition(&second)];
    let wait = f.group("all", conditions.clone()).await.unwrap();
    assert_eq!(wait["deferred"], true);
    assert_eq!(
        wait["wakeup"]["condition"],
        json!({ "type": "group", "mode": "all", "conditions": conditions })
    );
    assert_eq!(wait["wakeup"]["members"], json!([{}, {}]));
    assert_eq!(f.group("all", conditions.clone()).await.unwrap(), wait);
    f.finish().await;

    // A failure is a result too: it is recorded, and the group keeps waiting for the other.
    f.set_status(&first, "failed").await;
    f.record_groups().await;
    let wakeup = f.wakeup().await;
    assert_eq!(wakeup["members"][0]["observation"]["status"], "failed");
    assert!(wakeup["members"][1].get("observation").is_none());
    assert!(wakeup["observation"].is_null());
    assert!(f.ready_events().await.is_empty());
    assert_eq!(f.tick().await["run"]["status"], "succeeded");

    f.set_status(&second, "succeeded").await;
    f.record_groups().await;
    let observation = f.wakeup().await["observation"].clone();
    assert_eq!(observation["state"], "completed");
    assert_eq!(observation["type"], "group");
    assert_eq!(observation["mode"], "all");
    assert_eq!(observation["members"][0]["condition"], conditions[0]);
    assert_eq!(observation["members"][0]["result"]["status"], "failed");
    assert_eq!(observation["members"][1]["result"]["status"], "succeeded");
    assert_eq!(f.ready_events().await, std::slice::from_ref(&observation));

    let resumed = f.tick().await;
    assert_eq!(resumed["run"]["status"], "queued");
    let prompt = text(&resumed["run"]["chatExecution"], "text");
    assert!(prompt.contains(text(&first, "id")) && prompt.contains(text(&second, "id")));
    assert!(resumed["wakeup"].is_null());
    assert_eq!(f.calls().len(), 0, "Internal conditions never query GitHub");
}

#[tokio::test]
async fn any_resumes_at_the_first_result_and_leaves_the_others_without_one() {
    let f = Fixture::new().await;
    let first = f.target().await;
    let wait = f
        .group("any", vec![f.github_condition(), run_condition(&first)])
        .await
        .unwrap();
    assert_eq!(wait["deferred"], true);
    // The plugin condition is checked separately, with its own schedule and display.
    let members = &wait["wakeup"]["members"];
    assert!(members[0]["nextCheckAt"].is_number());
    assert!(members[1].get("nextCheckAt").is_none());
    assert!(wait["wakeup"].get("nextCheckAt").is_none());
    f.finish().await;
    assert_eq!(f.tick().await["run"]["status"], "succeeded");

    // Without waiting for progress to be recorded, the scheduler sees the first result.
    f.set_status(&first, "cancelled").await;
    let resumed = f.tick().await;
    assert_eq!(resumed["run"]["status"], "queued");
    let prompt = text(&resumed["run"]["chatExecution"], "text");
    assert!(prompt.contains(r#""mode":"any""#));
    assert!(prompt.contains(r#""result":null"#));
    assert!(prompt.contains(r#""status":"cancelled""#));
}

#[tokio::test]
async fn plugin_results_in_a_group_are_recorded_one_by_one() {
    let f = Fixture::new().await;
    let first = f.target().await;
    f.group("all", vec![f.github_condition(), run_condition(&first)])
        .await
        .unwrap();
    f.finish().await;
    let probes = f.calls().len();

    f.github_state("completed", Some("success"), 0);
    let mut chat = f.s.get("chats", &f.chat).await.unwrap();
    chat["wakeup"]["members"][0]["nextCheckAt"] = 0.into();
    f.s.store.put("chats", chat).await.unwrap();
    conditions::poll_once(&f.s).await.unwrap();
    assert_eq!(f.calls().len(), probes + 1);
    let wakeup = f.wakeup().await;
    assert_eq!(wakeup["members"][0]["observation"]["conclusion"], "success");
    assert!(wakeup["observation"].is_null());
    assert!(f.ready_events().await.is_empty());

    // A recorded result is never observed again.
    let mut chat = f.s.get("chats", &f.chat).await.unwrap();
    chat["wakeup"]["members"][0]["nextCheckAt"] = 0.into();
    f.s.store.put("chats", chat).await.unwrap();
    conditions::poll_once(&f.s).await.unwrap();
    assert_eq!(f.calls().len(), probes + 1);
    assert_eq!(f.tick().await["run"]["status"], "succeeded");

    f.set_status(&first, "succeeded").await;
    f.record_groups().await;
    let observation = f.wakeup().await["observation"].clone();
    assert_eq!(observation["state"], "completed");
    assert_eq!(observation["members"][0]["result"]["conclusion"], "success");
    assert_eq!(f.tick().await["run"]["status"], "queued");
}

#[tokio::test]
async fn the_group_deadline_reports_the_results_received_so_far() {
    let f = Fixture::new().await;
    let (first, second) = (f.target().await, f.target().await);
    f.group("all", vec![run_condition(&first), run_condition(&second)])
        .await
        .unwrap();
    f.finish().await;
    f.set_status(&first, "succeeded").await;
    f.due().await;
    let resumed = f.tick().await;
    assert_eq!(resumed["run"]["status"], "queued");
    let prompt = text(&resumed["run"]["chatExecution"], "text");
    assert!(prompt.contains(r#""state":"timed_out""#));
    assert!(prompt.contains(r#""status":"succeeded""#));
    assert!(prompt.contains(r#""result":null"#));
}

#[tokio::test]
async fn groups_already_complete_return_their_results_without_waiting() {
    let f = Fixture::new().await;
    let (first, second) = (f.target().await, f.target().await);
    f.set_status(&first, "succeeded").await;
    let conditions = vec![run_condition(&first), run_condition(&second)];

    let any = f.group("any", conditions.clone()).await.unwrap();
    assert_eq!(any["deferred"], false);
    assert_eq!(any["observation"]["state"], "completed");
    assert_eq!(
        any["observation"]["members"][0]["result"]["status"],
        "succeeded"
    );
    assert!(any["observation"]["members"][1]["result"].is_null());
    assert!(f.wakeup().await.is_null());

    // Only part of an `all` group is complete: it waits, keeping that result.
    let all = f.group("all", conditions.clone()).await.unwrap();
    assert_eq!(all["deferred"], true);
    assert_eq!(
        all["wakeup"]["members"][0]["observation"]["status"],
        "succeeded"
    );

    // Repeating the request keeps that wait; the last result then resumes it.
    assert_eq!(f.group("all", conditions).await.unwrap(), all);
    f.finish().await;
    f.set_status(&second, "failed").await;
    let resumed = f.tick().await;
    assert_eq!(resumed["run"]["status"], "queued");
    let prompt = text(&resumed["run"]["chatExecution"], "text");
    assert!(prompt.contains(r#""status":"succeeded""#) && prompt.contains(r#""status":"failed""#));
}

#[tokio::test]
async fn invalid_groups_are_refused() {
    let f = Fixture::new().await;
    let (first, second) = (f.target().await, f.target().await);
    let request = |conditions: Value, extra: Value| {
        let mut request = json!({
            "conditions": conditions, "mode": "all",
            "reason": "Join", "timeoutSeconds": 420,
        });
        for (key, value) in extra.as_object().unwrap() {
            if value.is_null() {
                request.as_object_mut().unwrap().remove(key);
            } else {
                request[key] = value.clone();
            }
        }
        request
    };
    let both = json!([run_condition(&first), run_condition(&second)]);
    for (input, status) in [
        (request(both.clone(), json!({ "mode": null })), 400),
        (request(both.clone(), json!({ "mode": "first" })), 400),
        (
            request(both.clone(), json!({ "timeoutSeconds": null })),
            400,
        ),
        (
            request(
                json!([run_condition(&first), { "type": "delay", "seconds": 60 }]),
                json!({}),
            ),
            400,
        ),
        (
            request(
                json!([run_condition(&first), run_condition(&first)]),
                json!({}),
            ),
            400,
        ),
        (request(json!([]), json!({})), 400),
        (
            request(
                json!([run_condition(&first), { "type": "run_finished", "runId": f.run }]),
                json!({}),
            ),
            400,
        ),
        (
            request(
                json!([run_condition(&first), { "type": "group", "mode": "any", "conditions": both }]),
                json!({}),
            ),
            400,
        ),
    ] {
        let result = wait_for::schedule(&f.s, &f.token, &input).await;
        assert_eq!(result.unwrap_err().status, status, "{input}");
    }
    assert!(f.wakeup().await.is_null());
}

#[tokio::test]
async fn cycles_through_grouped_dependencies_are_rejected() {
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
    let unrelated = f.target().await;
    let mut other = f.s.get("chats", other_id).await.unwrap();
    other["wakeup"] = json!({
        "id": id(), "messageId": target["chatExecution"]["messageId"],
        "condition": {
            "type": "group", "mode": "any",
            "conditions": [run_condition(&unrelated), { "type": "run_finished", "runId": f.run }],
        },
        "members": [{}, {}],
        "wakeAt": now() + 420_000,
    });
    f.s.store.put("chats", other).await.unwrap();
    f.s.store
        .change_run(
            text(&target, "id"),
            cairn_installation::runs::RunChanges {
                status: Some(cairn_contracts::RunStatus::Succeeded),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let input = vec![run_condition(&unrelated), run_condition(&target)];
    assert_eq!(f.group("all", input.clone()).await.unwrap_err(), 409);
    assert_eq!(
        f.group("all", vec![run_condition(&unrelated)])
            .await
            .unwrap()["deferred"],
        true
    );
}

#[tokio::test]
async fn initial_unavailable_plugin_results_count_without_aborting_the_group() {
    for mode in ["all", "any"] {
        let f = Fixture::new().await;
        let target = f.target().await;
        let mut invalid = f.github_condition();
        invalid["headSha"] = "2222222222222222222222222222222222222222".into();
        let wait = f
            .group(mode, vec![invalid, run_condition(&target)])
            .await
            .unwrap();
        if mode == "any" {
            assert_eq!(wait["deferred"], false);
            assert_eq!(
                wait["observation"]["members"][0]["result"]["state"],
                "unavailable"
            );
            assert!(wait["observation"]["members"][1]["result"].is_null());
        } else {
            assert_eq!(wait["deferred"], true);
            assert_eq!(
                wait["wakeup"]["members"][0]["observation"]["state"],
                "unavailable"
            );
            f.finish().await;
            assert_eq!(f.tick().await["run"]["status"], "succeeded");
            f.set_status(&target, "succeeded").await;
            let resumed = f.tick().await;
            assert_eq!(resumed["run"]["status"], "queued");
            assert!(text(&resumed["run"]["chatExecution"], "text").contains("unavailable"));
        }
    }
}

#[tokio::test]
async fn recorded_member_and_final_group_results_recheck_authorization_before_resuming() {
    for finalized in [false, true] {
        for revoke_grant in [false, true] {
            let f = Fixture::new().await;
            let target = f.target().await;
            f.github_state("completed", Some("success"), 0);
            f.group("all", vec![f.github_condition(), run_condition(&target)])
                .await
                .unwrap();
            f.finish().await;
            assert_eq!(
                f.wakeup().await["members"][0]["observation"]["conclusion"],
                "success"
            );
            f.set_status(&target, "succeeded").await;
            if finalized {
                f.record_groups().await;
                assert_eq!(f.wakeup().await["observation"]["state"], "completed");
            }
            let calls = f.calls().len();
            if revoke_grant {
                connections::set_enabled(
                    &f.s,
                    "github",
                    &f.instance,
                    &cairn_contracts::PluginEnabledInput { enabled: false },
                )
                .await
                .unwrap();
            } else {
                let mut project = f.s.get_project(&f.project).await.unwrap();
                project.origin = "https://github.com/another/repository.git".into();
                f.s.store
                    .transaction(move |db| db.save_project(&project, Some(&project.revision)))
                    .await
                    .unwrap();
            }
            let resumed = f.tick().await;
            assert_eq!(resumed["run"]["status"], "queued");
            let prompt = text(&resumed["run"]["chatExecution"], "text");
            assert!(prompt.contains(r#""state":"unavailable""#));
            assert!(!prompt.contains(r#""conclusion":"success""#));
            assert_eq!(f.calls().len(), calls);
        }
    }
}

#[tokio::test]
async fn a_recorded_any_result_keeps_its_snapshot_until_the_conversation_resumes() {
    let f = Fixture::new().await;
    let (first, second) = (f.target().await, f.target().await);
    f.group("any", vec![run_condition(&first), run_condition(&second)])
        .await
        .unwrap();
    f.finish().await;
    f.set_status(&second, "failed").await;
    f.record_groups().await;
    f.set_status(&first, "succeeded").await;
    let resumed = f.tick().await;
    let prompt = text(&resumed["run"]["chatExecution"], "text");
    assert!(prompt.contains(r#""result":null"#));
    assert!(prompt.contains(r#""status":"failed""#));
    assert!(!prompt.contains(r#""status":"succeeded""#));
}

#[tokio::test]
#[ignore = "local grouped-wait storage measurements; run explicitly after builds/tests finish"]
async fn measure_group_metadata() {
    for count in [1, 2, 8, 100] {
        let f = Fixture::new().await;
        let mut targets = Vec::new();
        for _ in 0..count {
            targets.push(f.target().await);
        }
        let conditions = targets.iter().map(run_condition).collect::<Vec<_>>();
        let mut writes = Vec::new();
        let cpu_start = process_cpu_ms();
        for index in 0..100 {
            let started = std::time::Instant::now();
            wait_for::schedule(
                &f.s,
                &f.token,
                &json!({
                    "conditions": conditions,
                    "mode": "all",
                    "reason": format!("Grouped measurement {index}"),
                    "timeoutSeconds": 420,
                }),
            )
            .await
            .unwrap();
            writes.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let write_cpu = process_cpu_ms() - cpu_start;
        let pending = f.s.get("chats", &f.chat).await.unwrap();
        let mut warm = Vec::new();
        for _ in 0..500 {
            let started = std::time::Instant::now();
            std::hint::black_box(f.s.get("chats", &f.chat).await.unwrap());
            warm.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let mut cold = Vec::new();
        let mut pool_open = Vec::new();
        for _ in 0..20 {
            let started = std::time::Instant::now();
            let store = cairn_installation::store::Store::open(&f.s.config.data_dir).unwrap();
            pool_open.push(started.elapsed().as_secs_f64() * 1000.0);
            let started = std::time::Instant::now();
            std::hint::black_box(store.get("chats", &f.chat).await.unwrap());
            cold.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        f.finish().await;
        let mut idle = Vec::new();
        let cpu_start = process_cpu_ms();
        for _ in 0..100 {
            let started = std::time::Instant::now();
            f.record_groups().await;
            idle.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let idle_cpu = process_cpu_ms() - cpu_start;
        let mut progress = Vec::new();
        let mut progress_cpu = 0.0;
        let mut first_bytes = 0;
        // Setup and target writes stay outside each timed progress tick. The reference
        // has one native condition, so this extra group scan has no progress to persist.
        for index in 0..20 {
            for target in &targets {
                f.set_status(target, "queued").await;
            }
            f.s.store
                .change_run(
                    &f.run,
                    cairn_installation::runs::RunChanges {
                        status: Some(cairn_contracts::RunStatus::Running),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            wait_for::schedule(
                &f.s,
                &f.token,
                &json!({
                    "conditions": conditions,
                    "mode": "all",
                    "reason": format!("Progress measurement {index}"),
                    "timeoutSeconds": 420,
                }),
            )
            .await
            .unwrap();
            f.finish().await;
            for (position, target) in targets.iter().enumerate() {
                f.set_status(target, "succeeded").await;
                let cpu_start = process_cpu_ms();
                let started = std::time::Instant::now();
                f.record_groups().await;
                progress.push(started.elapsed().as_secs_f64() * 1000.0);
                progress_cpu += process_cpu_ms() - cpu_start;
                if index == 0 && position == 0 {
                    first_bytes = serde_json::to_vec(&f.wakeup().await).unwrap().len();
                }
            }
        }
        let completed = f.wakeup().await;
        println!(
            "GROUP_WAIT_MEASUREMENT {}",
            json!({
                "conditions": count,
                "pendingWakeupBytes": serde_json::to_vec(&pending["wakeup"]).unwrap().len(),
                "pendingChatBytes": serde_json::to_vec(&pending).unwrap().len(),
                "firstProgressWakeupBytes": first_bytes,
                "finalWakeupBytes": serde_json::to_vec(&completed).unwrap().len(),
                "registration": milliseconds(&mut writes),
                "registrationProcessCpuMs": write_cpu,
                "warmRead": milliseconds(&mut warm),
                "freshPoolFirstRead": milliseconds(&mut cold),
                "freshPoolOpen": milliseconds(&mut pool_open),
                "idleGroupTick": milliseconds(&mut idle),
                "idleGroupTickProcessCpuMs": idle_cpu,
                "progressGroupTick": milliseconds(&mut progress),
                "progressGroupTickProcessCpuMs": progress_cpu,
                "externalProbeRequests": f.calls().len(),
            })
        );
    }
}

#[tokio::test]
async fn an_unused_any_member_stays_null_when_its_grant_is_revoked_after_resolution() {
    let f = Fixture::new().await;
    let target = f.target().await;
    f.group("any", vec![f.github_condition(), run_condition(&target)])
        .await
        .unwrap();
    f.finish().await;
    f.set_status(&target, "succeeded").await;
    f.record_groups().await;
    assert!(f.wakeup().await["observation"]["members"][0]["result"].is_null());
    connections::set_enabled(
        &f.s,
        "github",
        &f.instance,
        &cairn_contracts::PluginEnabledInput { enabled: false },
    )
    .await
    .unwrap();
    let resumed = f.tick().await;
    let prompt = text(&resumed["run"]["chatExecution"], "text");
    assert!(prompt.contains(r#""result":null"#));
    assert!(prompt.contains(r#""status":"succeeded""#));
    assert!(!prompt.contains("unavailable"));
}

#[tokio::test]
async fn a_hundred_conditions_keep_order_and_results_in_both_modes() {
    assert!(
        wait_for::tool(&[], false)["inputSchema"]["properties"]["conditions"]
            .get("maxItems")
            .is_none()
    );
    for mode in ["all", "any"] {
        let f = Fixture::new().await;
        let mut targets = Vec::new();
        for _ in 0..100 {
            targets.push(f.target().await);
        }
        let sources = targets.iter().map(run_condition).collect::<Vec<_>>();
        let wait = f.group(mode, sources.clone()).await.unwrap();
        assert_eq!(wait["deferred"], true);
        assert_eq!(wait["wakeup"]["members"].as_array().unwrap().len(), 100);
        f.finish().await;
        f.set_status(&targets[99], "failed").await;
        f.record_groups().await;
        if mode == "all" {
            assert!(f.wakeup().await["observation"].is_null());
            for target in &targets[..99] {
                f.set_status(target, "succeeded").await;
            }
            f.record_groups().await;
        }
        let result = f.wakeup().await["observation"].clone();
        assert_eq!(result["state"], "completed");
        let members = result["members"].as_array().unwrap();
        assert_eq!(members.len(), sources.len());
        for (index, member) in members.iter().enumerate() {
            assert_eq!(member["condition"], sources[index]);
            assert_eq!(member["result"].is_object(), mode == "all" || index == 99);
        }
        assert_eq!(members[99]["result"]["status"], "failed");
    }
}

#[tokio::test]
async fn long_dependency_chains_are_allowed_but_cycles_at_the_end_are_rejected() {
    let f = Fixture::new().await;
    let mut targets = Vec::new();
    for _ in 0..40 {
        targets.push(f.target().await);
    }
    for pair in targets.windows(2) {
        f.s.store
            .update_run_boundary(
                text(&pair[0], "id"),
                json!({
                    "wakeup": { "condition": run_condition(&pair[1]) },
                }),
            )
            .await
            .unwrap();
    }
    let sources = vec![run_condition(&targets[0]), run_condition(&targets[39])];
    assert_eq!(
        f.group("all", sources.clone()).await.unwrap()["deferred"],
        true
    );
    f.s.store
        .update_run_boundary(
            text(&targets[39], "id"),
            json!({
                "wakeup": { "condition": { "type": "run_finished", "runId": f.run } },
            }),
        )
        .await
        .unwrap();
    assert_eq!(f.group("any", sources).await.unwrap_err(), 409);
}

#[tokio::test]
async fn large_plugin_groups_probe_one_initial_batch_then_observe_every_member() {
    let f = Fixture::new().await;
    let sources = (1..=40).map(|run| {
        f.github.respond(&format!("repos/fixture/repo/actions/runs/{run}"), &json!({
            "id": run, "head_sha": SHA, "status": "completed", "conclusion": "success", "run_attempt": 1,
        }));
        json!({ "type": "github_workflow", "projectId": f.project, "runId": run, "headSha": SHA })
    }).collect::<Vec<_>>();
    let wait = f.group("all", sources).await.unwrap();
    assert_eq!(wait["deferred"], true);
    assert_eq!(
        f.calls().len(),
        4,
        "Registration probes only one bounded batch"
    );
    f.finish().await;
    for _ in 0..3 {
        conditions::poll_once(&f.s).await.unwrap();
    }
    let result = f.wakeup().await["observation"].clone();
    assert_eq!(result["state"], "completed");
    assert_eq!(result["members"].as_array().unwrap().len(), 40);
    assert!(
        result["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|member| member["result"]["conclusion"] == "success")
    );
    assert_eq!(
        f.calls().len(),
        40,
        "All members checked without rechecking completed ones"
    );
}
