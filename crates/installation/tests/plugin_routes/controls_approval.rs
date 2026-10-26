//! A real loaded Luau handler keeps its package approval across an HTTP await.
use super::*;

async fn control_retains_loaded_approval(control: &str) {
    for retire in [false, true] {
        let mut f = Started::new(None).await;
        let chat = text(&f.delivery, "chatId").to_owned();
        let question = "c".repeat(64);
        if control == "answer" {
            f.s.question_receive(
                text(&f.run, "id"),
                json!({
                    "id": question, "blocking": false,
                    "fields": [{"id": "choice", "title": "Continue?", "options": []}],
                }),
            )
            .await
            .unwrap();
        } else if control == "reset" {
            f.s.worker.cancel(&f.s, text(&f.run, "id")).await.unwrap();
            runtime::pump(&f.s, &f.connection, f.session.as_mut().unwrap())
                .await
                .unwrap();
            assert!(f.s.store.list(work::DELIVERIES).await.unwrap().is_empty());
        }
        let before = f.s.chat_detail(&chat).await.unwrap();
        let owner_key = format!("plugin-conversation:{chat}");
        let owner_before = f.s.store.kv(&owner_key).await.unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (ready, resume) = (entered.clone(), release.clone());
        let provider = axum::Router::new()
            .route(
                "/control-barrier",
                axum::routing::get(move || {
                    let (ready, resume) = (ready.clone(), resume.clone());
                    async move {
                        ready.notify_one();
                        resume.notified().await;
                        axum::Json(json!({"ok": true}))
                    }
                }),
            )
            .fallback(axum::routing::any(|| async {
                axum::Json(json!({"ok": true}))
            }));
        let (origin, provider) = common::serve_locally(provider).await;
        f.s.plugins.route_http_to(Some(origin));
        let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
        let mut payload =
            json!({"chat": chat, "delivery": f.delivery["id"], "controlBarrier": true});
        if control == "answer" {
            payload["question"] = json!(question);
        }
        if control == "reset" {
            payload["reset"] = json!(true);
        }
        let response = deliver(&app, &f.connection.id, json!({
            "id": "loaded-control", "kind": "interaction", "resource": f.delivery["reply"]["resource"],
            "author": {"id": "alice"}, "context": {"channel": "C1", "thread": "original-thread"}, "body": payload,
        })).await;
        assert_eq!(response.1["status"], "accepted", "{response:?}");
        let (s, connection) = (f.s.clone(), f.connection.clone());
        let mut session = f.session.take().unwrap();
        let handling = tokio::spawn(async move {
            let result = runtime::pump(&s, &connection, &mut session).await;
            (session, result)
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        if retire {
            let package = registry::active(&f.s, "router").await.unwrap();
            registry::approve(&f.s, "router", "1", &package.digest)
                .await
                .unwrap();
        }
        release.notify_one();
        let (session, _) = handling.await.unwrap();
        let after = f.s.chat_detail(&chat).await.unwrap();
        let owner_after = f.s.store.kv(&owner_key).await.unwrap();
        session.stop().await;
        provider.abort();
        if retire {
            match control {
                "answer" => assert_eq!(
                    after["questions"][0]["status"], before["questions"][0]["status"],
                    "A retired loaded handler answered the question"
                ),
                "cancel" => {
                    assert_eq!(
                        after["run"]["status"], before["run"]["status"],
                        "A retired loaded handler stopped the run"
                    );
                    assert_eq!(
                        after["run"]["cancelRequestedAt"],
                        before["run"]["cancelRequestedAt"]
                    );
                    assert_eq!(after["paused"], before["paused"]);
                }
                "reset" => assert_eq!(
                    owner_after, owner_before,
                    "A retired loaded handler reset its source binding"
                ),
                _ => unreachable!(),
            }
        } else {
            match control {
                "answer" => assert_eq!(after["questions"][0]["status"], "answering"),
                "cancel" => assert!(
                    after["run"]["status"] == "cancelled"
                        || after["run"]["cancelRequestedAt"].is_number(),
                    "{after}"
                ),
                "reset" => assert!(owner_after.unwrap()["retiredAt"].is_number()),
                _ => unreachable!(),
            }
        }
        f.s.shutdown.cancel();
    }
}

#[tokio::test]
async fn a_stale_loaded_handler_cannot_cancel() {
    control_retains_loaded_approval("cancel").await;
}

#[tokio::test]
async fn a_stale_loaded_handler_cannot_answer() {
    control_retains_loaded_approval("answer").await;
}

#[tokio::test]
async fn a_stale_loaded_handler_cannot_reset() {
    control_retains_loaded_approval("reset").await;
}

#[tokio::test]
async fn a_stale_loaded_handler_cannot_cancel_after_the_native_active_lock_wait() {
    for retire in [false, true] {
        let mut f = Started::new(None).await;
        let package = registry::active(&f.s, "router").await.unwrap();
        let mut manifest = serde_json::to_value(&package.manifest).unwrap();
        manifest["version"] = json!("2");
        let replacement = registry::install(
            &f.s,
            &json!({
                "manifest": manifest, "source": package.source.main,
            }),
        )
        .await
        .unwrap();
        let task = f.s.task(json!({
            "name": "Hold the native active guard", "prompt": "Fixture only", "worktree": false,
            "agentId": f.run["snapshot"]["agent"]["id"],
        }), None).await.unwrap();
        let other =
            f.s.enqueue(text(&task, "id"), "manual", None)
                .await
                .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (ready, resume) = (entered.clone(), release.clone());
        let provider = axum::Router::new().route(
            "/control-barrier",
            axum::routing::get(move || {
                let (ready, resume) = (ready.clone(), resume.clone());
                async move {
                    ready.notify_one();
                    resume.notified().await;
                    axum::Json(json!({"ok": true}))
                }
            }),
        );
        let (origin, provider) = common::serve_locally(provider).await;
        f.s.plugins.route_http_to(Some(origin));
        let app = cairn_installation::http::router(f.s.clone()).await.unwrap();
        let response = deliver(&app, &f.connection.id, json!({
            "id": "active-guard-control", "kind": "interaction", "resource": f.delivery["reply"]["resource"],
            "author": {"id": "alice"}, "context": {"channel": "C1", "thread": "original-thread"},
            "body": {"chat": f.delivery["chatId"], "delivery": f.delivery["id"], "controlBarrier": true},
        })).await;
        assert_eq!(response.1["status"], "accepted", "{response:?}");
        let (s, connection) = (f.s.clone(), f.connection.clone());
        let mut session = f.session.take().unwrap();
        let mut handling = tokio::spawn(async move {
            let result = runtime::pump(&s, &connection, &mut session).await;
            (session, result)
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        let (writer_ready, writer_started) = tokio::sync::oneshot::channel();
        let (writer_release, writer_resume) = std::sync::mpsc::channel();
        let store = f.s.store.clone();
        let writer = tokio::spawn(async move {
            store
                .transaction(move |_| {
                    writer_ready.send(()).unwrap();
                    writer_resume.recv().unwrap();
                    Ok(())
                })
                .await
        });
        writer_started.await.unwrap();
        // This public cancellation acquires the otherwise idle native active
        // guard, then queues its write behind the held writer. Poll it exactly
        // once; keeping the pending future retains that real guard without a
        // private-field hook or an invented worker cancellation.
        let mut other_cancel = Box::pin(f.s.worker.cancel(&f.s, text(&other, "id")));
        let held = futures_util::poll!(other_cancel.as_mut()).is_pending();
        writer_release.send(()).unwrap();
        writer.await.unwrap().unwrap();
        // Leave other_cancel unpolled: it still owns the active guard, while
        // every handler read/write check is now free to finish before approval.
        release.notify_one();
        // A bounded pending witness lets the actual handler finish its read
        // checks and reach cancel_authorized's active-lock wait. The RED with
        // a different version proves this reached the vulnerable await: its
        // ordinary pre-call digest check would otherwise reject the operation.
        let pending =
            tokio::time::timeout(std::time::Duration::from_millis(200), &mut handling).await;
        if retire {
            registry::approve(&f.s, "router", "2", text(&replacement, "digest"))
                .await
                .unwrap();
        }
        other_cancel.await.unwrap();
        let waiting = pending.is_err();
        let (session, _) = match pending {
            Ok(result) => result.unwrap(),
            Err(_) => handling.await.unwrap(),
        };
        let run = f.s.store.run(text(&f.run, "id")).await.unwrap();
        session.stop().await;
        provider.abort();
        assert!(
            held,
            "The public native cancellation did not hold the active guard"
        );
        assert!(
            waiting,
            "The control did not wait for the native active guard"
        );
        if retire {
            assert_eq!(
                run["status"], "running",
                "A retired package stopped the run after waiting for the active guard"
            );
            assert!(run["cancelRequestedAt"].is_null());
        } else {
            assert_eq!(
                run["status"], "cancelled",
                "The positive control did not actually stop the native run"
            );
        }
        f.s.shutdown.cancel();
    }
}
