//! Native installation mutations retain the source that authenticated their HTTP request.
use super::*;
use std::convert::Infallible;

#[tokio::test]
async fn native_task_creation_keeps_its_source_across_the_author_lock() {
    for retire in [false, true] {
        let f = Started::with_access(None, "http://runner:4311", true).await;
        let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
        let before = f.s.store.list("tasks").await.unwrap();
        let input = json!({
            "name": "Source-bound task", "prompt": "Fixture task creation only", "worktree": false,
            "agentId": f.run["snapshot"]["agent"]["id"],
        });
        let frame = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": common::mcp_arguments("create_task", &input),
        })
        .to_string();
        let author = f.s.task_author_lock.clone().lock_owned().await;
        let entered = Arc::new(Notify::new());
        let ready = entered.clone();
        let body = Body::from_stream(futures_util::stream::once(async move {
            // Installation endpoint authentication (and its source capture)
            // precedes the first body poll on the actual HTTP route.
            ready.notify_one();
            Ok::<_, Infallible>(axum::body::Bytes::from(frame))
        }));
        let token = f.bearer.clone();
        let mut pending = tokio::spawn(async move {
            super::entry_points::call(&app, &token, "/mcp-installation", body).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(200), &mut pending).await;
        if retire {
            let package = registry::active(&f.s, "router").await.unwrap();
            registry::approve(&f.s, "router", "1", &package.digest)
                .await
                .unwrap();
        }
        drop(author);
        let waiting = blocked.is_err();
        let response = match blocked {
            Ok(result) => result.unwrap(),
            Err(_) => pending.await.unwrap(),
        };
        let after = f.s.store.list("tasks").await.unwrap();
        f.s.shutdown.cancel();
        assert!(
            waiting,
            "The actual native call never waited for its author lock: {response:?}"
        );
        if retire {
            assert_eq!(
                after, before,
                "An original native request borrowed a replacement source approval to create a task: {response:?}"
            );
        } else {
            assert_eq!(response.0, StatusCode::OK, "{response:?}");
            assert_eq!(
                after.len(),
                before.len() + 1,
                "The positive native create never committed: {response:?}"
            );
            assert!(after.iter().any(|task| task["name"] == "Source-bound task"));
        }
    }
}

async fn native_call(
    f: &Started,
    name: &str,
    args: Value,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": common::mcp_arguments(name, &args)}).to_string();
    let entered = Arc::new(Notify::new());
    let ready = entered.clone();
    let body = Body::from_stream(futures_util::stream::once(async move {
        ready.notify_one();
        Ok::<_, Infallible>(axum::body::Bytes::from(frame))
    }));
    let token = f.bearer.clone();
    let pending = tokio::spawn(async move {
        super::entry_points::call(&app, &token, "/mcp-installation", body).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    pending
}

async fn task(f: &Started, name: &str) -> Value {
    f.s.task(json!({"name": name, "prompt": "Fixture native mutation", "agentId": f.run["snapshot"]["agent"]["id"], "worktree": false}), None).await.unwrap()
}

#[tokio::test]
async fn native_run_cancellation_keeps_its_source_across_the_active_lock() {
    for retire in [false, true] {
        let f = Started::with_access(None, "http://runner:4311", true).await;
        let target_task = task(&f, "Actual cancellation target").await;
        let target =
            f.s.enqueue(text(&target_task, "id"), "manual", None)
                .await
                .unwrap();
        let guard_task = task(&f, "Hold the native guard").await;
        let other =
            f.s.enqueue(text(&guard_task, "id"), "manual", None)
                .await
                .unwrap();
        let (ready, entered) = tokio::sync::oneshot::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let store = f.s.store.clone();
        let writer = tokio::spawn(async move {
            store
                .transaction(move |_| {
                    ready.send(()).unwrap();
                    resume.recv().unwrap();
                    Ok(())
                })
                .await
        });
        entered.await.unwrap();
        let mut other_cancel = Box::pin(f.s.worker.cancel(&f.s, text(&other, "id")));
        let held = futures_util::poll!(other_cancel.as_mut()).is_pending();
        release.send(()).unwrap();
        writer.await.unwrap().unwrap();
        // The unpolled public cancellation retains the actual active guard, but
        // the writer is free for every native authorization read/write.
        let mut pending = native_call(&f, "cancel_run", json!({"runId": target["id"]})).await;
        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(200), &mut pending).await;
        if retire {
            let package = registry::active(&f.s, "router").await.unwrap();
            registry::approve(&f.s, "router", "1", &package.digest)
                .await
                .unwrap();
        }
        other_cancel.await.unwrap();
        let waiting = blocked.is_err();
        let response = match blocked {
            Ok(result) => result.unwrap(),
            Err(_) => pending.await.unwrap(),
        };
        let after = f.s.store.run(text(&target, "id")).await.unwrap();
        f.s.shutdown.cancel();
        assert!(
            held && waiting,
            "The native call did not wait at the active guard: {response:?}"
        );
        if retire {
            assert_eq!(
                after["status"], "queued",
                "A retired native request cancelled another execution: {response:?}"
            );
            assert!(after["cancelRequestedAt"].is_null());
        } else {
            assert_eq!(
                after["status"], "cancelled",
                "The positive native call did not cancel: {response:?}"
            );
        }
    }
}

#[tokio::test]
async fn native_task_run_keeps_its_source_while_waiting_for_the_writer() {
    for retire in [false, true] {
        let f = Started::with_access(None, "http://runner:4311", true).await;
        let target = task(&f, "Actual run target").await;
        let (ready, entered) = tokio::sync::oneshot::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let store = f.s.store.clone();
        let writer = tokio::spawn(async move {
            store
                .transaction(move |db| {
                    let mut approval = db.get("plugin-packages", "package:router")?.unwrap();
                    ready.send(()).unwrap();
                    resume.recv().unwrap();
                    if retire {
                        // Replace the real approval record's epoch in the writer
                        // that the native queue operation is waiting behind. The
                        // manifest, digest, route, person and run grant stay intact.
                        approval["revision"] = json!(cairn_installation::config::id());
                        approval["updatedAt"] = json!(now());
                        db.put("plugin-packages", &approval)?;
                    }
                    Ok(())
                })
                .await
        });
        entered.await.unwrap();
        let mut pending = native_call(&f, "run_task", json!({"taskId": target["id"]})).await;
        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(200), &mut pending).await;
        release.send(()).unwrap();
        writer.await.unwrap().unwrap();
        let waiting = blocked.is_err();
        let response = match blocked {
            Ok(result) => result.unwrap(),
            Err(_) => pending.await.unwrap(),
        };
        let id = text(&target, "id").to_owned();
        let runs =
            f.s.store
                .read(move |db| db.runs(None, Some(&id), 100, 0, true))
                .await
                .unwrap();
        f.s.shutdown.cancel();
        assert!(
            waiting,
            "The native call never waited for the writer: {response:?}"
        );
        if retire {
            assert!(
                runs.is_empty(),
                "A retired native request queued an execution: {response:?}"
            );
        } else {
            assert_eq!(
                runs.len(),
                1,
                "The positive native call never queued: {response:?}"
            );
            assert_eq!(runs[0]["status"], "queued");
        }
    }
}

#[tokio::test]
async fn native_task_update_keeps_its_source_across_the_author_lock() {
    for retire in [false, true] {
        let f = Started::with_access(None, "http://runner:4311", true).await;
        let input = json!({
            "name": "Original source task", "prompt": "Fixture source-bound commitment", "worktree": false,
            "agentId": f.run["snapshot"]["agent"]["id"],
        });
        let first = native_call(&f, "create_task", input).await.await.unwrap();
        assert_eq!(first.0, StatusCode::OK, "{first:?}");
        let original = &first.1["result"]["structuredContent"]["result"];
        assert_eq!(original["name"], "Original source task", "{first:?}");
        let mut changed = original.clone();
        changed["name"] = json!("Updated source task");
        let guard = f.s.task_author_lock.clone().lock_owned().await;
        let mut pending = native_call(
            &f,
            "update_task",
            json!({"id": original["id"], "task": changed}),
        )
        .await;
        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(200), &mut pending).await;
        if retire {
            let package = registry::active(&f.s, "router").await.unwrap();
            registry::approve(&f.s, "router", "1", &package.digest)
                .await
                .unwrap();
        }
        drop(guard);
        let waiting = blocked.is_err();
        let response = match blocked {
            Ok(result) => result.unwrap(),
            Err(_) => pending.await.unwrap(),
        };
        let after = f.s.get("tasks", text(original, "id")).await.unwrap();
        f.s.shutdown.cancel();
        assert!(
            waiting,
            "The native update never waited for the author lock: {response:?}"
        );
        if retire {
            assert_eq!(
                after, *original,
                "A retired native update changed the task: {response:?}"
            );
            assert!(
                response.1["result"]["isError"] == true || response.1.get("error").is_some(),
                "{response:?}"
            );
        } else {
            assert_eq!(after["name"], "Updated source task", "{response:?}");
        }
    }
}
