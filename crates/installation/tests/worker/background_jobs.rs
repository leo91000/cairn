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
    activity: tokio::sync::Notify,
    disconnect_activity: Option<tokio_util::sync::CancellationToken>,
    close_started: tokio::sync::Notify,
    hold_close: Option<tokio_util::sync::CancellationToken>,
    deadline: Option<DeadlineCase>,
    closed: AtomicBool,
    deleted: AtomicBool,
}

fn invalid_control_response() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": "Execution environment sent an invalid control frame.",
            "code": "environment_invalid_control_frame",
        })),
    )
        .into_response()
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
            let item = if index == 0 && state.root.join("propose-plan").exists() {
                json!({"id":"answer","type":"proposed_plan","text":"## Proposed plan"})
            } else {
                json!({"id":"answer","type":"agent_message","text":if index == 0 {"Background work started"} else {"Background completion observed"}})
            };
            let output = format!(
                "{}\n{}\n",
                json!({"type":"item.completed","item":item}),
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
            JobRequest::Activity { request_id } => {
                if let Some(disconnect) = &state.disconnect_activity {
                    use futures_util::StreamExt;

                    let disconnect = disconnect.clone();
                    state.activity.notify_one();
                    if state.deadline == Some(DeadlineCase::InvalidActivity) {
                        disconnect.cancelled().await;
                        return invalid_control_response();
                    }
                    if state.deadline == Some(DeadlineCase::GatewayDuringActivity) {
                        disconnect.cancelled().await;
                        return StatusCode::BAD_GATEWAY.into_response();
                    }
                    if state.deadline == Some(DeadlineCase::GatewayTimeoutDuringActivity) {
                        disconnect.cancelled().await;
                        return StatusCode::GATEWAY_TIMEOUT.into_response();
                    }
                    let header =
                        futures_util::stream::iter([Ok(axum::body::Bytes::from_static(b"{"))]);
                    let disconnected = futures_util::stream::once(async move {
                        disconnect.cancelled().await;
                        Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionReset,
                            "fixture disconnected",
                        ))
                    });
                    return Body::from_stream(header.chain(disconnected)).into_response();
                }
                let jobs = state.jobs.lock().await.active();
                state.activity.notify_one();
                Event::Activity {
                    request_id,
                    jobs,
                    containers: false,
                }
            }
            JobRequest::ResumeTurn {
                request_id, plan, ..
            } => {
                state.start(plan).await;
                Event::Accepted { request_id }
            }
            JobRequest::Close { request_id } => {
                if let Some(hold) = &state.hold_close {
                    state.close_started.notify_one();
                    hold.cancelled().await;
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                state.jobs.lock().await.close().await.unwrap();
                state.closed.store(true, Ordering::SeqCst);
                if state.deadline == Some(DeadlineCase::InvalidClose) {
                    return invalid_control_response();
                }
                if state.deadline == Some(DeadlineCase::MalformedClose) {
                    return "{invalid".into_response();
                }
                if state.deadline == Some(DeadlineCase::UnexpectedCloseResponse) {
                    return Json(Event::Activity {
                        request_id,
                        jobs: false,
                        containers: false,
                    })
                    .into_response();
                }
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
        if matches!(
            state.deadline,
            Some(
                DeadlineCase::FlushFailedOnDelete
                    | DeadlineCase::ShutdownDuringWaitWithFlushFailure
            )
        ) {
            return Json(json!({ "StatusCode": 70, "FinalizationFailed": true })).into_response();
        }
        return Json(json!({})).into_response();
    }
    if path.ends_with("/wait")
        && matches!(
            state.deadline,
            Some(DeadlineCase::FlushFailedOnWait | DeadlineCase::WaitingWithFlushFailure)
        )
        && state.deleted.load(Ordering::SeqCst)
    {
        return Json(json!({ "StatusCode": 70, "FinalizationFailed": true })).into_response();
    }
    if path.ends_with("/wait") && state.deadline == Some(DeadlineCase::OfflineAtDeadline) {
        return Json(json!({ "StatusCode": 143, "PhysicalFinalized": true })).into_response();
    }
    if path.ends_with("/wait") && state.deadline == Some(DeadlineCase::RetiredNativeCleanup) {
        return Json(json!({
            "StatusCode": 70,
            "FinalizationFailed": true,
            "NativeCleanup": {
                "attemptId": path.split('/').nth(2).unwrap(),
                "workloadFenced": true,
                "message": "The old cleanup receipt is stale.",
            },
        }))
        .into_response();
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeadlineCase {
    Completed,
    DisconnectedDuringActivity,
    GatewayDuringActivity,
    GatewayTimeoutDuringActivity,
    InvalidActivity,
    InvalidClose,
    MalformedClose,
    UnexpectedCloseResponse,
    Waiting,
    FlushFailedOnDelete,
    FlushFailedOnWait,
    WaitingWithFlushFailure,
    RestartDuringFence,
    ShutdownDuringWait,
    ShutdownDuringWaitWithFlushFailure,
    OfflineAtDeadline,
    RetiredNativeCleanup,
}

async fn jobs_fixture(
    deadline: Option<DeadlineCase>,
) -> (Fixture, Arc<JobsController>, tokio::task::JoinHandle<()>) {
    let mut fixture = Fixture::with_concurrency(1).await;
    fixture.stop(false).await;
    if deadline.is_some() {
        fixture.clock = Some(FixtureClock::new(fixture.root.path()));
    }
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
        activity: tokio::sync::Notify::new(),
        disconnect_activity: matches!(
            deadline,
            Some(
                DeadlineCase::DisconnectedDuringActivity
                    | DeadlineCase::GatewayDuringActivity
                    | DeadlineCase::GatewayTimeoutDuringActivity
                    | DeadlineCase::InvalidActivity
            )
        )
        .then(tokio_util::sync::CancellationToken::new),
        close_started: tokio::sync::Notify::new(),
        hold_close: matches!(
            deadline,
            Some(
                DeadlineCase::RestartDuringFence
                    | DeadlineCase::ShutdownDuringWait
                    | DeadlineCase::ShutdownDuringWaitWithFlushFailure
            )
        )
        .then(tokio_util::sync::CancellationToken::new),
        deadline,
        closed: AtomicBool::new(false),
        deleted: AtomicBool::new(false),
    });
    let (listener, address) = common::bind().await;
    drop(service);
    common::reconfigure(&mut fixture.service, |config| {
        config.runner_url = format!("http://{address}");
    })
    .await;
    let server = common::serve(
        listener,
        Router::new()
            .fallback(any(serve_jobs_controller))
            .with_state(state.clone()),
    );
    cairn_installation::nodes::refresh_local(&fixture.service)
        .await
        .unwrap();
    (fixture, state, server)
}

#[tokio::test]
async fn managed_job_wait_releases_account_and_resumes_the_same_environment() {
    let (mut fixture, state, server) = jobs_fixture(None).await;
    let service = fixture.service.clone();
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

#[tokio::test]
async fn background_deadline_preserves_a_completed_chat_without_a_wait() {
    check_background_deadline(DeadlineCase::Completed).await;
}

#[tokio::test]
async fn background_deadline_preserves_success_when_the_controller_stream_disconnects() {
    check_background_deadline(DeadlineCase::DisconnectedDuringActivity).await;
}

#[tokio::test]
async fn background_deadline_preserves_success_when_the_gateway_loses_the_controller() {
    for case in [
        DeadlineCase::GatewayDuringActivity,
        DeadlineCase::GatewayTimeoutDuringActivity,
    ] {
        check_background_deadline(case).await;
    }
}

#[tokio::test]
async fn background_deadline_keeps_an_observed_controller_protocol_failure() {
    check_background_deadline(DeadlineCase::InvalidActivity).await;
}

#[tokio::test]
async fn background_deadline_keeps_a_controller_protocol_failure_during_close() {
    for case in [
        DeadlineCase::InvalidClose,
        DeadlineCase::MalformedClose,
        DeadlineCase::UnexpectedCloseResponse,
    ] {
        check_background_deadline(case).await;
    }
}

#[tokio::test]
async fn background_deadline_reports_interruption_when_the_chat_is_waiting() {
    check_background_deadline(DeadlineCase::Waiting).await;
}

#[tokio::test]
async fn background_deadline_surfaces_a_flush_failure_in_the_delete_receipt() {
    check_background_deadline(DeadlineCase::FlushFailedOnDelete).await;
}

#[tokio::test]
async fn background_deadline_observes_a_flush_failure_after_delete() {
    check_background_deadline(DeadlineCase::FlushFailedOnWait).await;
}

#[tokio::test]
async fn background_deadline_keeps_a_confirmed_flush_failure_for_an_active_wait() {
    check_background_deadline(DeadlineCase::WaitingWithFlushFailure).await;
}

#[tokio::test]
async fn background_deadline_finishes_fencing_after_the_manager_restarts() {
    check_background_deadline(DeadlineCase::RestartDuringFence).await;
}

#[tokio::test]
async fn background_deadline_keeps_a_wait_during_graceful_manager_shutdown() {
    check_background_deadline(DeadlineCase::ShutdownDuringWait).await;
}

#[tokio::test]
async fn shutdown_at_deadline_keeps_the_already_confirmed_provider_reply() {
    check_shutdown_after_receipt(ReceiptExit::Graceful).await;
}

#[tokio::test]
async fn restart_at_deadline_keeps_the_confirmed_plan_without_an_agent_message() {
    check_shutdown_after_receipt(ReceiptExit::Plan).await;
}

#[tokio::test]
async fn restart_at_deadline_keeps_the_whole_confirmed_multipart_reply() {
    check_shutdown_after_receipt(ReceiptExit::Multipart).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiptExit {
    Graceful,
    Plan,
    Multipart,
}

async fn check_shutdown_after_receipt(exit: ReceiptExit) {
    let (mut fixture, state, server) = jobs_fixture(Some(DeadlineCase::Completed)).await;
    let summary = match exit {
        ReceiptExit::Plan => {
            tokio::fs::write(state.root.join("propose-plan"), "")
                .await
                .unwrap();
            "## Proposed plan"
        }
        ReceiptExit::Multipart => "First part\nBackground work started",
        ReceiptExit::Graceful => "Background work started",
    };
    let chat = fixture.start_chat(message("Start a background job")).await;
    fixture.start().await;
    let run = fixture.chat_run(&chat).await;
    fixture
        .until(&run, |run| run["sessionId"].is_string())
        .await;
    let plan = state.turns.lock().await[0].0.clone();
    let clock = fixture.clock.as_mut().unwrap();
    let deadline = clock.millis + 10_000;
    let plan_path = fixture
        .service
        .config
        .data_dir
        .join("runner-plans")
        .join(format!("{}.json", text(&plan, "id")));
    let mut deadline_plan = plan;
    deadline_plan["expires"] = deadline.into();
    tokio::fs::write(plan_path, serde_json::to_vec(&deadline_plan).unwrap())
        .await
        .unwrap();
    if exit == ReceiptExit::Multipart {
        let path = fixture.run_directory(&run).join("output/result.md");
        tokio::fs::write(path, summary).await.unwrap();
    }
    let gate =
        tokio::net::UnixListener::bind(fixture.run_directory(&run).join("fixture-receipt.sock"))
            .unwrap();
    tokio::fs::write(state.root.join("finish-provider"), "")
        .await
        .unwrap();
    let (_connection, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
        .await
        .unwrap()
        .unwrap();
    let saved = fixture.checkpoint(&run).await;
    let reported_at = saved["retainedCompletion"]["finishedAt"].as_i64().unwrap();
    if exit == ReceiptExit::Multipart {
        assert_eq!(saved["retainedCompletion"]["outputSummary"], summary);
    }

    assert_eq!(
        fixture.service.store.run(&run).await.unwrap()["status"],
        "running"
    );
    let clock = fixture.clock.as_mut().unwrap();
    clock.advance(deadline - clock.millis + 1);
    let abrupt = exit != ReceiptExit::Graceful;
    fixture.stop(abrupt).await;
    let retired = if abrupt {
        fixture.start().await;
        fixture
            .until(&run, |run| {
                run["status"] == "succeeded"
                    && run["recoveryPending"] == false
                    && state.deleted.load(Ordering::SeqCst)
            })
            .await
    } else {
        fixture.service.store.run(&run).await.unwrap()
    };
    assert_eq!(retired["status"], "succeeded");
    assert_eq!(retired["finishedAt"], reported_at);
    assert_eq!(retired["summary"], summary);
    assert!(state.deleted.load(Ordering::SeqCst));
    assert_eq!(state.turns.lock().await.len(), 1);
    server.abort();
}

#[tokio::test]
async fn background_deadline_clears_the_chat_wait_if_ordinary_close_confirms_failed_flush() {
    check_background_deadline(DeadlineCase::ShutdownDuringWaitWithFlushFailure).await;
}

#[tokio::test]
async fn background_deadline_retires_a_completed_chat_when_the_manager_missed_expiry() {
    check_background_deadline(DeadlineCase::OfflineAtDeadline).await;
}

#[tokio::test]
async fn background_deadline_trusts_confirmed_retirement_over_stale_native_cleanup() {
    check_background_deadline(DeadlineCase::RetiredNativeCleanup).await;
}

async fn check_background_deadline(case: DeadlineCase) {
    let (mut fixture, state, server) = jobs_fixture(Some(case)).await;
    let chat = fixture.start_chat(message("Start a background job")).await;
    fixture.start().await;
    let run = fixture.chat_run(&chat).await;
    fixture
        .until(&run, |run| run["sessionId"].is_string())
        .await;
    let plan = state.turns.lock().await[0].0.clone();
    let job = workspace_call(
        &plan,
        "start_job",
        json!({"name":"measurement","command":"exec sleep 120"}),
        71,
    )
    .await;
    let job_id = text(&job, "jobId");
    let waiting = matches!(
        case,
        DeadlineCase::Waiting
            | DeadlineCase::WaitingWithFlushFailure
            | DeadlineCase::ShutdownDuringWait
            | DeadlineCase::ShutdownDuringWaitWithFlushFailure
    );
    if waiting {
        let wait = workspace_call(
            &plan,
            "wait_for",
            json!({"conditions":[{"type":"job_finished","jobId":job_id},{"type":"delay","seconds":45}],"mode":"any","timeoutSeconds":55,"reason":"Await measurement"}),
            72,
        )
        .await;
        assert_eq!(wait["deferred"], true);
    }
    // The controller's deadline comes before the wait's own timeout. Keep the
    // worker clock fixed until the provider receipt and its success are saved.
    let clock = fixture.clock.as_ref().unwrap();
    let deadline = clock.millis + 10_000;
    let plan_path = fixture
        .service
        .config
        .data_dir
        .join("runner-plans")
        .join(format!("{}.json", text(&plan, "id")));
    let mut deadline_plan = plan.clone();
    deadline_plan["expires"] = deadline.into();
    tokio::fs::write(&plan_path, serde_json::to_vec(&deadline_plan).unwrap())
        .await
        .unwrap();
    tokio::fs::write(state.root.join("finish-provider"), "")
        .await
        .unwrap();
    let completed = fixture
        .until(&run, |run| run["status"] == "succeeded")
        .await;
    assert_eq!(completed["summary"], "Background work started");
    assert_eq!(fixture.worker_active_runs().await, Some(1));
    assert!(matches!(
        state.jobs.lock().await.status(job_id).unwrap(),
        Outcome::Running
    ));

    tokio::time::timeout(Duration::from_secs(10), state.activity.notified())
        .await
        .unwrap();
    if matches!(
        case,
        DeadlineCase::ShutdownDuringWait | DeadlineCase::ShutdownDuringWaitWithFlushFailure
    ) {
        let mut clock = fixture.clock.take().unwrap();
        let release_close = async {
            tokio::time::timeout(Duration::from_secs(10), state.close_started.notified())
                .await
                .unwrap();
            clock.advance(deadline - clock.millis + 1);
            state.hold_close.as_ref().unwrap().cancel();
        };
        tokio::join!(fixture.stop(false), release_close);
        let retired = fixture.service.store.run(&run).await.unwrap();
        let flush_failed = case == DeadlineCase::ShutdownDuringWaitWithFlushFailure;
        if flush_failed {
            assert_eq!(retired["status"], "failed");
            assert!(text(&retired, "error").contains("filesystem could not be flushed"));
        } else {
            assert_eq!(retired["status"], "succeeded");
            assert_eq!(retired["finishedAt"], completed["finishedAt"]);
        }
        assert!(fixture.checkpoint(&run).await["heldEnvironment"].is_null());
        let waiting = fixture
            .service
            .store
            .read(move |db| Ok(db.conversation(&chat)?.unwrap().wakeup.is_some()))
            .await
            .unwrap();
        assert_eq!(waiting, !flush_failed);
        assert!(state.deleted.load(Ordering::SeqCst));
        server.abort();
        return;
    }
    if case == DeadlineCase::OfflineAtDeadline {
        assert!(fixture.checkpoint(&run).await["heldEnvironment"].is_object());
        fixture.stop(true).await;
    }
    let clock = fixture.clock.as_mut().unwrap();
    clock.advance(deadline - clock.millis + 1);
    if let Some(disconnect) = &state.disconnect_activity {
        disconnect.cancel();
    }
    if let Some(hold) = &state.hold_close {
        tokio::time::timeout(Duration::from_secs(10), state.close_started.notified())
            .await
            .unwrap();
        let recovering = fixture.service.store.run(&run).await.unwrap();
        assert_eq!(recovering["recoveryPending"], true);
        assert_eq!(recovering["status"], "succeeded");
        assert_eq!(recovering["finishedAt"], completed["finishedAt"]);
        let saved = fixture.checkpoint(&run).await;
        assert_eq!(saved["settled"]["status"], "succeeded");
        assert!(saved["heldEnvironment"].is_null());
        assert!(!state.deleted.load(Ordering::SeqCst));
        let relay = common::relay_fixture::context(&fixture.service).await;
        reqwest::Client::new()
            .post(format!("{}/api/chats/{chat}/stop", fixture.url))
            .header("x-test-relay-token", text(&relay, "value"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        assert!(fixture.service.store.run(&run).await.unwrap()["cancelRequestedAt"].is_null());
        fixture.stop(true).await;
        hold.cancel();
        fixture.start().await;
    }
    if case == DeadlineCase::OfflineAtDeadline {
        fixture.start().await;
    }
    let expected_status = match case {
        DeadlineCase::Completed
        | DeadlineCase::DisconnectedDuringActivity
        | DeadlineCase::GatewayDuringActivity
        | DeadlineCase::GatewayTimeoutDuringActivity
        | DeadlineCase::RestartDuringFence
        | DeadlineCase::OfflineAtDeadline
        | DeadlineCase::RetiredNativeCleanup => "succeeded",
        DeadlineCase::InvalidActivity
        | DeadlineCase::InvalidClose
        | DeadlineCase::MalformedClose
        | DeadlineCase::UnexpectedCloseResponse
        | DeadlineCase::Waiting
        | DeadlineCase::FlushFailedOnDelete
        | DeadlineCase::FlushFailedOnWait
        | DeadlineCase::WaitingWithFlushFailure => "failed",
        DeadlineCase::ShutdownDuringWait | DeadlineCase::ShutdownDuringWaitWithFlushFailure => {
            unreachable!("shutdown verified while worker is stopped")
        }
    };
    let restarted = matches!(
        case,
        DeadlineCase::RestartDuringFence | DeadlineCase::OfflineAtDeadline
    );
    if restarted {
        fixture
            .until(&run, |run| {
                matches!(run["status"].as_str(), Some("succeeded" | "failed"))
                    && run["recoveryPending"] == false
                    && state.deleted.load(Ordering::SeqCst)
            })
            .await;
    }
    fixture.until_worker_quiescent().await;
    assert_eq!(state.closed.load(Ordering::SeqCst), !restarted);
    assert!(state.deleted.load(Ordering::SeqCst));
    assert_eq!(state.turns.lock().await.len(), 1);
    assert!(fixture.checkpoint(&run).await["heldEnvironment"].is_null());

    let retired = fixture.service.store.run(&run).await.unwrap();
    assert_eq!(retired["status"], expected_status, "{}", retired["error"]);
    assert_eq!(retired["recoveryPending"], false);
    let control_fault = match case {
        DeadlineCase::InvalidActivity | DeadlineCase::InvalidClose => {
            Some("Execution controller failed to complete the control request.")
        }
        DeadlineCase::MalformedClose | DeadlineCase::UnexpectedCloseResponse => {
            Some("Execution environment sent an invalid control frame.")
        }
        _ => None,
    };
    match case {
        DeadlineCase::InvalidActivity
        | DeadlineCase::InvalidClose
        | DeadlineCase::MalformedClose
        | DeadlineCase::UnexpectedCloseResponse => {
            assert_eq!(retired["error"], control_fault.unwrap());
        }
        DeadlineCase::Waiting => assert_eq!(
            retired["error"],
            "Environment background work reached its time limit."
        ),
        DeadlineCase::FlushFailedOnDelete
        | DeadlineCase::FlushFailedOnWait
        | DeadlineCase::WaitingWithFlushFailure => {
            assert!(text(&retired, "error").contains("filesystem could not be flushed"));
        }
        DeadlineCase::Completed
        | DeadlineCase::DisconnectedDuringActivity
        | DeadlineCase::GatewayDuringActivity
        | DeadlineCase::GatewayTimeoutDuringActivity
        | DeadlineCase::RestartDuringFence
        | DeadlineCase::OfflineAtDeadline
        | DeadlineCase::RetiredNativeCleanup => {
            assert_eq!(retired["summary"], completed["summary"]);
            assert_eq!(retired["finishedAt"], completed["finishedAt"]);
            assert!(retired["error"].is_null());
        }
        DeadlineCase::ShutdownDuringWait | DeadlineCase::ShutdownDuringWaitWithFlushFailure => {
            unreachable!("shutdown verified while worker is stopped")
        }
    }
    assert_eq!(retired["sessionId"], completed["sessionId"]);
    assert_eq!(retired["workspace"], completed["workspace"]);
    let job_id = job_id.to_owned();
    let (outcome, deadline_event): (String, String) = fixture
        .service
        .store
        .read(move |db| {
            let outcome = db.0.query_row(
                "SELECT json_extract(outcome,'$.state') FROM background_jobs WHERE job_id=?",
                [job_id],
                |row| row.get(0),
            )?;
            let deadline_event = db.0.query_row(
                "SELECT type FROM events WHERE run_id=? AND text=? ORDER BY id DESC LIMIT 1",
                [
                    run,
                    control_fault
                        .unwrap_or("Environment background work reached its time limit.")
                        .into(),
                ],
                |row| row.get(0),
            )?;
            Ok((outcome, deadline_event))
        })
        .await
        .unwrap();
    assert_eq!(outcome, "unknown");
    assert_eq!(
        deadline_event,
        if waiting || control_fault.is_some() {
            "error"
        } else {
            "diagnostic"
        }
    );
    fixture.stop(false).await;
    server.abort();
}
