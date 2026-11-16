//! Whole manager/runner-client journey; only the execution controller and provider are fixtures.
use super::*;
use cairn_installation::background_jobs::{
    Outcome, Registry,
    controller::TurnReceipt,
    protocol::{Event, Request as JobRequest},
};
use std::sync::atomic::{AtomicBool, Ordering};

struct JobsController {
    legacy: Arc<Controller>,
    root: PathBuf,
    jobs: tokio::sync::Mutex<Registry>,
    turns: tokio::sync::Mutex<Vec<(Value, Vec<Value>, bool)>>,
    closed: AtomicBool,
    deleted: AtomicBool,
}

impl JobsController {
    async fn start(self: &Arc<Self>, plan: Value) {
        let turn = text(&plan, "turnId").to_owned();
        let mut turns = self.turns.lock().await;
        if turns.iter().any(|(old, _, _)| old["turnId"] == turn) {
            return;
        }
        let index = turns.len();
        let output = format!(
            "{}\n{}\n",
            json!({"type":"thread.started","thread_id":"fixture-session"}),
            json!({"type":"chat.delivered","messageId":plan["chat"]["execution"]["messageId"]})
        );
        let packet = json!({"offset":1,"event":{"type":"output","stderr":false,"data":STANDARD.encode(output)}});
        turns.push((plan, vec![packet], false));
        drop(turns);
        let state = self.clone();
        tokio::spawn(async move {
            if index == 0 {
                while !state.root.join("finish-provider").exists()
                    && !state.closed.load(Ordering::SeqCst)
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            let output = format!(
                "{}\n{}\n",
                json!({"type":"item.completed","item":{"id":"answer","type":"agent_message","text":if index == 0 {"Background work started"} else {"Background completion observed"}}}),
                json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}})
            );
            let mut turns = state.turns.lock().await;
            turns[index].1.push(json!({"offset":2,"event":{"type":"output","stderr":false,"data":STANDARD.encode(output)}}));
            turns[index].2 = true;
        });
    }
}

async fn serve_jobs_controller(
    State(state): State<Arc<JobsController>>,
    request: Request,
) -> Response {
    let path = request.uri().path().to_owned();
    if path == "/health" {
        let mut value =
            common::read_json(serve_controller(State(state.legacy.clone()), request).await).await;
        value["environmentProtocol"] = 1.into();
        return Json(value).into_response();
    }
    if path.ends_with("/environment") {
        let bytes = axum::body::to_bytes(request.into_body(), 256 * 1024)
            .await
            .unwrap();
        let operation: JobRequest = serde_json::from_slice(&bytes).unwrap();
        let response = match operation {
            JobRequest::StartJob {
                request_id,
                name,
                command,
            } => {
                let job_id = state
                    .jobs
                    .lock()
                    .await
                    .start(&request_id, &name, &command)
                    .await
                    .unwrap();
                Event::JobStarted { request_id, job_id }
            }
            JobRequest::LookupJob {
                request_id,
                call_id,
            } => Event::JobLookup {
                request_id,
                job_id: state.jobs.lock().await.lookup(&call_id),
            },
            JobRequest::JobStatus { request_id, job_id } => {
                let outcome = state.jobs.lock().await.status(&job_id).unwrap();
                Event::JobStatus {
                    request_id,
                    job_id,
                    outcome,
                }
            }
            JobRequest::Activity { request_id } => Event::Activity {
                request_id,
                jobs: state.jobs.lock().await.active(),
                containers: false,
            },
            JobRequest::ResumeTurn {
                request_id, plan, ..
            } => {
                state.start(plan).await;
                Event::Accepted { request_id }
            }
            JobRequest::Close { request_id } => {
                state.jobs.lock().await.close().await.unwrap();
                state.closed.store(true, Ordering::SeqCst);
                Event::Accepted { request_id }
            }
        };
        return Json(response).into_response();
    }
    if path.contains("/turn/") {
        let parts: Vec<_> = path.split('/').collect();
        let turn = parts[4];
        let turns = state.turns.lock().await;
        let Some((plan, _, done)) = turns.iter().find(|(plan, _, _)| plan["turnId"] == turn) else {
            return StatusCode::ACCEPTED.into_response();
        };
        if path.ends_with("/start") {
            return Json(json!({"offset":0})).into_response();
        }
        if !done {
            drop(turns);
            tokio::time::sleep(Duration::from_millis(30)).await;
            return StatusCode::ACCEPTED.into_response();
        }
        return Json(TurnReceipt {
            attempt_id: text(plan, "id").into(),
            generation: "fixture-generation".into(),
            turn_id: turn.into(),
            code: 0,
            log_start: 0,
            log_end: 2,
        })
        .into_response();
    }
    if path.ends_with("/logs") {
        let offset = request
            .uri()
            .query()
            .and_then(|q| q.split('&').find_map(|p| p.strip_prefix("offset=")))
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let index = state.turns.lock().await.len() - 1;
        let stream = futures_util::stream::unfold(
            (state, index, offset),
            |(state, index, offset)| async move {
                let frame = state.turns.lock().await[index].1.get(offset).cloned();
                let (frame, next) = match frame {
                    Some(frame) => (frame, offset + 1),
                    None => {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        (json!({"type":"heartbeat"}), offset)
                    }
                };
                Some((
                    Ok::<_, std::io::Error>(axum::body::Bytes::from(format!("{frame}\n"))),
                    (state, index, next),
                ))
            },
        );
        return Body::from_stream(stream).into_response();
    }
    if request.method() == "POST" && path.starts_with("/runs/") && path.split('/').count() == 3 {
        let attempt = path.split('/').nth(2).unwrap();
        if let Some((original, _, _)) = state.turns.lock().await.first() {
            assert_eq!(
                original["id"], attempt,
                "Managed job continuation replaced its environment"
            );
            return Json(json!({})).into_response();
        }
        {
            let plan: Value = serde_json::from_slice(
                &tokio::fs::read(
                    state
                        .legacy
                        .data
                        .join("runner-plans")
                        .join(format!("{attempt}.json")),
                )
                .await
                .unwrap(),
            )
            .unwrap();
            state.legacy.plans.lock().await.push(plan.clone());
            state.start(plan).await;
        }
        return Json(json!({})).into_response();
    }
    if request.method() == "DELETE" && path.starts_with("/runs/") {
        state.jobs.lock().await.close().await.unwrap();
        state.deleted.store(true, Ordering::SeqCst);
        return Json(json!({})).into_response();
    }
    serve_controller(State(state.legacy.clone()), request).await
}

async fn workspace_call(plan: &Value, name: &str, args: Value, call: u64) -> Value {
    let home = plan["imports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|import| import["target"] == "/home/node")
        .unwrap();
    let client = reqwest::Client::builder()
        .unix_socket(Path::new(text(home, "source")).join("cairn-mcp.sock"))
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let response = client.post(text(&plan["chat"]["codexConfig"]["mcp_servers"]["cairn_workspace"],"url"))
        .bearer_auth(text(&plan["mcpEnv"],"CAIRN_MCP_RUN_TOKEN"))
        .header("mcp-protocol-version","2025-11-25")
        .json(&json!({"jsonrpc":"2.0","id":call,"method":"tools/call","params":{"name":name,"arguments":args}})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let reply: Value = response.json().await.unwrap();
    assert!(reply["error"].is_null(), "{reply}");
    assert_ne!(reply["result"]["isError"], true, "{reply}");
    reply["result"]["structuredContent"].clone()
}

#[tokio::test]
async fn managed_job_wait_releases_account_and_resumes_the_same_environment() {
    let mut fixture = Fixture::with_concurrency(1).await;
    fixture.stop(false).await;
    let service = fixture.service.clone();
    let mut account = serde_json::to_value(
        service
            .accounts
            .create(&service, Provider::Codex, "Background fixture")
            .await
            .unwrap(),
    )
    .unwrap();
    account["state"] = "ready".into();
    account["maxConcurrentRuns"] = 1.into();
    service
        .store
        .save_account_fixture(account.clone())
        .await
        .unwrap();
    codex_credentials(&service,text(&account,"id"),json!({"access_token":"synthetic","refresh_token":"synthetic-refresh","account_id":"job-journey"})).await;
    let state = Arc::new(JobsController {
        legacy: Arc::new(Controller {
            data: service.config.data_dir.clone(),
            plans: tokio::sync::Mutex::new(Vec::new()),
            failures: 0,
            slots: 4,
            hold: AtomicBool::new(false),
        }),
        root: fixture.root.path().to_owned(),
        jobs: tokio::sync::Mutex::new(Registry::new(
            Path::new(env!("CARGO_BIN_EXE_cairn")),
            fixture.root.path(),
            cairn_installation::process::Environment::new(),
            cairn_installation::background_jobs::boot_ms().unwrap() + 60_000,
        )),
        turns: tokio::sync::Mutex::new(Vec::new()),
        closed: AtomicBool::new(false),
        deleted: AtomicBool::new(false),
    });
    let (listener, address) = common::bind().await;
    drop(service);
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let service = fixture.service.clone();
    let server = common::serve(
        listener,
        Router::new()
            .fallback(any(serve_jobs_controller))
            .with_state(state.clone()),
    );
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    let chat = fixture
        .start_chat(message("Start and await a managed measurement"))
        .await;
    fixture.start().await;
    let run = fixture.chat_run(&chat).await;
    fixture
        .until(&run, |run| run["sessionId"].is_string())
        .await;
    let plan = state.turns.lock().await[0].0.clone();
    assert_eq!(plan["environmentProtocol"], 1);
    let command = "echo $$ > job.pid; while [ ! -e finish-job ]; do sleep 0.05; done; printf measurement-complete; exit 7";
    let job = workspace_call(
        &plan,
        "start_job",
        json!({"name":"measurement","command":command}),
        71,
    )
    .await;
    let repeated = workspace_call(
        &plan,
        "start_job",
        json!({"name":"measurement","command":command}),
        71,
    )
    .await;
    assert_eq!(job, repeated);
    let job_id = text(&job, "jobId");
    let wait = workspace_call(&plan,"wait_for",json!({"conditions":[{"type":"job_finished","jobId":job_id},{"type":"delay","seconds":45}],"mode":"any","timeoutSeconds":55,"reason":"Await measurement"}),72).await;
    assert_eq!(wait["deferred"], true);
    tokio::fs::write(state.root.join("finish-provider"), "")
        .await
        .unwrap();
    fixture
        .until(&run, |run| run["status"] == "succeeded")
        .await;
    eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || {
            service
                .accounts
                .lease(&service, &run)
                .await
                .unwrap()
                .is_none()
                .then_some(())
        },
    )
    .await;
    assert!(
        !state.closed.load(Ordering::SeqCst),
        "run={}, checkpoint={}, job={:?}",
        fixture.service.store.run(&run).await.unwrap(),
        fixture.checkpoint(&run).await,
        state.jobs.lock().await.status(job_id).unwrap()
    );
    assert!(
        !state.deleted.load(Ordering::SeqCst),
        "run={}, checkpoint={}",
        fixture.service.store.run(&run).await.unwrap(),
        fixture.checkpoint(&run).await
    );
    assert!(matches!(
        state.jobs.lock().await.status(job_id).unwrap(),
        Outcome::Running
    ));
    assert_eq!(state.turns.lock().await.len(), 1);
    let saved = fixture.checkpoint(&run).await;
    assert_eq!(saved["heldEnvironment"]["attempt"], plan["id"]);
    assert_eq!(saved["heldEnvironment"]["generation"], "fixture-generation");
    let pid = tokio::fs::read_to_string(state.root.join("job.pid"))
        .await
        .unwrap();
    assert!(Path::new(&format!("/proc/{}", pid.trim())).exists());
    tokio::fs::write(state.root.join("finish-job"), "")
        .await
        .unwrap();
    let completed = fixture
        .until(&run, |run| {
            run["status"] == "succeeded" && run["summary"] == "Background completion observed"
        })
        .await;
    assert_eq!(completed["sessionId"], "fixture-session");
    let turns = state.turns.lock().await;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].0["id"], turns[1].0["id"]);
    assert!(
        turns[1].0["chat"]
            .to_string()
            .contains("measurement-complete")
    );
    assert!(turns[1].0["chat"].to_string().contains("exitCode"));
    drop(turns);
    assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
    eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || state.deleted.load(Ordering::SeqCst).then_some(()),
    )
    .await;
    assert!(state.closed.load(Ordering::SeqCst));
    assert_eq!(state.legacy.plans.lock().await.len(), 1);
    fixture.stop(false).await;
    server.abort();
}
