use base64::Engine;
use std::{
    path::Path,
    time::{Duration, Instant},
};

use cairn_installation::{
    background_jobs::{Outcome, Registry},
    process::Environment,
};

fn deadline() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &raw mut time) },
        0
    );
    time.tv_sec as u64 * 1000 + time.tv_nsec as u64 / 1_000_000 + 5000
}

async fn finished(jobs: &Registry, id: &str) -> cairn_installation::background_jobs::Receipt {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        match jobs.status(id).unwrap() {
            Outcome::Finished { receipt } => return receipt,
            Outcome::Unknown { message } => panic!("{message}"),
            Outcome::Running => assert!(Instant::now() < deadline, "job failed to settle"),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn repeated_call_runs_once_in_original_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    let mut jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        Environment::new(),
        deadline(),
    );
    let command = "printf 'one\\n' >> executions; printf output; exit 7";
    let id = jobs
        .start("request-1", "measurement", command)
        .await
        .unwrap();
    assert_eq!(
        jobs.start("request-1", "measurement", command)
            .await
            .unwrap(),
        id
    );
    assert_eq!(jobs.lookup("request-1"), Some(id.clone()));
    assert_eq!(jobs.lookup("missing"), None);
    let receipt = finished(&jobs, &id).await;
    assert_eq!(receipt.exit_code, Some(7));
    assert_eq!(receipt.log_tail, "output");
    assert_eq!(
        std::fs::read_to_string(directory.path().join("executions")).unwrap(),
        "one\n"
    );
    assert!(!jobs.active());
    assert!(
        jobs.start("request-1", "measurement", "true")
            .await
            .is_err()
    );
    jobs.stop();
    assert_eq!(
        jobs.start("request-1", "measurement", command)
            .await
            .unwrap(),
        id
    );
    assert!(jobs.start("request-2", "new", "true").await.is_err());
}

#[tokio::test]
async fn stopping_registry_reaps_command_and_reports_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let mut jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        Environment::new(),
        deadline(),
    );
    let id = jobs
        .start("request-1", "measurement", "echo $$ > pid; exec sleep 30")
        .await
        .unwrap();
    let started = Instant::now();
    while !directory.path().join("pid").exists() {
        assert!(started.elapsed() < Duration::from_secs(3));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pid = std::fs::read_to_string(directory.path().join("pid")).unwrap();
    jobs.stop();
    let receipt = finished(&jobs, &id).await;
    assert!(receipt.stopped);
    assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
    assert!(!jobs.active());
}

#[tokio::test]
async fn planning_environment_refuses_background_commands() {
    use cairn_installation::background_jobs::{
        channel::Channel,
        environment,
        protocol::{Event, Request},
    };
    let directory = tempfile::tempdir().unwrap();
    let plan = serde_json::json!({"id":"attempt","runId":"run","sandbox":"workspace-write","chat":{"planning":true,"sandbox":"read-only"}});
    let jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        Environment::new(),
        deadline(),
    );
    let (controller, workload) = tokio::net::UnixStream::pair().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let worker_stop = stop.clone();
    let serving = tokio::spawn(async move {
        environment::serve(workload, plan, jobs, worker_stop, |_| {
            Box::pin(async { panic!("No provider requested") })
        })
        .await
    });
    let (channel, mut events) = Channel::attach(controller);
    assert!(matches!(events.recv().await, Some(Event::Ready { .. })));
    let reply = channel
        .request(Request::StartJob {
            request_id: "call".into(),
            name: "write".into(),
            command: "touch bypass".into(),
        })
        .await
        .unwrap();
    assert!(matches!(reply, Event::Refused { .. }));
    assert!(!directory.path().join("bypass").exists());
    channel
        .request(Request::Close {
            request_id: "close".into(),
        })
        .await
        .unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_job_survives_two_provider_turns_in_the_same_environment() {
    use cairn_installation::background_jobs::{
        channel::Channel,
        environment,
        protocol::{Event, Request},
    };
    let directory = tempfile::tempdir().unwrap();
    let credentials = directory.path().join("credentials");
    std::fs::create_dir(&credentials).unwrap();
    let plan = serde_json::json!({"id": "attempt", "runId": "run", "cwd": directory.path(),"chat":{"claudeManagedAuth":true,"claudeCredentialsDirectory":credentials}});
    let jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        Environment::new(),
        deadline(),
    );
    let (controller, workload) = tokio::net::UnixStream::pair().unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let worker_stop = stop.clone();
    let initial = plan.clone();
    let serving = tokio::spawn(async move {
        environment::serve(workload, initial, jobs, worker_stop, |plan| {
            let cwd = plan["cwd"].as_str().unwrap().to_owned();
            Box::pin(async move {
                Ok(environment::Launch {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "printf synthetic-token > credentials/.credentials.json; setsid sh -c 'echo $$ > provider-child; exec sleep 30' >/dev/null 2>&1 & sleep 0.1; test -z \"${HOME+x}\" || exit 91; printf provider-output".into()],
                    cwd: cwd.into(), environment: Environment::new(), prompt: String::new(),
                })
            })
        })
        .await
    });
    let (channel, mut events) = Channel::attach(controller);
    assert!(matches!(events.recv().await, Some(Event::Ready { .. })));
    let start = channel
        .request(Request::StartJob {
            request_id: "job-start".into(),
            name: "measurement".into(),
            command: "sleep 1; printf job-finished".into(),
        })
        .await
        .unwrap();
    let Event::JobStarted { job_id, .. } = start else {
        panic!("job not admitted");
    };
    let mut changed = plan.clone();
    changed["chat"] = serde_json::json!({"sandbox": "danger-full-access", "writableRoots": ["/"]});
    assert!(matches!(
        channel
            .request(Request::ResumeTurn {
                request_id: "widen".into(),
                turn_id: "widen".into(),
                plan: changed
            })
            .await
            .unwrap(),
        Event::Refused { .. }
    ));
    for turn in ["one", "two"] {
        assert!(matches!(
            channel
                .request(Request::ResumeTurn {
                    request_id: turn.into(),
                    turn_id: turn.into(),
                    plan: plan.clone()
                })
                .await
                .unwrap(),
            Event::Accepted { .. }
        ));
        let mut output = Vec::new();
        let mut started = false;
        loop {
            let event = tokio::time::timeout(Duration::from_secs(3), events.recv())
                .await
                .unwrap()
                .unwrap();
            match event {
                Event::TurnStarted { turn_id } => {
                    assert_eq!(turn_id, turn);
                    assert!(!started);
                    started = true;
                }
                Event::Output { data, .. } => {
                    assert!(started, "output must follow its start boundary");
                    output.extend(
                        base64::engine::general_purpose::STANDARD
                            .decode(data)
                            .unwrap(),
                    );
                }
                Event::TurnFinished { turn_id, code } => {
                    assert!(started);
                    assert_eq!(turn_id, turn);
                    assert_eq!(code, 0);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(output, b"provider-output");
        assert!(
            !credentials.join(".credentials.json").exists(),
            "provider credentials must be cleared before completion"
        );
        let child = std::fs::read_to_string(directory.path().join("provider-child")).unwrap();
        assert!(!Path::new(&format!("/proc/{}", child.trim())).exists());
        assert!(matches!(
            channel
                .request(Request::ResumeTurn {
                    request_id: format!("retry-{turn}"),
                    turn_id: turn.into(),
                    plan: plan.clone()
                })
                .await
                .unwrap(),
            Event::Accepted { .. }
        ));
    }
    let status = channel
        .request(Request::JobStatus {
            request_id: "status".into(),
            job_id: job_id.clone(),
        })
        .await
        .unwrap();
    assert!(matches!(
        status,
        Event::JobStatus {
            outcome: Outcome::Running,
            ..
        }
    ));
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), events.recv())
            .await
            .unwrap()
            .unwrap();
        if let Event::JobChanged {
            job_id: observed,
            outcome: Outcome::Finished { receipt },
        } = event
        {
            assert_eq!(observed, job_id);
            assert_eq!(receipt.exit_code, Some(0));
            assert_eq!(receipt.log_tail, "job-finished");
            break;
        }
    }
    assert!(matches!(
        channel
            .request(Request::Close {
                request_id: "close".into()
            })
            .await
            .unwrap(),
        Event::Accepted { .. }
    ));
    tokio::time::timeout(Duration::from_secs(4), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn close_waits_for_job_termination_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let mut jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        Environment::new(),
        deadline(),
    );
    let id = jobs
        .start(
            "request",
            "delayed-stop",
            "trap '' TERM; echo $$ > pid; while :; do sleep 1; done",
        )
        .await
        .unwrap();
    let started = Instant::now();
    while !directory.path().join("pid").exists() {
        assert!(started.elapsed() < Duration::from_secs(3));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pid = std::fs::read_to_string(directory.path().join("pid")).unwrap();
    jobs.close().await.unwrap();
    assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
    assert!(matches!(
        jobs.status(&id).unwrap(),
        Outcome::Finished { .. }
    ));
}

#[tokio::test]
async fn lost_provider_supervisor_requires_environment_fencing() {
    use cairn_installation::background_jobs::{
        channel::Channel,
        environment,
        protocol::{Event, Request},
    };
    let directory = tempfile::tempdir().unwrap();
    let plan = serde_json::json!({"id": "attempt", "runId": "run", "cwd": directory.path()});
    let jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        Environment::new(),
        deadline(),
    );
    let (controller, workload) = tokio::net::UnixStream::pair().unwrap();
    let initial = plan.clone();
    let serving = tokio::spawn(async move {
        environment::serve(
            workload,
            initial,
            jobs,
            tokio_util::sync::CancellationToken::new(),
            |plan| {
                let cwd = plan["cwd"].as_str().unwrap().to_owned();
                Box::pin(async move {
                    Ok(environment::Launch {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "kill -KILL $PPID; exit 0".into()],
                        cwd: cwd.into(),
                        environment: Environment::new(),
                        prompt: String::new(),
                    })
                })
            },
        )
        .await
    });
    let (channel, mut events) = Channel::attach(controller);
    assert!(matches!(events.recv().await, Some(Event::Ready { .. })));
    assert!(matches!(
        channel
            .request(Request::ResumeTurn {
                request_id: "turn".into(),
                turn_id: "turn".into(),
                plan: plan.clone()
            })
            .await
            .unwrap(),
        Event::Accepted { .. }
    ));
    let failure = tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(failure.message.contains("fencing"));
    assert!(
        channel
            .request(Request::ResumeTurn {
                request_id: "again".into(),
                turn_id: "again".into(),
                plan
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unknown_job_stops_new_admission_and_requires_fencing() {
    let directory = tempfile::tempdir().unwrap();
    let mut jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        Environment::new(),
        deadline(),
    );
    let id = jobs
        .start("lost", "lost", "kill -KILL $PPID; exit 0")
        .await
        .unwrap();
    let start = Instant::now();
    loop {
        if matches!(jobs.status(&id).unwrap(), Outcome::Unknown { .. }) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(jobs.start("new", "new", "true").await.is_err());
    assert!(jobs.close().await.unwrap_err().message.contains("fencing"));
}

#[tokio::test]
async fn workload_loader_variables_never_reach_private_helper_socket() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("probe.c");
    let library = directory.path().join("probe.so");
    std::fs::write(
        &source,
        r#"#include <sys/socket.h>
#include <stdlib.h>
#include <unistd.h>
__attribute__((constructor)) static void probe(void) {
  int type; socklen_t size=sizeof(type);
  if (getsockopt(3,SOL_SOCKET,SO_TYPE,&type,&size)==0) _exit(91);
}
"#,
    )
    .unwrap();
    assert!(
        std::process::Command::new("cc")
            .args(["-shared", "-fPIC", "-o"])
            .arg(&library)
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    let environment = Environment::from([
        ("LD_PRELOAD".into(), library.to_str().unwrap().into()),
        ("WORKLOAD_VALUE".into(), "kept".into()),
    ]);
    let mut jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        environment,
        deadline(),
    );
    let id = jobs
        .start(
            "loader-test",
            "loader test",
            "printf %s \"$WORKLOAD_VALUE\"",
        )
        .await
        .unwrap();
    let receipt = finished(&jobs, &id).await;
    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(receipt.log_tail, "kept");
}

#[tokio::test]
async fn unknown_job_interrupts_provider_preparation_before_launch() {
    use cairn_installation::background_jobs::{
        channel::Channel,
        environment,
        protocol::{Event, Request},
    };
    use std::sync::Arc;
    let directory = tempfile::tempdir().unwrap();
    let plan = serde_json::json!({"id": "attempt", "runId": "run", "cwd": directory.path()});
    let jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        Environment::new(),
        deadline(),
    );
    let (controller, workload) = tokio::net::UnixStream::pair().unwrap();
    let initial = plan.clone();
    let preparing = Arc::new(tokio::sync::Notify::new());
    let entered = preparing.clone();
    let serving = tokio::spawn(async move {
        environment::serve(
            workload,
            initial,
            jobs,
            tokio_util::sync::CancellationToken::new(),
            move |_| {
                entered.notify_one();
                Box::pin(std::future::pending())
            },
        )
        .await
    });
    let (channel, mut events) = Channel::attach(controller);
    assert!(matches!(events.recv().await, Some(Event::Ready { .. })));
    channel
        .request(Request::StartJob {
            request_id: "job".into(),
            name: "job".into(),
            command: "sleep 0.3; kill -KILL $PPID; exit 0".into(),
        })
        .await
        .unwrap();
    let request = tokio::spawn(async move {
        channel
            .request(Request::ResumeTurn {
                request_id: "turn".into(),
                turn_id: "turn".into(),
                plan,
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), preparing.notified())
        .await
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.message.contains("fencing"));
    assert!(request.await.unwrap().is_err());
}

#[tokio::test]
async fn controller_disconnect_interrupts_preparation() {
    use cairn_installation::{
        background_jobs::{environment, protocol::Request},
        microvm::wire,
    };
    use std::sync::Arc;
    let directory = tempfile::tempdir().unwrap();
    let plan = serde_json::json!({"id": "attempt", "runId": "run"});
    let jobs = Registry::new(
        Path::new(env!("CARGO_BIN_EXE_cairn")),
        directory.path(),
        Environment::new(),
        deadline(),
    );
    let (mut controller, workload) = tokio::net::UnixStream::pair().unwrap();
    let preparing = Arc::new(tokio::sync::Notify::new());
    let entered = preparing.clone();
    let initial = plan.clone();
    let serving = tokio::spawn(async move {
        environment::serve(
            workload,
            initial,
            jobs,
            tokio_util::sync::CancellationToken::new(),
            move |_| {
                entered.notify_one();
                Box::pin(std::future::pending())
            },
        )
        .await
    });
    let mut reader = tokio::io::BufReader::new(&mut controller);
    wire::read(&mut reader).await.unwrap().unwrap();
    drop(reader);
    wire::write(
        &mut controller,
        &Request::ResumeTurn {
            request_id: "turn".into(),
            turn_id: "turn".into(),
            plan,
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), preparing.notified())
        .await
        .unwrap();
    drop(controller);
    let error = tokio::time::timeout(Duration::from_secs(2), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.message.contains("disconnected"));
}
