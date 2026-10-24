use super::*;

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
            .patch_run(
                text(run, "id"),
                json!({ "status": status, "finishedAt": now() }),
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
                    .events(&run, 0, 100)?
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
    let nine = (0..9)
        .map(|_| json!({ "type": "run_finished", "runId": id() }))
        .collect::<Vec<_>>();
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
        (request(json!(nine), json!({})), 400),
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
        .patch_run(text(&target, "id"), json!({ "status": "succeeded" }))
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
