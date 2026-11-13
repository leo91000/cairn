//! A change of node expires the working environment (ADR-0083, local conversation
//! disks). Backend migration is the same node change; see native_nodes.rs.
use super::*;
use cairn_installation::environment_expiration::{self, WARNING};
use std::collections::HashSet;

/// Two schedulable nodes that `agent` may use, with an idle chat on `source`.
struct Change {
    owner: Owner,
    agent: String,
    chat: String,
    run: String,
    source: String,
    destination: String,
}

impl Change {
    async fn new(status: RunStatus) -> Self {
        let owner = Owner::new().await;
        let (agent, chat, run, source, destination) = (id(), id(), id(), id(), id());
        for node in [&source, &destination] {
            owner
                .put_node_fixture(schedulable_node(node, &limits(4, 8192, 65536)))
                .await;
        }
        owner
            .grant_nodes(&agent, json!([source, destination]))
            .await;
        let message = id();
        let mut record = run_record(&run, status);
        record["taskId"] = chat.clone().into();
        record["trigger"] = "chat".into();
        record["snapshot"] = agent_run(&run, &agent)["snapshot"].clone();
        record["nodeId"] = source.clone().into();
        record["pinnedNodeId"] = source.clone().into();
        record["isolated"] = true.into();
        record["sessionId"] = "previous-engine-session".into();
        record["chatExecution"] = json!({
            "messageId": message, "text": "Keep working", "attachments": [],
        });
        owner.add_run_fixture(&record).await;
        owner
            .save_conversation_boundary(json!({
                "id": chat, "runId": run, "agentId": agent, "title": "kept",
                "nativeProvenanceUnknown": true,
            }))
            .await;
        owner
            .set_checkpoint(
                &run,
                json!({
                    "nodeId": source, "runnerId": run, "completed": true, "remainingMs": 123000,
                    "prepared": { "backend": "firecracker", "isolated": true },
                }),
            )
            .await;
        owner
            .put(
                "node-volumes",
                json!({
                    "id": format!("{run}:{source}"), "nodeId": source, "runId": run,
                    "materialized": true, "diskMiB": 128,
                }),
            )
            .await;
        Self {
            owner,
            agent,
            chat,
            run,
            source,
            destination,
        }
    }

    async fn move_to(&self, body: Value) -> (StatusCode, Value) {
        self.owner
            .send(
                "POST",
                &format!("/api/nodes/placement/{}/move", self.run),
                body,
            )
            .await
    }

    async fn retirements(&self) -> Vec<Value> {
        self.owner
            .service
            .store
            .list("environment-retirements")
            .await
            .unwrap()
            .into_iter()
            .filter(|retirement| retirement["runId"] == self.run)
            .collect()
    }

    async fn admission_blocked(&self) -> bool {
        let run = self.run.clone();
        self.owner
            .service
            .store
            .read(move |db| environment_expiration::admission_blocked(db, &run))
            .await
            .unwrap()
    }

    async fn events(&self) -> Vec<String> {
        let run = self.run.clone();
        self.owner
            .service
            .store
            .read(move |db| {
                Ok(db
                    .0
                    .prepare("SELECT text FROM events WHERE run_id=? ORDER BY id")?
                    .query_map([run], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap()
    }
}

async fn agent_move(owner: &Owner, run: &str, args: Value) -> Value {
    let bearer = id();
    let current = owner.run(run).await;
    let message = current["chatExecution"]["messageId"]
        .as_str()
        .or(current["missionTurnId"].as_str())
        .map(str::to_owned);
    let grant = cairn_installation::mcps::record::RunGrant {
        run_id: run.to_owned(),
        message_id: message,
        servers: std::collections::BTreeMap::default(),
        workspace: true,
        installation: false,
    };
    let key = auth::hex_digest(&bearer);
    owner
        .service
        .store
        .transaction(move |db| db.save_mcp_run_grant(&key, &grant, None))
        .await
        .unwrap();
    cairn_installation::project_workspaces::rpc(
        &owner.service,
        &bearer,
        "tools/call",
        &json!({"name": "move_to_node", "arguments": args}),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn confirmed_node_change_expires_the_environment_and_imposes_its_destination_once() {
    let change = Change::new(RunStatus::Succeeded).await;
    let question = change
        .owner
        .service
        .store
        .transaction({
            let (chat, run) = (change.chat.clone(), change.run.clone());
            move |db| {
                let question = json!({
                    "id": id(), "chatId": chat, "runId": run, "source": "cairn",
                    "status": "pending", "blocking": true, "createdAt": now(),
                    "fields": [{ "id": "choice", "title": "Pick one" }],
                });
                db.save_question(&serde_json::from_value(question.clone())?)?;
                Ok(question)
            }
        })
        .await
        .unwrap();

    // The existing confirmation is not a confirmation that files are lost.
    let (status, refused) = change
        .move_to(json!({ "nodeId": change.destination }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains(WARNING),
        "{refused}"
    );
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("discardWorkingFiles: true")
    );
    assert!(change.owner.run(&change.run).await["environmentState"].is_null());
    assert!(change.retirements().await.is_empty());

    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["status"], "moved");
    assert_eq!(moved["nodeId"], change.destination);

    // One transaction detached the environment: no blocking cleanup state.
    let expired = change.owner.run(&change.run).await;
    assert_eq!(expired["environmentState"], "expired");
    assert_eq!(expired["environmentExpirationCause"], "node-change");
    assert!(expired["environmentExpiredAt"].is_number());
    assert!(expired["nodeId"].is_null());
    assert!(expired["sessionId"].is_null());
    assert!(
        expired["pinnedNodeId"].is_null(),
        "the old pin cannot override the change"
    );
    assert_eq!(expired["targetNodeId"], change.destination);
    assert_eq!(expired["status"], RunStatus::Succeeded);
    let checkpoint = change
        .owner
        .service
        .store
        .checkpoint_boundary(&change.run)
        .await
        .unwrap()
        .unwrap();
    assert!(checkpoint["nodeId"].is_null(), "{checkpoint}");
    assert!(checkpoint["prepared"].is_null(), "{checkpoint}");
    let chat = change
        .owner
        .service
        .chat_detail(&change.chat)
        .await
        .unwrap();
    assert_eq!(chat["environmentState"], "expired");
    assert_eq!(chat["environmentExpirationCause"], "node-change");
    let questions = change
        .owner
        .service
        .store
        .read({
            let chat = change.chat.clone();
            move |db| Ok(serde_json::to_value(db.questions(&chat)?)?)
        })
        .await
        .unwrap();
    let closed = questions
        .as_array()
        .unwrap()
        .iter()
        .find(|current| current["id"] == question["id"])
        .unwrap();
    assert_eq!(closed["status"], "cancelled");
    assert!(
        change
            .events()
            .await
            .iter()
            .any(|event| event
                .starts_with("Environment removed when the conversation changed node."))
    );

    // The old disk is retired in the background, never on the next admission's node.
    let retirements = change.retirements().await;
    assert_eq!(retirements.len(), 1);
    assert_eq!(retirements[0]["cause"], "node-change");
    assert_eq!(retirements[0]["nodes"], json!([change.source]));
    let run = change.run.clone();
    let retiring = change
        .owner
        .service
        .store
        .read(move |db| environment_expiration::retiring_nodes(db, &run))
        .await
        .unwrap();
    assert_eq!(retiring, vec![change.source.clone()]);

    assert!(
        placement::reserve(&change.owner.service, &expired, &id())
            .await
            .is_err(),
        "fencing precedes every admission"
    );
    change.owner.service.retire_environments().await.unwrap();

    // The first admission happens on the destination, then placement is automatic.
    let attempt = id();
    let admitted = placement::reserve(&change.owner.service, &expired, &attempt)
        .await
        .unwrap();
    assert_eq!(admitted["nodeId"], change.destination);
    assert!(change.owner.run(&change.run).await["targetNodeId"].is_null());
    placement::release(&change.owner.service, &attempt)
        .await
        .unwrap();
}

#[tokio::test]
async fn imposed_destination_waits_while_authorized_then_falls_back_to_automatic_placement() {
    let change = Change::new(RunStatus::Succeeded).await;
    let other = id();
    change
        .owner
        .put_node_fixture(schedulable_node(&other, &limits(4, 8192, 65536)))
        .await;
    change
        .owner
        .grant_nodes(
            &change.agent,
            json!([change.source, change.destination, other]),
        )
        .await;
    let mut full = schedulable_node(&change.destination, &limits(4, 8192, 65536));
    full["pressure"] = "memory".into();
    change.owner.put_node_fixture(full).await;
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");

    change.owner.service.retire_environments().await.unwrap();
    // A full destination keeps the new environment waiting for it.
    let expired = change.owner.run(&change.run).await;
    let error = placement::reserve(&change.owner.service, &expired, &id())
        .await
        .unwrap_err();
    assert!(placement::is_no_capacity(&error), "{error:?}");
    assert_eq!(
        change.owner.run(&change.run).await["targetNodeId"],
        change.destination
    );

    // Once the destination is no longer authorized, placement is automatic again,
    // still excluding the node that has not deleted the old disk.
    change
        .owner
        .grant_nodes(&change.agent, json!([change.source, other]))
        .await;
    let attempt = id();
    let admitted = placement::reserve(&change.owner.service, &expired, &attempt)
        .await
        .unwrap();
    assert_eq!(admitted["nodeId"], other);
    assert!(change.owner.run(&change.run).await["targetNodeId"].is_null());
    placement::release(&change.owner.service, &attempt)
        .await
        .unwrap();
}

#[tokio::test]
async fn retirement_fences_before_readmission_and_forgets_a_revoked_nodes_disk() {
    let change = Change::new(RunStatus::Succeeded).await;
    // The last execution still holds its attempt on an unreachable node.
    change
        .owner
        .put_attempt_fixture(json!({
            "id": change.run, "runId": change.run, "nodeId": change.source,
            "released": false, "leaseRequired": true,
        }))
        .await;
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    // Messages are accepted immediately: expiration has no blocking cleanup state.
    let (status, sent) = change
        .owner
        .send(
            "POST",
            &format!("/api/chats/{}/messages", change.chat),
            json!({ "id": id(), "text": "Continue on the destination" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{sent}");

    // The unreachable execution is not confirmed stopped: no new environment yet.
    change.owner.service.retire_environments().await.unwrap();
    assert!(change.admission_blocked().await);
    let retirements = change.retirements().await;
    assert_eq!(retirements[0]["fenced"], false);
    assert!(retirements[0]["error"].is_string(), "{}", retirements[0]);
    assert!(retirements[0]["retryAfter"].is_number());
    assert_eq!(
        change
            .owner
            .service
            .chat_detail(&change.chat)
            .await
            .unwrap()["environmentError"],
        retirements[0]["error"]
    );

    // Revoking the node releases what it can never acknowledge, including the disk.
    let (status, revoked) = change
        .owner
        .send(
            "POST",
            &format!("/api/nodes/{}/revoke", change.source),
            json!({ "discardWorkingFiles": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    change
        .owner
        .service
        .store
        .transaction(|db| {
            for mut retirement in db.list("environment-retirements")? {
                retirement["retryAfter"] = Value::Null;
                db.put("environment-retirements", &retirement)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    change
        .owner
        .put_attempt_fixture(json!({
            "id": change.run, "runId": change.run, "nodeId": change.source,
            "released": true,
        }))
        .await;
    change.owner.service.retire_environments().await.unwrap();
    assert!(change.retirements().await.is_empty());
    assert!(!change.admission_blocked().await);
    assert!(
        change
            .owner
            .stored("node-volumes", &format!("{}:{}", change.run, change.source))
            .await
            .is_none()
    );
    assert!(
        change
            .owner
            .service
            .chat_detail(&change.chat)
            .await
            .unwrap()["environmentError"]
            .is_null()
    );
}

#[tokio::test]
async fn live_environment_detaches_immediately_and_admission_waits_until_it_stops() {
    let change = Change::new(RunStatus::Running).await;
    let token = CancellationToken::new();
    change
        .owner
        .service
        .worker
        .active
        .lock()
        .await
        .insert(change.run.clone(), token.clone());
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert!(token.is_cancelled(), "the current turn is interrupted");
    let pending = change.owner.run(&change.run).await;
    assert!(pending["cancelRequestedAt"].is_number());
    assert_eq!(pending["environmentState"], "expired");
    assert_eq!(pending["environmentExpirationCause"], "node-change");
    assert!(pending["nodeId"].is_null());
    assert!(pending["sessionId"].is_null());
    assert_eq!(pending["targetNodeId"], change.destination);
    assert!(change.admission_blocked().await);
    change
        .owner
        .service
        .detach_stopped_environments()
        .await
        .unwrap();
    assert!(change.retirements().await.is_empty());

    // The worker records the stopped turn and releases its slot.
    change
        .owner
        .service
        .store
        .change_run(
            &change.run,
            cairn_installation::runs::RunChanges {
                status: Some(RunStatus::Cancelled),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    change
        .owner
        .service
        .worker
        .active
        .lock()
        .await
        .remove(&change.run);
    change
        .owner
        .service
        .detach_stopped_environments()
        .await
        .unwrap();
    let detached = change.owner.run(&change.run).await;
    assert!(detached["nodeId"].is_null(), "{detached}");
    assert_eq!(detached["targetNodeId"], change.destination);
    assert_eq!(detached["status"], RunStatus::Cancelled);
    assert_eq!(change.retirements().await.len(), 1);
}

#[tokio::test]
async fn an_agent_changing_its_own_node_continues_there_in_a_fresh_environment() {
    let change = Change::new(RunStatus::Running).await;
    let refused = agent_move(
        &change.owner,
        &change.run,
        json!({ "nodeId": change.destination }),
    )
    .await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(
        refused.to_string().contains("discardWorkingFiles"),
        "{refused}"
    );
    assert!(change.owner.run(&change.run).await["environmentState"].is_null());

    let moved = agent_move(
        &change.owner,
        &change.run,
        json!({ "nodeId": change.destination, "discardWorkingFiles": true }),
    )
    .await;
    assert_ne!(moved["isError"], true, "{moved}");
    let continued = change.owner.run(&change.run).await;
    assert_eq!(continued["status"], RunStatus::Queued);
    assert_eq!(continued["environmentState"], "expired", "{continued}");
    assert!(continued["environmentExpiredAt"].is_number());
    assert_eq!(continued["environmentExpirationCause"], "node-change");
    assert_eq!(continued["targetNodeId"], change.destination);
    assert!(continued["cancelRequestedAt"].is_null());
    assert!(continued["chatExecution"]["context"].is_string());
    assert_eq!(continued["chatExecution"]["text"], "Keep working");
    let checkpoint = change
        .owner
        .service
        .store
        .checkpoint_boundary(&change.run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint["turnPending"], true);
    assert_eq!(checkpoint["launched"], false);
    assert!(checkpoint["prepared"].is_null());
    let events = change.events().await;
    assert!(
        events
            .iter()
            .any(|event| event.starts_with("The agent changed this conversation's node")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.contains("The conversation continues in a fresh environment")),
        "{events:?}"
    );
    assert!(
        change
            .owner
            .service
            .chat_detail(&change.chat)
            .await
            .unwrap()["environmentState"]
            .is_null()
    );
    let (status, selected) = change.move_to(json!({ "nodeId": change.source })).await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert_eq!(change.retirements().await.len(), 1);
    assert_eq!(
        change.owner.run(&change.run).await["targetNodeId"],
        change.source
    );
}

#[tokio::test]
async fn an_active_mission_resumes_on_the_destination() {
    let change = Change::new(RunStatus::Running).await;
    change
        .owner
        .service
        .store
        .transaction({
            let (run, chat) = (change.run.clone(), change.chat.clone());
            move |db| {
                db.0.execute("DELETE FROM conversations WHERE id=?", [chat])?;
                db.change_run(
                    &run,
                    cairn_installation::runs::RunChanges {
                        trigger: Some("manual".into()),
                        ..Default::default()
                    },
                )?;
                db.update_run_boundary(&run, &json!({ "chatExecution": null }))
            }
        })
        .await
        .unwrap();
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    let resumed = change.owner.run(&change.run).await;
    assert_eq!(
        resumed["status"],
        RunStatus::Queued,
        "error={}, summary={}",
        resumed["error"],
        resumed["summary"]
    );
    assert_eq!(resumed["targetNodeId"], change.destination);
    assert!(resumed["sessionHandoff"].is_string());
    assert_eq!(resumed["environmentExpirationCause"], "node-change");
    assert!(resumed["nodeId"].is_null());
}

#[tokio::test]
async fn revocation_requires_its_own_confirmation_and_expires_the_nodes_environments() {
    let change = Change::new(RunStatus::Succeeded).await;
    let path = format!("/api/nodes/{}/revoke", change.source);
    let (status, refused) = change
        .owner
        .send("POST", &path, json!({ "releasePlacements": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains(environment_expiration::NODE_WARNING)
    );
    assert_ne!(
        change.owner.stored("nodes", &change.source).await.unwrap()["revoked"],
        true
    );

    let (status, revoked) = change
        .owner
        .send("POST", &path, json!({ "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    let expired = change.owner.run(&change.run).await;
    assert_eq!(expired["environmentState"], "expired");
    assert_eq!(expired["environmentExpirationCause"], "node-revocation");
    assert!(expired["nodeId"].is_null());
    // Without releasePlacements the pin keeps naming the revoked node: it waits.
    assert_eq!(expired["pinnedNodeId"], change.source);
    assert!(expired["targetNodeId"].is_null());
    assert!(
        change
            .events()
            .await
            .iter()
            .any(|event| event.starts_with("Environment removed when its node was revoked."))
    );
    change.owner.service.retire_environments().await.unwrap();
    assert!(
        change.retirements().await.is_empty(),
        "a revoked node's disk is forgotten"
    );
}

#[tokio::test]
async fn unavailable_or_unauthorized_nodes_keep_their_environment_waiting() {
    let change = Change::new(RunStatus::Queued).await;
    let run = change.owner.run(&change.run).await;
    assert!(
        moves::node_ready(&change.owner.service, &run)
            .await
            .unwrap()
    );

    let mut offline = schedulable_node(&change.source, &limits(4, 8192, 65536));
    offline["lastSeen"] = 0.into();
    change.owner.put_node_fixture(offline).await;
    assert!(
        !moves::node_ready(&change.owner.service, &run)
            .await
            .unwrap()
    );
    let waiting = change.owner.run(&change.run).await;
    assert_eq!(waiting["nodeState"], "waiting-for-node");
    assert!(waiting["moveRequest"].is_null());

    change
        .owner
        .put_node_fixture(schedulable_node(&change.source, &limits(4, 8192, 65536)))
        .await;
    change
        .owner
        .grant_nodes(&change.agent, json!([change.destination]))
        .await;
    assert!(
        !moves::node_ready(&change.owner.service, &run)
            .await
            .unwrap()
    );

    // A full node keeps the existing environment: placement never picks another node.
    change
        .owner
        .grant_nodes(&change.agent, json!([change.source, change.destination]))
        .await;
    let mut full = schedulable_node(&change.source, &limits(4, 8192, 65536));
    full["pressure"] = "memory".into();
    change.owner.put_node_fixture(full).await;
    let error = placement::check(&change.owner.service, &run)
        .await
        .unwrap_err();
    assert!(placement::is_no_capacity(&error), "{error:?}");
    let current = change.owner.run(&change.run).await;
    assert!(current["moveRequest"].is_null());
    assert!(current["environmentState"].is_null());
    assert!(
        change
            .owner
            .service
            .store
            .attempts_fixture()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn choosing_again_before_admission_replaces_the_destination_without_discard_confirmation() {
    let change = Change::new(RunStatus::Succeeded).await;
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    let expired_at = change.owner.run(&change.run).await["environmentExpiredAt"].clone();
    let (status, chosen) = change.move_to(json!({ "nodeId": change.source })).await;
    assert_eq!(status, StatusCode::OK, "{chosen}");
    let waiting = change.owner.run(&change.run).await;
    assert_eq!(waiting["targetNodeId"], change.source);
    assert_eq!(waiting["environmentExpiredAt"], expired_at);
    assert_eq!(change.retirements().await.len(), 1);
    // A saved placement replaces the one-use destination too, including automatic.
    let (status, saved) = change
        .owner
        .send(
            "PUT",
            &format!("/api/nodes/placement/{}", change.run),
            json!({ "selection": { "pinnedNodeId": null, "preferredNodeId": null } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(change.owner.run(&change.run).await["targetNodeId"].is_null());
}

#[tokio::test]
async fn a_retired_disk_blocks_only_readmission_to_its_own_node_until_deleted() {
    let change = Change::new(RunStatus::Succeeded).await;
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    // The attempt is stopped; the source disk is on an unreachable node.
    change.owner.service.retire_environments().await.unwrap();
    assert!(!change.admission_blocked().await);
    assert_eq!(
        change.retirements().await[0]["nodes"],
        json!([change.source])
    );
    assert!(change.retirements().await[0]["error"].is_string());
    let (status, chosen) = change.move_to(json!({ "nodeId": change.source })).await;
    assert_eq!(status, StatusCode::OK, "{chosen}");
    let run = change.owner.run(&change.run).await;
    assert!(
        placement::reserve(&change.owner.service, &run, &id())
            .await
            .is_err(),
        "old disk must be deleted before reuse on the same node"
    );
    let (status, chosen) = change
        .move_to(json!({ "nodeId": change.destination }))
        .await;
    assert_eq!(status, StatusCode::OK, "{chosen}");
    let run = change.owner.run(&change.run).await;
    let attempt = id();
    let admitted = placement::reserve(&change.owner.service, &run, &attempt)
        .await
        .unwrap();
    assert_eq!(admitted["nodeId"], change.destination);
    assert!(change.owner.run(&change.run).await["environmentState"].is_null());
    placement::release(&change.owner.service, &attempt)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_waiting_mission_keeps_its_wakeup_when_changing_node() {
    let change = Change::new(RunStatus::Waiting).await;
    let wakeup = json!({ "id": id(), "messageId": "mission-turn", "seconds": 1, "createdAt": now() - 2000, "wakeAt": now() - 1000, "reason": "Wait for the scheduled continuation" });
    let (run, chat, retained) = (change.run.clone(), change.chat.clone(), wakeup.clone());
    change
        .owner
        .service
        .store
        .transaction(move |db| {
            db.0.execute("DELETE FROM conversations WHERE id=?", [chat])?;
            db.change_run(
                &run,
                cairn_installation::runs::RunChanges {
                    trigger: Some("manual".into()),
                    wakeup: Some(Some(serde_json::from_value(retained)?)),
                    ..Default::default()
                },
            )?;
            db.update_run_boundary(
                &run,
                &json!({ "chatExecution": null, "missionTurnId": "mission-turn" }),
            )?;
            db.save_checkpoint_boundary(&run, &json!({ "completed": true, "remainingMs": 123000 }))
        })
        .await
        .unwrap();
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    let waiting = change.owner.run(&change.run).await;
    assert_eq!(waiting["status"], RunStatus::Waiting);
    assert_eq!(waiting["wakeup"], wakeup);
    assert_eq!(waiting["targetNodeId"], change.destination);
    assert_eq!(waiting["environmentExpirationCause"], "node-change");
    let checkpoint = change
        .owner
        .service
        .store
        .checkpoint_boundary(&change.run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint["remainingMs"], 123000);
    let prompt = cairn_installation::run_output::prompt(&waiting, false);
    assert!(prompt.contains("when the conversation changed node"));
    assert!(prompt.contains("<previous_conversation>"));
    change
        .owner
        .service
        .worker
        .tick(&change.owner.service)
        .await
        .unwrap();
    let resumed = change.owner.run(&change.run).await;
    assert_eq!(
        resumed["status"],
        RunStatus::Queued,
        "error={}, summary={}",
        resumed["error"],
        resumed["summary"]
    );
    assert!(resumed["wakeup"].is_null());
    let checkpoint = change
        .owner
        .service
        .store
        .checkpoint_boundary(&change.run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint["remainingMs"], 123000);
}

#[tokio::test]
async fn explicit_stop_wins_over_a_pending_self_move_or_mission_continuation() {
    for mission in [false, true] {
        let change = Change::new(RunStatus::Running).await;
        if mission {
            let (run, chat) = (change.run.clone(), change.chat.clone());
            change
                .owner
                .service
                .store
                .transaction(move |db| {
                    db.0.execute("DELETE FROM conversations WHERE id=?", [chat])?;
                    db.change_run(
                        &run,
                        cairn_installation::runs::RunChanges {
                            trigger: Some("manual".into()),
                            ..Default::default()
                        },
                    )?;
                    db.update_run_boundary(&run, &json!({ "chatExecution": null }))
                })
                .await
                .unwrap();
        }
        let token = CancellationToken::new();
        change
            .owner
            .service
            .worker
            .active
            .lock()
            .await
            .insert(change.run.clone(), token);
        if mission {
            let (status, moved) = change
                .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
                .await;
            assert_eq!(status, StatusCode::OK, "{moved}");
            change
                .owner
                .service
                .worker
                .cancel(&change.owner.service, &change.run)
                .await
                .unwrap();
        } else {
            let moved = agent_move(
                &change.owner,
                &change.run,
                json!({ "nodeId": change.destination, "discardWorkingFiles": true }),
            )
            .await;
            assert_ne!(moved["isError"], true, "{moved}");
            let (status, stopped) = change
                .owner
                .send(
                    "POST",
                    &format!("/api/chats/{}/stop", change.chat),
                    json!({}),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{stopped}");
        }
        change
            .owner
            .service
            .worker
            .active
            .lock()
            .await
            .remove(&change.run);
        change
            .owner
            .service
            .detach_stopped_environments()
            .await
            .unwrap();
        let stopped = change.owner.run(&change.run).await;
        assert_eq!(stopped["status"], RunStatus::Cancelled);
        assert_eq!(stopped["environmentState"], "expired");
        assert_eq!(stopped["targetNodeId"], change.destination);
        assert!(stopped["cancelRequestedAt"].is_number());
    }
}

#[tokio::test]
async fn expiration_restores_a_live_missions_settled_wait_but_explicit_stop_cancels_it() {
    for stopped in [false, true] {
        let change = Change::new(RunStatus::Waiting).await;
        let wakeup = json!({ "id": id(), "messageId": "mission-turn", "seconds": 1, "createdAt": now() - 2000, "wakeAt": now() - 1000, "reason": "Continue the original mission" });
        let (run, chat, wake) = (change.run.clone(), change.chat.clone(), wakeup.clone());
        change
            .owner
            .service
            .store
            .transaction(move |db| {
                db.0.execute("DELETE FROM conversations WHERE id=?", [chat])?;
                db.change_run(
                    &run,
                    cairn_installation::runs::RunChanges {
                        trigger: Some("manual".into()),
                        wakeup: Some(Some(serde_json::from_value(wake)?)),
                        ..Default::default()
                    },
                )?;
                db.update_run_boundary(
                    &run,
                    &json!({ "chatExecution": null, "missionTurnId": "mission-turn" }),
                )
            })
            .await
            .unwrap();
        change
            .owner
            .service
            .worker
            .active
            .lock()
            .await
            .insert(change.run.clone(), CancellationToken::new());
        let (status, moved) = change
            .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
            .await;
        assert_eq!(status, StatusCode::OK, "{moved}");
        if stopped {
            change
                .owner
                .service
                .worker
                .cancel(&change.owner.service, &change.run)
                .await
                .unwrap();
        }
        // The interrupted worker's ordinary cancellation settlement clears its wait.
        change
            .owner
            .service
            .store
            .change_run(
                &change.run,
                cairn_installation::runs::RunChanges {
                    status: Some(RunStatus::Cancelled),
                    wakeup: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        change
            .owner
            .service
            .worker
            .active
            .lock()
            .await
            .remove(&change.run);
        change
            .owner
            .service
            .detach_stopped_environments()
            .await
            .unwrap();
        let detached = change.owner.run(&change.run).await;
        assert_eq!(detached["environmentState"], "expired");
        if stopped {
            assert_eq!(detached["status"], RunStatus::Cancelled);
            assert!(detached["wakeup"].is_null());
            continue;
        }
        assert_eq!(detached["status"], RunStatus::Waiting);
        assert_eq!(detached["wakeup"], wakeup);
        let checkpoint = change
            .owner
            .service
            .store
            .checkpoint_boundary(&change.run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint["remainingMs"], 123000);
        assert!(
            cairn_installation::run_output::prompt(&detached, false)
                .contains("when the conversation changed node")
        );
        change
            .owner
            .service
            .worker
            .tick(&change.owner.service)
            .await
            .unwrap();
        let queued = change.owner.run(&change.run).await;
        assert_eq!(
            queued["status"],
            RunStatus::Queued,
            "error={}, summary={}",
            queued["error"],
            queued["summary"]
        );
        assert_eq!(queued["environmentState"], "expired");
        let (status, selected) = change
            .move_to(json!({ "nodeId": change.destination }))
            .await;
        assert_eq!(status, StatusCode::OK, "{selected}");
        assert_eq!(change.retirements().await.len(), 1);
        change.owner.service.retire_environments().await.unwrap();
        let attempt = id();
        assert!(
            placement::reserve(&change.owner.service, &queued, &id())
                .await
                .is_err()
        );
        let current = change.owner.run(&change.run).await;
        placement::reserve(&change.owner.service, &current, &attempt)
            .await
            .unwrap();
        assert!(change.owner.run(&change.run).await["environmentState"].is_null());
        placement::release(&change.owner.service, &attempt)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn queued_messages_and_active_missions_can_reselect_before_first_admission_without_losing_files_again()
 {
    for mission in [false, true] {
        let change = Change::new(if mission {
            RunStatus::Running
        } else {
            RunStatus::Succeeded
        })
        .await;
        if mission {
            let (run, chat) = (change.run.clone(), change.chat.clone());
            change
                .owner
                .service
                .store
                .transaction(move |db| {
                    db.0.execute("DELETE FROM conversations WHERE id=?", [chat])?;
                    db.change_run(
                        &run,
                        cairn_installation::runs::RunChanges {
                            trigger: Some("manual".into()),
                            ..Default::default()
                        },
                    )?;
                    db.update_run_boundary(&run, &json!({ "chatExecution": null }))
                })
                .await
                .unwrap();
        }
        let (status, moved) = change
            .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
            .await;
        assert_eq!(status, StatusCode::OK, "{moved}");
        if !mission {
            let (status, sent) = change
                .owner
                .send(
                    "POST",
                    &format!("/api/chats/{}/messages", change.chat),
                    json!({ "id": id(), "text": "Continue when the destination has room" }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{sent}");
            change
                .owner
                .service
                .chat_tick(&HashSet::new())
                .await
                .unwrap();
        }
        let queued = change.owner.run(&change.run).await;
        assert_eq!(
            queued["status"],
            RunStatus::Queued,
            "error={}, summary={}",
            queued["error"],
            queued["summary"]
        );
        assert_eq!(queued["environmentState"], "expired");
        let (status, selected) = change.move_to(json!({ "nodeId": change.source })).await;
        assert_eq!(status, StatusCode::OK, "{selected}");
        let selected = change.owner.run(&change.run).await;
        assert_eq!(selected["targetNodeId"], change.source);
        assert_eq!(
            selected["environmentExpiredAt"],
            queued["environmentExpiredAt"]
        );
        assert_eq!(change.retirements().await.len(), 1);
    }
}

#[tokio::test]
async fn revocation_sees_a_fresh_admission_before_its_checkpoint_is_persisted() {
    let change = Change::new(RunStatus::Succeeded).await;
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    change.owner.service.retire_environments().await.unwrap();
    let run = change.owner.run(&change.run).await;
    let attempt = id();
    placement::reserve(&change.owner.service, &run, &attempt)
        .await
        .unwrap();
    let admitted = change.owner.run(&change.run).await;
    assert_eq!(admitted["nodeId"], change.destination);
    assert!(admitted["environmentState"].is_null());
    let checkpoint = change
        .owner
        .service
        .store
        .checkpoint_boundary(&change.run)
        .await
        .unwrap()
        .unwrap();
    assert!(
        checkpoint["nodeId"].is_null(),
        "the worker has not bound its checkpoint yet"
    );
    let token = CancellationToken::new();
    change
        .owner
        .service
        .worker
        .active
        .lock()
        .await
        .insert(change.run.clone(), token.clone());
    let (status, revoked) = change
        .owner
        .send(
            "POST",
            &format!("/api/nodes/{}/revoke", change.destination),
            json!({ "discardWorkingFiles": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert!(token.is_cancelled());
    let expired = change.owner.run(&change.run).await;
    assert_eq!(expired["environmentState"], "expired");
    assert_eq!(expired["environmentExpirationCause"], "node-revocation");
    assert!(expired["nodeId"].is_null());
    change
        .owner
        .service
        .worker
        .active
        .lock()
        .await
        .remove(&change.run);
    change
        .owner
        .service
        .detach_stopped_environments()
        .await
        .unwrap();
    placement::release(&change.owner.service, &attempt)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_pending_expiration_holds_a_due_mission_wakeup_until_detachment() {
    let change = Change::new(RunStatus::Waiting).await;
    let wakeup = json!({ "id": id(), "messageId": "original-turn", "seconds": 1, "createdAt": now() - 2000, "wakeAt": now() - 1000, "reason": "Continue after the condition" });
    let (run, chat, wake) = (change.run.clone(), change.chat.clone(), wakeup.clone());
    change
        .owner
        .service
        .store
        .transaction(move |db| {
            db.0.execute("DELETE FROM conversations WHERE id=?", [chat])?;
            db.change_run(
                &run,
                cairn_installation::runs::RunChanges {
                    trigger: Some("manual".into()),
                    wakeup: Some(Some(serde_json::from_value(wake)?)),
                    ..Default::default()
                },
            )?;
            db.update_run_boundary(
                &run,
                &json!({ "chatExecution": null, "missionTurnId": "original-turn" }),
            )
        })
        .await
        .unwrap();
    change
        .owner
        .service
        .worker
        .active
        .lock()
        .await
        .insert(change.run.clone(), CancellationToken::new());
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    change
        .owner
        .service
        .worker
        .active
        .lock()
        .await
        .remove(&change.run);
    // The scheduler wins the wakeup race against the retirement task.
    change
        .owner
        .service
        .worker
        .tick(&change.owner.service)
        .await
        .unwrap();
    let held = change.owner.run(&change.run).await;
    assert_eq!(held["status"], RunStatus::Waiting);
    assert_eq!(held["missionTurnId"], "original-turn");
    assert_eq!(held["wakeup"], wakeup);
    change
        .owner
        .service
        .detach_stopped_environments()
        .await
        .unwrap();
    let detached = change.owner.run(&change.run).await;
    assert_eq!(detached["wakeup"]["messageId"], detached["missionTurnId"]);
    change
        .owner
        .service
        .worker
        .tick(&change.owner.service)
        .await
        .unwrap();
    let continued = change.owner.run(&change.run).await;
    assert_eq!(continued["status"], RunStatus::Queued);
    assert_eq!(continued["missionTurnId"], wakeup["id"]);
    assert!(continued["wakeup"].is_null());
}

#[tokio::test]
async fn a_stop_accepted_before_expiration_is_preserved_for_moves_and_revocation() {
    for revoke in [false, true] {
        let change = Change::new(RunStatus::Running).await;
        let (run, chat) = (change.run.clone(), change.chat.clone());
        change
            .owner
            .service
            .store
            .transaction(move |db| {
                db.0.execute("DELETE FROM conversations WHERE id=?", [chat])?;
                db.change_run(
                    &run,
                    cairn_installation::runs::RunChanges {
                        trigger: Some("manual".into()),
                        ..Default::default()
                    },
                )?;
                db.update_run_boundary(&run, &json!({ "chatExecution": null }))
            })
            .await
            .unwrap();
        change
            .owner
            .service
            .worker
            .active
            .lock()
            .await
            .insert(change.run.clone(), CancellationToken::new());
        change
            .owner
            .service
            .worker
            .cancel(&change.owner.service, &change.run)
            .await
            .unwrap();
        let stopped_at = change.owner.run(&change.run).await["cancelRequestedAt"].clone();
        assert!(stopped_at.is_number());
        let (status, expired) = if revoke {
            change
                .owner
                .send(
                    "POST",
                    &format!("/api/nodes/{}/revoke", change.source),
                    json!({ "discardWorkingFiles": true, "releasePlacements": true }),
                )
                .await
        } else {
            change
                .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
                .await
        };
        assert_eq!(status, StatusCode::OK, "{expired}");
        change
            .owner
            .service
            .worker
            .active
            .lock()
            .await
            .remove(&change.run);
        change
            .owner
            .service
            .detach_stopped_environments()
            .await
            .unwrap();
        let current = change.owner.run(&change.run).await;
        assert_eq!(current["status"], RunStatus::Cancelled);
        assert_eq!(current["environmentState"], "expired");
        assert_eq!(current["cancelRequestedAt"], stopped_at);
    }
}

#[tokio::test]
async fn a_stale_capacity_check_cannot_erase_a_replacement_destination() {
    let change = Change::new(RunStatus::Succeeded).await;
    let replacement = id();
    change
        .owner
        .put_node_fixture(schedulable_node(&replacement, &limits(4, 8192, 65536)))
        .await;
    change
        .owner
        .grant_nodes(
            &change.agent,
            json!([change.source, change.destination, replacement]),
        )
        .await;
    let (status, moved) = change
        .move_to(json!({ "nodeId": change.destination, "discardWorkingFiles": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    let stale = change.owner.run(&change.run).await;
    change
        .owner
        .grant_nodes(&change.agent, json!([change.source, replacement]))
        .await;
    let (status, selected) = change.move_to(json!({ "nodeId": replacement })).await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert!(
        placement::check(&change.owner.service, &stale)
            .await
            .is_err()
    );
    assert_eq!(
        change.owner.run(&change.run).await["targetNodeId"],
        replacement
    );
}
