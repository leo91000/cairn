use common::AgentFixture;

mod common;

use axum::{Json, Router, extract::State, routing::get};
use cairn_installation::{
    config::{MAIN_AGENT_ID, id, now},
    nodes::{LOCAL_NODE_ID, exclusive},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::Mutex;

struct Fixture {
    root: tempfile::TempDir,
    s: Arc<Service>,
    remote: Arc<Mutex<Value>>,
    run: String,
    token: String,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let remote = Arc::new(Mutex::new(
            json!({ "reservation": null, "highestEpoch": 42 }),
        ));
        let app = Router::new().route("/exclusive", get(|State(state): State<Arc<Mutex<Value>>>| async move { Json(state.lock().await.clone()) }))
            .route("/exclusive/acquire", axum::routing::post(|State(state): State<Arc<Mutex<Value>>>, Json(mut body): Json<Value>| async move {
                let mut state = state.lock().await;
                body["state"] = "acquired".into();
                body["pausedAttemptIds"] = json!([]);
                state["reservation"] = body;
                state["highestEpoch"] = state["reservation"]["epoch"].clone();
                Json(state.clone())
            }))
            .route("/exclusive/release", axum::routing::post(|State(state): State<Arc<Mutex<Value>>>, Json(body): Json<Value>| async move {
                let mut state = state.lock().await;
                state["reservation"] = Value::Null;
                state["highestEpoch"] = body["epoch"].clone();
                Json(state.clone())
            })).with_state(remote.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let runner_url = format!("http://{}", listener.local_addr().unwrap());
        let s = Service::new(cairn_installation::config::Config {
            runner_url,
            ..common::config(root.path())
        })
        .await
        .unwrap();
        let stop = s.shutdown.clone();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .unwrap();
        });
        s.store
            .put(
                "nodes",
                json!({
                    "id": LOCAL_NODE_ID,
                    "exclusiveAgents": [MAIN_AGENT_ID],
                    "exclusiveReservations": true,
                    "limits": { "memoryMiB": 4096 },
                }),
            )
            .await
            .unwrap();
        s.agent_fixture(
            json!({ "exclusiveNodes": [LOCAL_NODE_ID] }),
            Some(MAIN_AGENT_ID),
        )
        .await
        .unwrap();
        let mut fixture = Self {
            root,
            s,
            remote,
            run: String::new(),
            token: String::new(),
        };
        let (run, token) = fixture.new_run().await;
        fixture.run = run;
        fixture.token = token;
        tokio::spawn(exclusive::maintain(fixture.s.clone()));
        fixture
    }

    async fn new_run(&self) -> (String, String) {
        let chat = self.s.chat_create(json!({})).await.unwrap();
        let message = id();
        self.s
            .chat_send(
                text(&chat, "id"),
                json!({ "id": message, "text": "Exclusive fixture" }),
            )
            .await
            .unwrap();
        self.s
            .chat_tick(&HashSet::from([self.run.clone()]))
            .await
            .unwrap();
        let run = self.s.chat_detail(text(&chat, "id")).await.unwrap()["run"].clone();
        let run_id = text(&run, "id").to_owned();
        self.s
            .store
            .patch_run(
                &run_id,
                json!({ "status": "running", "nodeId": LOCAL_NODE_ID }),
            )
            .await
            .unwrap();
        self.s.chat_acknowledge(&run_id, &message).await.unwrap();
        let config = self.s.mcps.run_configuration(&self.s, &run).await.unwrap();
        self.s.store.set(&format!("run-checkpoint:{run_id}"), json!({
            "runnerId": id(), "nodeId": LOCAL_NODE_ID, "prepared": { "backend": "firecracker" },
        }), None).await.unwrap();
        (run_id, text(&config["env"], "CAIRN_MCP_RUN_TOKEN").into())
    }

    async fn until(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let status = exclusive::status(&self.s, &self.token).await.unwrap();
            if status.as_array().unwrap().iter().any(&predicate) {
                return status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "reservation did not settle: {status}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.s.shutdown.cancel();
    }
}

fn request() -> Value {
    json!({ "durationSeconds": 60, "waitSeconds": 3, "reason": "Build with the node available" })
}

#[tokio::test]
async fn backup_without_requests_releases_an_orphaned_controller_reservation() {
    let f = Fixture::new().await;
    let reservation_id = id();
    *f.remote.lock().await = json!({
        "reservation": { "id": reservation_id, "epoch": 100, "state": "acquired" },
        "highestEpoch": 100,
    });
    assert!(
        f.s.store
            .list("node-exclusive-requests")
            .await
            .unwrap()
            .is_empty()
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !f.remote.lock().await["reservation"].is_null() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "orphaned reservation was not released"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let acquired = exclusive::acquire(&f.s, &f.token, &request())
        .await
        .unwrap();
    assert_eq!(acquired["state"], "acquired");
    assert_eq!(f.remote.lock().await["highestEpoch"], 101);
}

#[tokio::test]
async fn manager_acquisition_is_idempotent_fenced_and_released_before_waiting() {
    let f = Fixture::new().await;
    let acquired = exclusive::acquire(&f.s, &f.token, &request())
        .await
        .unwrap();
    assert_eq!(acquired["state"], "acquired");
    assert_eq!(f.remote.lock().await["reservation"]["epoch"], 43);
    assert_eq!(
        exclusive::acquire(&f.s, &f.token, &request())
            .await
            .unwrap()["expiresAt"],
        acquired["expiresAt"]
    );
    let renewed = exclusive::renew(
        &f.s,
        &f.token,
        &json!({ "reservationId": acquired["id"], "durationSeconds": 120 }),
    )
    .await
    .unwrap();
    assert_eq!(
        renewed["expiresAt"], acquired["expiresAt"],
        "unacknowledged renewal does not claim more time"
    );
    assert_eq!(renewed["renewalPending"], true);
    f.until(|record| record["renewalPending"] == false).await;
    cairn_installation::chats::wait_for::schedule(
        &f.s,
        &f.token,
        &json!({ "conditions": [{ "type": "delay", "seconds": 30 }], "reason": "Remote result" }),
    )
    .await
    .unwrap();
    f.until(|record| record["state"] == "released").await;
    assert!(f.remote.lock().await["reservation"].is_null());
    assert!(
        f.s.store
            .get("nodes", LOCAL_NODE_ID)
            .await
            .unwrap()
            .unwrap()["exclusiveReservation"]
            .is_null()
    );
    assert!(f.root.path().exists());
}

#[tokio::test]
async fn access_revocation_releases_but_never_grants_another_agent_permission() {
    let f = Fixture::new().await;
    f.s.agent_fixture(json!({ "exclusiveNodes": [] }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    assert_eq!(
        exclusive::acquire(&f.s, &f.token, &request())
            .await
            .unwrap_err()
            .status,
        403
    );
    f.s.agent_fixture(
        json!({ "exclusiveNodes": [LOCAL_NODE_ID] }),
        Some(MAIN_AGENT_ID),
    )
    .await
    .unwrap();
    let acquired = exclusive::acquire(&f.s, &f.token, &request())
        .await
        .unwrap();
    assert_eq!(acquired["state"], "acquired");
    f.s.agent_fixture(json!({ "exclusiveNodes": [] }), Some(MAIN_AGENT_ID))
        .await
        .unwrap();
    f.until(|record| record["state"] == "released").await;
    assert!(f.remote.lock().await["reservation"].is_null());
}

#[tokio::test]
async fn another_run_cannot_release_and_the_next_request_waits_in_order() {
    let f = Fixture::new().await;
    let acquired = exclusive::acquire(&f.s, &f.token, &request())
        .await
        .unwrap();
    let (other, token) = f.new_run().await;
    let release = json!({ "reservationId": acquired["id"] });
    assert_eq!(
        exclusive::release(&f.s, &token, &release)
            .await
            .unwrap_err()
            .status,
        403
    );
    let mut pending = request();
    pending["waitSeconds"] = 5.into();
    let s = f.s.clone();
    let task = tokio::spawn(async move { exclusive::acquire(&s, &token, &pending).await.unwrap() });
    while f
        .s
        .store
        .list("node-exclusive-requests")
        .await
        .unwrap()
        .len()
        < 2
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(f.remote.lock().await["reservation"]["id"], acquired["id"]);
    exclusive::release(&f.s, &f.token, &release).await.unwrap();
    f.until(|record| record["state"] == "released").await;
    assert_eq!(task.await.unwrap()["state"], "acquired");
    assert_eq!(f.remote.lock().await["reservation"]["epoch"], 44);
    assert_eq!(
        exclusive::release(&f.s, &f.token, &release).await.unwrap()["state"],
        "released"
    );
    assert_ne!(other, f.run);
}

#[tokio::test]
async fn replaced_attempt_and_invalid_input_cannot_reuse_a_reservation() {
    let f = Fixture::new().await;
    let mut invalid = request();
    invalid["durationSeconds"] = 901.into();
    assert_eq!(
        exclusive::acquire(&f.s, &f.token, &invalid)
            .await
            .unwrap_err()
            .status,
        400
    );
    let acquired = exclusive::acquire(&f.s, &f.token, &request())
        .await
        .unwrap();
    f.s.store.set(&format!("run-checkpoint:{}", f.run), json!({ "runnerId": id(), "nodeId": LOCAL_NODE_ID, "prepared": { "backend": "firecracker" } }), None).await.unwrap();
    f.until(|record| record["state"] == "released").await;
    assert!(acquired["expiresAt"].as_i64().unwrap() > now());
}

#[tokio::test]
async fn nonblocking_requests_remain_queued_until_their_independent_acquisition_timeout() {
    let mut f = Fixture::new().await;
    let held = exclusive::acquire(&f.s, &f.token, &request())
        .await
        .unwrap();
    let (_, token) = f.new_run().await;
    let mut nonblocking = request();
    nonblocking["waitSeconds"] = 0.into();
    nonblocking["acquisitionTimeoutSeconds"] = 1.into();
    let expired = exclusive::acquire(&f.s, &token, &nonblocking)
        .await
        .unwrap();
    assert_eq!(expired["state"], "queued");
    assert!(expired["waitUntil"].as_i64().unwrap() > now());
    f.token = token;
    f.until(|record| record["id"] == expired["id"] && record["state"] == "expired")
        .await;
    assert_eq!(f.remote.lock().await["reservation"]["id"], held["id"]);

    nonblocking["acquisitionTimeoutSeconds"] = 30.into();
    let queued = exclusive::acquire(&f.s, &f.token, &nonblocking)
        .await
        .unwrap();
    assert_eq!(queued["state"], "queued");
    assert!(queued["waitUntil"].as_i64().unwrap() > now() + 20_000);
    exclusive::release_owned(&f.s, text(&held, "id"), None)
        .await
        .unwrap();
    f.until(|record| record["id"] == queued["id"] && record["state"] == "acquired")
        .await;
    assert_eq!(f.remote.lock().await["reservation"]["id"], queued["id"]);
}
