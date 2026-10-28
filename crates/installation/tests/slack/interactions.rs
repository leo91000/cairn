//! Actual provider questions and cancellation through the shipped Luau package.
use crate::common::AgentFixture;

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_requester_can_answer_the_native_question_over_signed_http() {
    exchange(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_requester_can_stop_the_native_question_over_signed_http() {
    exchange(false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_requester_can_answer_the_native_question_over_verified_wss() {
    exchange(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_requester_can_stop_the_native_question_over_verified_wss() {
    exchange(true, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_requester_can_stop_ordinary_work_over_signed_http() {
    ordinary_progress_stop(false, ProgressBoundary::Confirmed).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_requester_can_stop_ordinary_work_over_verified_wss() {
    ordinary_progress_stop(true, ProgressBoundary::Confirmed).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_work_still_replies_and_retires_its_control_when_cleanup_is_interrupted() {
    ordinary_progress_stop(false, ProgressBoundary::InterruptedCleanup).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uncertain_progress_edit_is_not_repeated_and_its_confirmed_stop_stays_usable() {
    ordinary_progress_stop(false, ProgressBoundary::InterruptedEdit).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_progress_stop_acks_before_metadata_and_rechecks_current_visibility() {
    ordinary_progress_stop(false, ProgressBoundary::PrivateAfterAck).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_progress_stop_acks_before_metadata_and_rechecks_current_visibility() {
    ordinary_progress_stop(true, ProgressBoundary::PrivateAfterAck).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_the_requester_after_progress_stop_admission_prevents_cancellation() {
    ordinary_progress_stop(false, ProgressBoundary::RevokedAfterAck).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_progress_retries_a_definitely_rejected_creation() {
    let mut live = Live::start(false).await;
    live.source["event"]["text"] = json!("<@UBOT> fixture:chat-hang");
    live.slack.first_post_retry_after.store(2, Ordering::SeqCst);
    live.slack.rate_limit_first.store(true, Ordering::SeqCst);
    live.source().await;
    common::eventually(
        Duration::from_secs(35),
        Duration::from_millis(20),
        async || {
            live.slack
                .posts
                .lock()
                .await
                .iter()
                .any(|post| {
                    post["channel"] == "CTEST"
                        && post["thread_ts"] == live.source["event"]["ts"]
                        && post["text"].contains("Working")
                })
                .then_some(())
        },
    )
    .await;
    assert_eq!(live.slack.post_attempts.load(Ordering::SeqCst), 2);
    assert_eq!(live.slack.posts.lock().await.len(), 1);
    let times = live.slack.post_times.lock().await;
    assert!(times[1].duration_since(times[0]) >= Duration::from_secs(2));
    drop(times);
    live.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_progress_creation_is_not_repeated_before_or_after_restart() {
    let mut live = Live::start(false).await;
    live.source["event"]["text"] = json!("<@UBOT> fixture:chat-hang");
    live.slack.uncertain_first.store(true, Ordering::SeqCst);
    live.source().await;
    common::eventually(
        Duration::from_secs(25),
        Duration::from_millis(20),
        async || {
            live.slack
                .posts
                .lock()
                .await
                .iter()
                .any(|post| post["text"].contains("Working"))
                .then_some(())
        },
    )
    .await;
    // Exercise another actual eight-second progress poll before reopening.
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(live.slack.post_attempts.load(Ordering::SeqCst), 1);
    let chats = live.service.chat_list().await.unwrap();
    assert_eq!(chats.len(), 1);
    let chat = text(&chats[0], "id").to_owned();
    stop_manager(&mut live.manager).await;
    live.manager =
        start_manager_socket(&live._root.path().join("config.json"), &live.origin, None).await;
    common::eventually(
        Duration::from_secs(25),
        Duration::from_millis(20),
        async || {
            let detail = live.service.chat_detail(&chat).await.ok()?;
            (detail["run"]["status"] == "succeeded").then_some(())
        },
    )
    .await;
    common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || {
            live.slack
                .posts
                .lock()
                .await
                .iter()
                .any(|post| post["text"].contains("I reviewed the workspace"))
                .then_some(())
        },
    )
    .await;
    let posts = live.slack.posts.lock().await;
    assert_eq!(
        posts
            .iter()
            .filter(|post| post["text"].starts_with("Working"))
            .count(),
        1
    );
    drop(posts);
    live.finish().await;
}

enum ProgressBoundary {
    Confirmed,
    InterruptedEdit,
    InterruptedCleanup,
    PrivateAfterAck,
    RevokedAfterAck,
}

async fn ordinary_progress_stop(socket_mode: bool, boundary: ProgressBoundary) {
    let mut live = Live::start(socket_mode).await;
    live.slack.interrupt_update_body.store(
        matches!(boundary, ProgressBoundary::InterruptedEdit),
        Ordering::SeqCst,
    );
    live.source["event"]["text"] = json!(
        "<@UBOT> fixture:chat-hang fixture:progress-step PRIVATE_NATIVE_REQUEST_DO_NOT_EXPORT"
    );
    live.source().await;
    let post = common::eventually(
        Duration::from_secs(25),
        Duration::from_millis(20),
        async || {
            live.slack
                .posts
                .lock()
                .await
                .iter()
                .find(|post| {
                    post["text"].contains("Working")
                        && post
                            .get("blocks")
                            .is_some_and(|blocks| blocks.contains("cairn_stop"))
                })
                .cloned()
        },
    )
    .await;
    assert_eq!(post["channel"], "CTEST");
    assert_eq!(post["thread_ts"], live.source["event"]["ts"]);
    assert!(!post["text"].contains("PRIVATE_NATIVE_REQUEST_DO_NOT_EXPORT"));
    let blocks: Value = serde_json::from_str(&post["blocks"]).unwrap();
    let block = blocks
        .as_array()
        .unwrap()
        .iter()
        .find(|block| {
            block["elements"].as_array().is_some_and(|elements| {
                elements
                    .iter()
                    .any(|element| element["action_id"] == "cairn_stop")
            })
        })
        .unwrap();
    let button = block["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|element| element["action_id"] == "cairn_stop")
        .unwrap();
    let ts = live.slack.post_ids.lock().await[text(block, "block_id")].clone();
    common::eventually(
        Duration::from_secs(20),
        Duration::from_millis(20),
        async || {
            live.slack
                .updates
                .lock()
                .await
                .iter()
                .any(|update| {
                    update["ts"] == ts
                        && update["text"].contains("Writing the answer")
                        && update["blocks"].contains("cairn_stop")
                        && !update["blocks"].contains("PRIVATE_PROVIDER_PROGRESS_DO_NOT_EXPORT")
                })
                .then_some(())
        },
    )
    .await;
    if matches!(boundary, ProgressBoundary::InterruptedEdit) {
        // The provider accepted this edit but interrupted its HTTP response.
        // Exercise another native progress poll with no confirmed edit result.
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(live.slack.updates.lock().await.len(), 1);
    }
    let payload = json!({
        "type": "block_actions",
        "api_app_id": "ATEST",
        "team": { "id": "TTEST" },
        "user": { "id": "UALICE" },
        "channel": { "id": "CTEST" },
        "container": { "type": "message", "channel_id": "CTEST", "message_ts": ts },
        "message": {
            "user": "UBOT", "app_id": "ATEST", "ts": ts,
            "thread_ts": live.source["event"]["ts"],
        },
        "actions": [{
            "type": "button",
            "action_id": button["action_id"],
            "block_id": block["block_id"],
            "value": button["value"],
            "action_ts": format!("{}.000100", now() / 1000),
        }],
    });
    let mut bob = payload.clone();
    bob["user"]["id"] = json!("UBOB");
    live.control(&bob, "bob-cannot-stop-ordinary-work", false)
        .await;
    let mut forged = payload.clone();
    forged["message"]["ts"] = json!("1700000002.000001");
    forged["container"]["message_ts"] = forged["message"]["ts"].clone();
    live.control(&forged, "wrong-progress-message", false).await;
    let mut answer = payload.clone();
    answer["actions"][0]["action_id"] = json!("cairn_answer_1_1");
    answer["actions"][0]["block_id"] = json!(format!("{}:1", text(button, "value")));
    live.control(&answer, "progress-is-not-an-issued-question", false)
        .await;
    assert!(!live.controller.stopped().await);
    if matches!(
        boundary,
        ProgressBoundary::PrivateAfterAck | ProgressBoundary::RevokedAfterAck
    ) {
        live.slack.hold_reply_check.store(true, Ordering::SeqCst);
        live.control(&payload, "stop-ack-before-current-metadata", true)
            .await;
        tokio::time::timeout(
            Duration::from_secs(5),
            live.slack.channel_entered.notified(),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(3100)).await;
        assert!(!live.controller.stopped().await);
        if matches!(boundary, ProgressBoundary::PrivateAfterAck) {
            live.slack.private.store(true, Ordering::SeqCst);
        } else {
            let response = common::send(
                &live.app,
                owner(
                    "POST",
                    &format!("{}/people/UALICE", live.route_path),
                    &json!({ "action": "remove", "revision": live.route_revision }),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        live.slack.release_channel.notify_one();
        common::eventually(
            Duration::from_secs(5),
            Duration::from_millis(20),
            async || {
                live.service
                    .store
                    .list(work::EVENTS)
                    .await
                    .ok()?
                    .iter()
                    .any(|event| event["attempts"].as_u64().is_some_and(|n| n >= 1))
                    .then_some(())
            },
        )
        .await;
        assert!(!live.controller.stopped().await);
        let chats = live.service.chat_list().await.unwrap();
        let detail = live
            .service
            .chat_detail(text(&chats[0], "id"))
            .await
            .unwrap();
        assert_eq!(detail["run"]["status"], "running");
        live.finish().await;
        return;
    }
    live.slack.interrupt_update_body.store(
        matches!(boundary, ProgressBoundary::InterruptedCleanup),
        Ordering::SeqCst,
    );
    live.control(&payload, "alice-stops-ordinary-work", true)
        .await;
    common::eventually(
        Duration::from_secs(20),
        Duration::from_millis(20),
        async || live.controller.stopped().await.then_some(()),
    )
    .await;
    common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || {
            live.slack
                .posts
                .lock()
                .await
                .iter()
                .any(|post| {
                    post["channel"] == "CTEST"
                        && post["thread_ts"] == live.source["event"]["ts"]
                        && post["text"].contains("Stopped.")
                })
                .then_some(())
        },
    )
    .await;
    let posted = live.slack.posts.lock().await;
    assert!(
        posted
            .iter()
            .all(|post| !post["text"].contains("PRIVATE_PROVIDER_PROGRESS_DO_NOT_EXPORT"))
    );
    assert!(
        posted
            .iter()
            .all(|post| !post["text"].contains("PRIVATE_NATIVE_REQUEST_DO_NOT_EXPORT"))
    );
    drop(posted);
    common::eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || {
            live.slack
                .updates
                .lock()
                .await
                .iter()
                .any(|update| {
                    update["channel"] == "CTEST"
                        && update["ts"] == ts
                        && update["text"] == "Work stopped."
                        && !update["blocks"].contains("cairn_stop")
                })
                .then_some(())
        },
    )
    .await;
    live.control(&payload, "retired-progress-control", false)
        .await;
    live.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_with_multiple_fields_waits_for_all_canonical_choices() {
    let mut live = Live::start(false).await;
    live.source["event"]["text"] =
        json!("<@UBOT> fixture:question blocking fixture:question-multiple SLACK_QUESTION_FINAL");
    let (delivery, payload) = question(&mut live, false).await;
    live.control(&payload, "first-field-only", true).await;
    common::eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || {
            let rows = live.service.store.list("plugin-storage").await.ok()?;
            rows.iter()
                .any(|row| row["value"]["answers"]["direction"] == "Gradual rollout (Recommended)")
                .then_some(())
        },
    )
    .await;
    let detail = live
        .service
        .chat_detail(text(&delivery, "chatId"))
        .await
        .unwrap();
    assert_eq!(detail["questions"][0]["status"], "pending");
    assert!(!live.controller.stopped().await);
    let mut last = payload.clone();
    last["actions"][0]["action_id"] = json!("cairn_answer_2_1");
    last["actions"][0]["block_id"] = json!(format!("{}:2", text(&payload["actions"][0], "value")));
    last["actions"][0]["action_ts"] = json!(format!("{}.000102", now() / 1000));
    live.control(&last, "last-field-completes-question", true)
        .await;
    common::eventually(
        Duration::from_secs(15),
        Duration::from_millis(20),
        async || {
            let detail = live
                .service
                .chat_detail(text(&delivery, "chatId"))
                .await
                .ok()?;
            (detail["run"]["status"] == "succeeded").then_some(())
        },
    )
    .await;
    let answers = provider_received_answer(&live).await;
    assert_eq!(answers["delivery"], json!({ "answers": ["Patch"] }));
    live.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_definitely_rejected_question_post_retries_before_issuing_its_control() {
    let mut live = Live::start(false).await;
    live.slack.first_post_retry_after.store(2, Ordering::SeqCst);
    live.slack.rate_limit_first.store(true, Ordering::SeqCst);
    let (delivery, payload) = question(&mut live, false).await;
    assert_eq!(live.slack.post_attempts.load(Ordering::SeqCst), 2);
    assert_eq!(live.slack.posts.lock().await.len(), 1);
    let times = live.slack.post_times.lock().await;
    assert!(times[1].duration_since(times[0]) >= Duration::from_secs(2));
    drop(times);
    live.control(&payload, "answer-confirmed-retried-question", true)
        .await;
    common::eventually(
        Duration::from_secs(15),
        Duration::from_millis(20),
        async || {
            let detail = live
                .service
                .chat_detail(text(&delivery, "chatId"))
                .await
                .ok()?;
            (detail["run"]["status"] == "succeeded").then_some(())
        },
    )
    .await;
    provider_received_answer(&live).await;
    live.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uncertain_question_post_is_not_repeated_when_the_manager_reopens_the_provider() {
    let mut live = Live::start(false).await;
    live.slack.uncertain_first.store(true, Ordering::SeqCst);
    live.source().await;
    let state = common::eventually(
        Duration::from_secs(20),
        Duration::from_millis(20),
        async || {
            live.service
                .store
                .list("plugin-storage")
                .await
                .ok()?
                .into_iter()
                .find(|row| row["value"]["status"] == "posting")
                .map(|row| row["value"].clone())
        },
    )
    .await;
    common::eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || (live.slack.posts.lock().await.len() == 1).then_some(()),
    )
    .await;
    let delivery = live
        .service
        .store
        .get(work::DELIVERIES, text(&state, "delivery"))
        .await
        .unwrap()
        .unwrap();
    stop_manager(&mut live.manager).await;
    live.manager =
        start_manager_socket(&live._root.path().join("config.json"), &live.origin, None).await;
    let fresh = issued(&live, &delivery, false, 1).await;
    assert_ne!(fresh["actions"][0]["value"], state["key"]);
    // A reopened provider may issue a new question. It must not repeat the
    // uncertain old POST, whose controls never became confirmed/usable.
    let posts = live.slack.posts.lock().await;
    assert_eq!(
        posts
            .iter()
            .filter(|post| post
                .get("blocks")
                .is_some_and(|blocks| blocks.contains(text(&state, "key"))))
            .count(),
        1
    );
    drop(posts);
    let mut old = fresh.clone();
    old["actions"][0]["value"] = state["key"].clone();
    old["actions"][0]["block_id"] = json!(format!("{}:1", text(&state, "key")));
    live.control(&old, "unconfirmed-question-cannot-be-answered", false)
        .await;
    live.control(&fresh, "answer-new-confirmed-question", true)
        .await;
    common::eventually(
        Duration::from_secs(15),
        Duration::from_millis(20),
        async || {
            let detail = live
                .service
                .chat_detail(text(&delivery, "chatId"))
                .await
                .ok()?;
            (detail["run"]["status"] == "succeeded").then_some(())
        },
    )
    .await;
    provider_received_answer(&live).await;
    live.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secret_native_questions_keep_their_prompts_and_choices_inside_cairn() {
    let mut live = Live::start(false).await;
    live.source["event"]["text"] =
        json!("<@UBOT> fixture:question blocking fixture:question-secret");
    let (delivery, payload) = question(&mut live, true).await;
    let detail = live
        .service
        .chat_detail(text(&delivery, "chatId"))
        .await
        .unwrap();
    assert_eq!(detail["questions"][0]["fields"][0]["secret"], true);
    let exported = serde_json::to_string(&*live.slack.posts.lock().await).unwrap()
        + &serde_json::to_string(&live.service.store.list("plugin-storage").await.unwrap())
            .unwrap();
    assert!(!exported.contains("PRIVATE_NATIVE_QUESTION_DO_NOT_EXPORT"));
    assert!(!exported.contains("Gradual rollout") && !exported.contains("cairn_answer"));
    assert!(exported.contains("Answer this question in Cairn"));
    live.control(&payload, "stop-without-exporting-a-secret-question", true)
        .await;
    common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || live.controller.stopped().await.then_some(()),
    )
    .await;
    live.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_question_controls_ack_before_slow_metadata_but_do_not_bypass_visibility() {
    boundary(false, Boundary::Private).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_question_controls_ack_before_slow_metadata_but_do_not_bypass_visibility() {
    boundary(true, Boundary::Private).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_the_requester_after_control_admission_prevents_the_native_answer() {
    boundary(false, Boundary::Revoked).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reissued_question_after_restart_needs_its_new_author_bound_control() {
    boundary(false, Boundary::Restart).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_accepted_question_answer_survives_a_cooldown_longer_than_the_error_retry_budget() {
    let mut live = Live::start(false).await;
    let (delivery, payload) = question(&mut live, false).await;
    let metadata_before = live.slack.metadata_attempts.load(Ordering::SeqCst);
    // This is an actual 90-second provider wait, beyond the old finite error
    // retry schedule (~50s). No native answer, cooldown or completion is seeded.
    live.slack.metadata_retry_after.store(90, Ordering::SeqCst);
    live.slack
        .metadata_errors
        .lock()
        .await
        .push_back("ratelimited".to_owned());
    live.control(&payload, "accepted-before-long-cooldown", true)
        .await;
    common::eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || {
            (live.slack.metadata_attempts.load(Ordering::SeqCst) == metadata_before + 1)
                .then_some(())
        },
    )
    .await;
    tokio::time::sleep(Duration::from_secs(85)).await;
    assert!(
        !live
            .service
            .store
            .list(work::EVENTS)
            .await
            .unwrap()
            .is_empty(),
        "An ACKed control was lost before Slack's Retry-After elapsed"
    );
    assert_eq!(
        live.slack.metadata_attempts.load(Ordering::SeqCst),
        metadata_before + 1,
        "A provider read bypassed its cooldown"
    );
    common::eventually(
        Duration::from_secs(20),
        Duration::from_millis(20),
        async || {
            let detail = live
                .service
                .chat_detail(text(&delivery, "chatId"))
                .await
                .ok()?;
            (detail["run"]["status"] == "succeeded").then_some(())
        },
    )
    .await;
    provider_received_answer(&live).await;
    live.finish().await;
}

enum Boundary {
    Private,
    Revoked,
    Restart,
}

async fn boundary(socket_mode: bool, boundary: Boundary) {
    let mut live = Live::start(socket_mode).await;
    if matches!(boundary, Boundary::Restart) {
        live.source["event"]["text"] =
            json!("<@UBOT> fixture:question blocking fixture:question-delay SLACK_QUESTION_FINAL");
    }
    let (delivery, payload) = question(&mut live, false).await;
    if matches!(boundary, Boundary::Restart) {
        stop_manager(&mut live.manager).await;
        live.manager = start_manager_socket(
            &live._root.path().join("config.json"),
            &live.origin,
            live.socket.as_ref().map(|socket| socket.origin.as_str()),
        )
        .await;
        common::eventually(
            Duration::from_secs(15),
            Duration::from_millis(10),
            async || (live.controller.plans.lock().await.len() == 2).then_some(()),
        )
        .await;
        let fresh = issued(&live, &delivery, false, 1).await;
        assert_ne!(fresh["actions"][0]["value"], payload["actions"][0]["value"]);
        assert_ne!(fresh["message"]["ts"], payload["message"]["ts"]);
        // The original question was withdrawn when the provider reopened its
        // session. A previously issued answer must not control the reissued RPC.
        live.control(&payload, "old-question-after-restart", true)
            .await;
        common::eventually(
            Duration::from_secs(5),
            Duration::from_millis(20),
            async || {
                live.service
                    .store
                    .list(work::EVENTS)
                    .await
                    .ok()?
                    .iter()
                    .any(|event| event["attempts"].as_u64().is_some_and(|n| n >= 1))
                    .then_some(())
            },
        )
        .await;
        let detail = live
            .service
            .chat_detail(text(&delivery, "chatId"))
            .await
            .unwrap();
        assert!(
            detail["questions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|question| question["status"] == "pending")
        );
        let mut bob = fresh.clone();
        bob["user"]["id"] = json!("UBOB");
        live.control(&bob, "bob-cannot-control-reissued-question", false)
            .await;
        live.control(&fresh, "new-issued-question-after-restart", true)
            .await;
        common::eventually(
            Duration::from_secs(20),
            Duration::from_millis(20),
            async || {
                let detail = live
                    .service
                    .chat_detail(text(&delivery, "chatId"))
                    .await
                    .ok()?;
                (detail["run"]["status"] == "succeeded").then_some(())
            },
        )
        .await;
        assert_eq!(
            live.slack
                .posts
                .lock()
                .await
                .iter()
                .filter(|post| post
                    .get("blocks")
                    .is_some_and(|blocks| blocks.contains("cairn_answer")))
                .count(),
            2,
            "Only the two distinct native questions may be posted"
        );
        provider_received_answer(&live).await;
        let plans = live.controller.plans.lock().await;
        assert_eq!(
            plans.len(),
            2,
            "Recovery must resume the same logical execution in a new runner attempt"
        );
        assert_eq!(plans[0]["chat"]["runId"], plans[1]["chat"]["runId"]);
        assert_ne!(plans[0]["chat"]["attemptId"], plans[1]["chat"]["attemptId"]);
    } else {
        live.slack.hold_reply_check.store(true, Ordering::SeqCst);
        // The acknowledged callback is durably queued before this real provider
        // read. Hold it beyond Slack's entire ACK deadline; no native effect may
        // happen until current channel visibility and authority are checked.
        live.control(&payload, "ack-before-metadata", true).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            live.slack.channel_entered.notified(),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(3100)).await;
        let detail = live
            .service
            .chat_detail(text(&delivery, "chatId"))
            .await
            .unwrap();
        assert_eq!(detail["questions"][0]["status"], "pending");
        if matches!(boundary, Boundary::Private) {
            live.slack.private.store(true, Ordering::SeqCst);
        } else {
            let response = common::send(
                &live.app,
                owner(
                    "POST",
                    &format!("{}/people/UALICE", live.route_path),
                    &json!({ "action": "remove", "revision": live.route_revision }),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        live.slack.release_channel.notify_one();
        common::eventually(
            Duration::from_secs(5),
            Duration::from_millis(20),
            async || {
                live.service
                    .store
                    .list(work::EVENTS)
                    .await
                    .ok()?
                    .iter()
                    .any(|event| event["attempts"].as_u64().is_some_and(|n| n >= 1))
                    .then_some(())
            },
        )
        .await;
        let detail = live
            .service
            .chat_detail(text(&delivery, "chatId"))
            .await
            .unwrap();
        assert_eq!(detail["questions"][0]["status"], "pending");
        assert_eq!(detail["run"]["status"], "running");
        assert!(!live.controller.stopped().await);
        if matches!(boundary, Boundary::Revoked) {
            common::eventually(
                Duration::from_secs(5),
                Duration::from_millis(20),
                async || {
                    live.service
                        .store
                        .list("plugin-storage")
                        .await
                        .ok()?
                        .iter()
                        .all(|row| {
                            row["connectionId"] != live.instance["id"]
                                || !text(row, "key").starts_with("question:")
                        })
                        .then_some(())
                },
            )
            .await;
        }
    }
    live.finish().await;
}

struct Live {
    _root: TempDir,
    service: Arc<Service>,
    slack: Arc<Slack>,
    controller: Arc<controller::Controller>,
    app: Router,
    instance: Value,
    source: Value,
    manager: tokio::process::Child,
    origin: String,
    route_path: String,
    route_revision: String,
    socket: Option<socket::Fixture>,
    servers: [tokio::task::JoinHandle<()>; 2],
}

impl Live {
    async fn start(socket_mode: bool) -> Self {
        let root = TempDir::new().unwrap();
        let mut config = common::config(root.path());
        config.codex_bin = common::fixture("codex.mjs");
        std::fs::create_dir_all(config.home.join(".codex")).unwrap();
        std::fs::write(config.home.join(".codex/auth.json"), r#"{"tokens":{}}"#).unwrap();
        let controller = controller::Controller::new(config.clone());
        let (controller_origin, controller_server) =
            common::serve_locally(controller.router()).await;
        config.runner_url = controller_origin;
        let service = Service::new(config.clone()).await.unwrap();
        common::relay_fixture::claimed(&service).await.unwrap();
        service
            .plugins
            .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let slack = Arc::new(Slack::default());
        let provider = Router::new()
            .route("/api/{method}", post(api).get(api))
            .with_state(slack.clone());
        let (origin, server) = common::serve_locally(provider).await;
        service.plugins.route_http_to(Some(origin.clone()));
        let app = cairn_installation::http::router(service.clone())
            .await
            .unwrap();
        let (instance, resource) = configure_transport(&app, &service, socket_mode).await;
        let agent = service
            .agent_fixture(
                json!({
                    "name": "Slack questions helper",
                    "access": { "projects": [], "skills": [], "mcps": [], "installation": false },
                }),
                None,
            )
            .await
            .unwrap();
        let routes_path = format!(
            "/api/plugins/slack/connections/{}/resources/workspace/{}/routes",
            text(&instance, "id"),
            text(&resource, "id")
        );
        let route = save(
            &app,
            &routes_path,
            json!({
                "name": "Alice and Bob may contribute",
                "enabled": true,
                "match": { "channel": "CTEST" },
                "agentId": agent["id"],
                "accessMode": "allowlist",
                "userIds": ["UALICE", "UBOB"],
                "maxMessagesPerHour": 10,
                "maxPerPersonPerHour": 5,
            }),
        )
        .await;
        let route_path = format!("{routes_path}/{}", text(&route, "id"));
        let route_revision = text(&route, "revision").to_owned();
        std::fs::write(
            config.data_dir.join("storage-s3.json"),
            json!({
                "bucket": "fixture-storage", "endpoint": "https://127.0.0.1:1",
            })
            .to_string(),
        )
        .unwrap();
        config.worker_enabled = true;
        let file = root.path().join("config.json");
        std::fs::write(&file, serde_json::to_vec(&config).unwrap()).unwrap();
        let socket = if socket_mode {
            Some(socket::Fixture::start(true).await)
        } else {
            None
        };
        let manager = start_manager_socket(
            &file,
            &origin,
            socket.as_ref().map(|fixture| fixture.origin.as_str()),
        )
        .await;
        let source = json!({
            "type": "event_callback", "api_app_id": "ATEST", "team_id": "TTEST", "event_id": "EvQUESTION",
            "event": {
                "type": "app_mention", "user": "UALICE", "channel": "CTEST",
                "ts": format!("{}.000001", now() / 1000),
                "text": "<@UBOT> fixture:question blocking SLACK_QUESTION_FINAL",
            },
        });
        Self {
            _root: root,
            service,
            slack,
            controller,
            app,
            instance,
            source,
            manager,
            origin,
            route_path,
            route_revision,
            socket,
            servers: [server, controller_server],
        }
    }

    async fn source(&mut self) {
        if let Some(socket) = self.socket.as_mut() {
            socket.send(self.source.clone(), "question-source").await;
            socket.ack("question-source").await;
        } else {
            assert_eq!(
                ingress(
                    &self.app,
                    text(&self.instance, "id"),
                    self.source.clone(),
                    SIGNING_SECRET
                )
                .await,
                (StatusCode::OK, "accepted".into())
            );
        }
    }

    async fn control(&mut self, payload: &Value, id: &str, allowed: bool) {
        let started = Instant::now();
        if let Some(socket) = self.socket.as_mut() {
            socket.interactive(payload.clone(), id).await;
            socket.ack(id).await;
        } else {
            let (status, body) = reset::control(
                &self.app,
                text(&self.instance, "id"),
                payload,
                SIGNING_SECRET,
            )
            .await;
            assert_eq!(
                status,
                if allowed {
                    StatusCode::OK
                } else {
                    StatusCode::FORBIDDEN
                },
                "{body}"
            );
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "Control ACK exceeded Slack's deadline"
        );
    }

    async fn finish(&mut self) {
        stop_manager(&mut self.manager).await;
        self.controller.shutdown();
        self.service.shutdown.cancel();
        for server in &self.servers {
            server.abort();
        }
        if let Some(socket) = self.socket.take() {
            socket.stop().await;
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.controller.shutdown();
        self.service.shutdown.cancel();
        let _ = self.manager.start_kill();
        for server in &self.servers {
            server.abort();
        }
    }
}

async fn question(live: &mut Live, stop: bool) -> (Value, Value) {
    let first_post = live.slack.posts.lock().await.len();
    live.source().await;
    let delivery = common::eventually(
        Duration::from_secs(20),
        Duration::from_millis(20),
        async || {
            let delivery = live
                .service
                .store
                .list(work::DELIVERIES)
                .await
                .ok()?
                .pop()?;
            let detail = live
                .service
                .chat_detail(text(&delivery, "chatId"))
                .await
                .ok()?;
            let pending = detail["questions"].as_array()?.iter().any(|question| {
                question["status"] == "pending" && question["turnId"] == delivery["id"]
            });
            pending.then_some(delivery)
        },
    )
    .await;
    let payload = issued(live, &delivery, stop, first_post).await;
    (delivery, payload)
}

async fn issued(live: &Live, delivery: &Value, stop: bool, first_post: usize) -> Value {
    let post = common::eventually(
        Duration::from_secs(12),
        Duration::from_millis(20),
        async || {
            live.slack
                .posts
                .lock()
                .await
                .iter()
                .skip(first_post)
                .find(|post| {
                    post.get("blocks").is_some_and(|blocks| {
                        blocks.contains(if stop { "cairn_stop" } else { "cairn_answer" })
                    })
                })
                .cloned()
        },
    )
    .await;
    let blocks: Value = serde_json::from_str(&post["blocks"]).unwrap();
    let (block, button) = blocks
        .as_array()
        .unwrap()
        .iter()
        .find_map(|block| {
            let button = block["elements"].as_array()?.iter().find(|element| {
                if stop {
                    element["action_id"] == "cairn_stop"
                } else {
                    element["text"]["text"] == "Gradual rollout (Recommended)"
                }
            })?;
            Some((block.clone(), button.clone()))
        })
        .expect("The native question did not expose the expected control");
    let message_ts = live.slack.post_ids.lock().await[text(&block, "block_id")].clone();
    // Seeing the provider receive a POST is not a confirmed write. In particular,
    // killing the manager at that point would test an uncertain outcome instead
    // of recovery of a persisted, issued control.
    common::eventually(
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || {
            let current = live
                .service
                .store
                .get(work::DELIVERIES, text(delivery, "id"))
                .await
                .ok()??;
            current["asked"]
                .as_array()
                .is_some_and(|asked| {
                    asked.contains(
                        &button["value"]
                            .as_str()
                            .unwrap()
                            .rsplit(':')
                            .next()
                            .unwrap()
                            .into(),
                    )
                })
                .then_some(())
        },
    )
    .await;
    let payload = json!({
        "type": "block_actions", "api_app_id": "ATEST", "team": { "id": "TTEST" },
        "user": { "id": live.source["event"]["user"] }, "channel": { "id": "CTEST" },
        "container": { "type": "message", "channel_id": "CTEST", "message_ts": message_ts },
        "message": { "user": "UBOT", "app_id": "ATEST", "ts": message_ts, "thread_ts": live.source["event"]["ts"] },
        "actions": [{ "type": "button", "action_id": button["action_id"], "block_id": block["block_id"], "value": button["value"], "action_ts": format!("{}.000100", now() / 1000), "text": { "type": "plain_text", "text": "CLIENT_MUST_NOT_CHOOSE_THE_ANSWER" } }],
        "response_url": "https://hooks.slack.com/PRIVATE_SYNTHETIC_DO_NOT_STORE",
        "state": { "values": { "unrelated": "PRIVATE_SYNTHETIC_DO_NOT_STORE" } },
    });
    payload
}

async fn exchange(socket_mode: bool, stop: bool) {
    let mut live = Live::start(socket_mode).await;
    let (delivery, payload) = question(&mut live, stop).await;
    for (field, value) in [
        ("/api_app_id", json!("AOTHER")),
        ("/team/id", json!("TOTHER")),
        ("/message/user", json!("UOTHER")),
        ("/message/thread_ts", json!("1700000001.000009")),
        ("/actions/0/block_id", json!("unknown-issued-block")),
        ("/actions/0/value", json!("question:00000000:00000000")),
    ] {
        let mut invalid = payload.clone();
        *invalid.pointer_mut(field).unwrap() = value;
        live.control(&invalid, &format!("wrong-{field}"), false)
            .await;
    }
    let mut another_message = payload.clone();
    another_message["container"]["message_ts"] = json!("1700000002.000001");
    another_message["message"]["ts"] = json!("1700000002.000001");
    live.control(&another_message, "wrong-source-message", false)
        .await;
    let mut bob = payload.clone();
    bob["user"]["id"] = json!("UBOB");
    live.control(&bob, "bob-cannot-control-alice", false).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let detail = live
        .service
        .chat_detail(text(&delivery, "chatId"))
        .await
        .unwrap();
    assert_eq!(detail["questions"][0]["status"], "pending");
    assert_eq!(detail["run"]["status"], "running");
    assert!(!live.controller.stopped().await);
    live.control(&payload, "alice-controls-her-own-run", true)
        .await;
    let expected_status = if stop { "cancelled" } else { "succeeded" };
    common::eventually(
        Duration::from_secs(20),
        Duration::from_millis(30),
        async || {
            let detail = live
                .service
                .chat_detail(text(&delivery, "chatId"))
                .await
                .ok()?;
            (detail["run"]["status"] == expected_status).then_some(())
        },
    )
    .await;
    if stop {
        common::eventually(
            Duration::from_secs(5),
            Duration::from_millis(20),
            async || live.controller.stopped().await.then_some(()),
        )
        .await;
    } else {
        provider_received_answer(&live).await;
    }
    common::eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || {
            let posts = live.slack.posts.lock().await;
            posts
                .iter()
                .any(|post| {
                    post["channel"] == "CTEST"
                        && post["thread_ts"] == text(&live.source["event"], "ts")
                        && post["text"].contains(if stop {
                            "Stopped."
                        } else {
                            "SLACK_QUESTION_FINAL"
                        })
                })
                .then_some(())
        },
    )
    .await;
    let times = live.slack.post_times.lock().await;
    assert!(
        times
            .windows(2)
            .all(|pair| pair[1].duration_since(pair[0]) >= Duration::from_secs(1)),
        "Question and reply posts did not share channel pacing"
    );
    drop(times);
    let retained = serde_json::to_string(&live.service.store.list("plugin-events").await.unwrap())
        .unwrap()
        + &serde_json::to_string(&live.service.store.list("plugin-storage").await.unwrap())
            .unwrap();
    assert!(
        !retained.contains("PRIVATE_SYNTHETIC_DO_NOT_STORE")
            && !retained.contains("CLIENT_MUST_NOT_CHOOSE_THE_ANSWER")
    );
    assert_eq!(live.controller.plans.lock().await.len(), 1);
    live.finish().await;
}

async fn provider_received_answer(live: &Live) -> Value {
    let plans = live.controller.plans.lock().await;
    let home = plans.last().unwrap()["imports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mount| mount["target"] == "/home/node")
        .unwrap()["source"]
        .as_str()
        .unwrap();
    let state: Value = serde_json::from_slice(
        &std::fs::read(PathBuf::from(home).join(".codex/fixture-conversation.json")).unwrap(),
    )
    .unwrap();
    let answer = state["turns"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|turn| turn["items"].as_array().unwrap())
        .find(|item| item["type"] == "fixtureAnswer")
        .expect("The real provider did not receive the answer");
    assert_eq!(
        answer["answers"]["direction"],
        json!({ "answers": ["Gradual rollout (Recommended)"] })
    );
    answer["answers"].clone()
}
