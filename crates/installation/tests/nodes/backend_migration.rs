use super::*;
use common::run_fixture::RunFixture;

async fn wait_migration(owner: &Owner, run: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let value = owner.run(run).await;
            if !value["backendMigration"].is_object()
                || value["backendMigration"]["process"].is_null()
            {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap()
}

/// Seeds `node` with a VM disk, or with a disk created natively when `native`.
async fn seed_ext4(owner: &Owner, record: &Value, node: &str, master: &str, native: bool) {
    let run = record["id"].as_str().unwrap();
    let tree = owner.root().join(if native {
        "native-tree"
    } else {
        "migration-tree"
    });
    let (workspace, home, output) = if native {
        ("workspaces", "home/.codex/sessions", "output")
    } else {
        (
            "upper/data/runs/test/workspace",
            "upper/home/node/.codex/sessions",
            "upper/data/runs/test/output",
        )
    };
    let workspace = tree.join(workspace);
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("uncommitted.txt"), MOVED_CONTENTS).unwrap();
    let home = tree.join(home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("history"), b"engine context").unwrap();
    std::fs::create_dir_all(tree.join(output)).unwrap();
    let raw = owner.root().join(if native {
        "native-source.ext4"
    } else {
        "migration-source.ext4"
    });
    std::fs::File::create(&raw)
        .unwrap()
        .set_len(128 * MIB as u64)
        .unwrap();
    assert!(
        tokio::process::Command::new("mkfs.ext4")
            .args(["-q", "-F", "-d"])
            .arg(&tree)
            .arg(&raw)
            .status()
            .await
            .unwrap()
            .success()
    );
    let directory = owner.root().join(node).join("disks").join(run);
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    std::fs::create_dir_all(&directory).unwrap();
    let runtime = if native {
        let node = owner.stored("nodes", node).await.unwrap();
        json!({ "backend": "native", "storage": "ublk", "os": "linux", "arch": "x86_64", "runtimeId": node["runtimeId"] })
    } else {
        json!({ "runtimeId": "development", "backend": "firecracker", "storage": "ublk" })
    };
    std::fs::write(directory.join("runtime.json"), runtime.to_string()).unwrap();
    let grant = disk_grants::new_disk(&owner.service, record, node)
        .await
        .unwrap();
    let context = json!({ "master": master, "grant": grant, "policy": small_reserve() });
    let remote = Arc::new(
        RemoteSource::new(
            &context,
            tokio::runtime::Handle::current(),
            CancellationToken::default(),
        )
        .unwrap(),
    );
    let manifest = json!({ "version": 1, "size": 128 * MIB, "blockSize": 4_194_304,
        "blocks": (0..32).map(|i| json!({ "offset": i * 4 * MIB, "size": 4 * MIB, "hash": null })).collect::<Vec<_>>() });
    let journal = LazyDisk::create(&directory.join("lazy"), &manifest, remote).unwrap();
    journal.set_context(&context).unwrap();
    let bytes = std::fs::read(raw).unwrap();
    for (i, chunk) in bytes.chunks(4 * MIB).enumerate() {
        journal.write_at((i * 4 * MIB) as u64, chunk).unwrap();
    }
    journal.sync().unwrap();
}

async fn partial_cold_write(owner: &Owner, node: &str, run: &str) {
    let directory = owner.root().join(node).join("disks").join(run);
    let volume = runtime::load(&directory).await.unwrap();
    let mut context = LazyDisk::context(&directory.join("lazy")).unwrap();
    context["policy"]["memoryCacheMiB"] = 4.into();
    volume.disk.set_context(&context).unwrap();
    let current = owner.run(run).await;
    let point = owner
        .stored("node-backups", current["backup"]["id"].as_str().unwrap())
        .await
        .unwrap();
    let manifest = publication::manifest(&owner.service, &point).await.unwrap();
    let offset = manifest["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|block| block["hash"].is_string() && block["offset"].as_u64().unwrap() > 0)
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    let disk = volume.clone();
    tokio::task::spawn_blocking(move || disk.read_at(offset, &mut vec![0; 4 * MIB]).unwrap())
        .await
        .unwrap();
    let cache = directory.join("lazy/cache");
    if cache.exists() {
        std::fs::remove_dir_all(&cache).unwrap();
    }
    std::fs::create_dir_all(cache).unwrap();
    // Preserve bytes while creating a partial dirty frame in a nonzero ext4
    // block. Capturing it must demand-read the uncached acknowledged S3 base.
    volume.disk.write_at(0, &[0]).unwrap();
    volume.disk.sync().unwrap();
}

async fn assert_files(owner: &Owner, node: &str, run: &str, native: bool) {
    let directory = owner.root().join(node).join("disks").join(run);
    let volume = runtime::load(&directory).await.unwrap();
    let bytes = tokio::task::spawn_blocking(move || {
        let mut bytes = vec![0; 128 * MIB];
        for (i, chunk) in bytes.chunks_mut(4 * MIB).enumerate() {
            volume.read_at((i * 4 * MIB) as u64, chunk).unwrap();
        }
        bytes
    })
    .await
    .unwrap();
    let disk = owner.root().join("assert.ext4");
    std::fs::write(&disk, bytes).unwrap();
    let fs = ext4_view::Ext4::load(Box::new(std::fs::File::open(disk).unwrap())).unwrap();
    let path = if native {
        "/workspaces/uncommitted.txt"
    } else {
        "/upper/data/runs/test/workspace/uncommitted.txt"
    };
    assert_eq!(fs.read(path).unwrap(), MOVED_CONTENTS);
    let home = if native {
        "/home/.codex/sessions/history"
    } else {
        "/upper/home/node/.codex/sessions/history"
    };
    assert_eq!(fs.read(home).unwrap(), b"engine context");
}

async fn runtime(owner: &Owner, node: &str, run: &str) -> Value {
    let path = owner
        .root()
        .join(node)
        .join("disks")
        .join(run)
        .join("runtime.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

async fn agent_move(owner: &Owner, run: &str, args: Value) -> Value {
    let bearer = id();
    let current = owner.run(run).await;
    let grant = cairn_installation::mcps::record::RunGrant {
        run_id: run.to_owned(),
        message_id: current["missionTurnId"].as_str().map(str::to_owned),
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

async fn move_to(owner: &Owner, run: &str, node: &str) -> Value {
    let (status, response) = owner
        .send(
            "POST",
            &format!("/api/nodes/placement/{run}/move"),
            json!({ "nodeId": node }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    response
}

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py --case backend_migration"]
async fn backend_migration_keeps_chat_and_files_both_directions_and_preserves_source_on_failure() {
    let (listener, address) = common::bind().await;
    let owner = Owner::at(address.to_string()).await;
    owner
        .put_node_fixture(json!({ "id": LOCAL_NODE_ID, "storage": small_reserve() }))
        .await;
    let (run, agent, chat, vm_node, native_node) = (id(), id(), id(), id(), id());
    // One agent may use both machines: each conversation keeps its own backend.
    let profile = json!({ "id": agent, "revision": id(), "access": { "backend": "auto", "nodes": [vm_node, native_node] } });
    owner.put("agents", profile.clone()).await;
    let mut record = run_record(&run, RunStatus::Succeeded);
    record["isolated"] = true.into();
    record["nodeId"] = vm_node.clone().into();
    record["sessionId"] = "old-engine-session".into();
    record["executionBackend"] = "firecracker".into();
    record["resources"] = limits(1, 512, 128);
    record["snapshot"] = json!({ "agent": profile });
    owner.add_run_fixture(&record).await;
    owner.save_conversation_boundary( json!({ "id": chat, "runId": run, "agentId": agent, "paused": false, "title": "same conversation" })).await;
    let message = json!({ "id": id(), "chatId": chat, "text": "keep this history", "createdAt": now(), "status": "delivered", "provider": "codex", "model": "retained-model", "reasoning": "high", "speed": "fast", "mode": "steer", "attachments": [{ "id": id(), "chatId": chat, "name": "retained-notes.txt", "size": 17, "mediaType": "text/plain", "kind": "file", "digest": "retained-digest", "createdAt": now() }] });
    let saved = message.clone();
    let message = owner
        .service
        .store
        .transaction(move |db| db.put_message(&saved))
        .await
        .unwrap();
    let prepared = json!({ "backend": "firecracker", "isolated": true, "projectRoot": "/data/runs/test/workspace", "output": "/data/runs/test/output/result.md" });
    owner.set_checkpoint(&run, json!({ "nodeId": vm_node, "runnerId": run, "runtimeId": "development", "completed": true, "prepared": prepared })).await;
    let stop = CancellationToken::new();
    let faults = MoveFaults {
        failed: Arc::new(AtomicBool::new(false)),
        cancel_during_capture: Arc::new(AtomicBool::new(false)),
    };
    let mut tasks = Vec::new();
    // This fixture has no Beacon authority server. Model its fresh owner
    // membership independently of slow CI filesystem/S3 timing.
    let authority = owner.service.store.clone();
    tasks.push(tokio::spawn(async move {
        loop {
            authority.set("native-authority", json!({ "observedAt": now(), "accounts": { "fixture-owner": { "email": "fixture@example.test" } } }), None).await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }));
    for node in [&vm_node, &native_node] {
        tasks.extend(connect_node(&owner, &record, node, address, &faults, &stop, false).await);
    }
    let native_image = owner
        .root()
        .join(&native_node)
        .join("images/native-fixture");
    std::fs::create_dir_all(&native_image).unwrap();
    std::fs::write(native_image.join("root.ext4"), b"fixture native tools").unwrap();
    std::fs::write(native_image.join("vmlinux"), b"fixture transport").unwrap();
    let mut native = owner.stored("nodes", &native_node).await.unwrap();
    native["runtimeId"] = "development".into();
    native["capabilities"]["nativeModeProtocol"] = 1.into();
    native["nativeModeObserved"] =
        json!({ "mode": "protected", "generation": null, "status": "applied" });
    native["capabilities"]["backend"] = "native".into();
    native["capabilities"]["ublk"] = true.into();
    native["capabilities"]["vmDisks"] = true.into();
    native["nativeEnabled"] = true.into();
    native["nativeAccounts"] = json!(["fixture-owner"]);
    owner.put_node_fixture(native).await;
    let mut vm_record = owner.stored("nodes", &vm_node).await.unwrap();
    vm_record["capabilities"]["backend"] = "firecracker".into();
    vm_record["tags"] = json!(["cpu"]);
    owner.put_node_fixture(vm_record).await;
    seed_ext4(
        &owner,
        &record,
        &vm_node,
        &format!("http://{address}/"),
        false,
    )
    .await;
    tasks.push(owner.serve(listener));
    publication::capture(&owner.service, &owner.run(&run).await)
        .await
        .unwrap();

    // A VM disk moves to the native node and back without conversion: the
    // same files keep their VM layout, and the engine session is retained.
    for (destination, backend) in [(&native_node, "native"), (&vm_node, "firecracker")] {
        let expected_status = if backend == "firecracker" {
            RunStatus::Failed
        } else {
            RunStatus::Succeeded
        };
        owner
            .service
            .store
            .change_run(
                &run,
                cairn_installation::runs::RunChanges {
                    status: Some(expected_status),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let response = move_to(&owner, &run, destination).await;
        assert_eq!(response["status"], "moving", "{response}");
        let pending = owner.run(&run).await;
        assert_eq!(pending["moveRequest"]["idle"], true, "{pending}");
        assert_eq!(pending["moveRequest"]["backend"], backend);
        moves::advance(&owner.service, &pending).await.unwrap();
        let settled = owner.run(&run).await;
        assert!(settled["movementError"].is_null(), "{settled}");
        assert_eq!(settled["nodeId"], *destination, "{settled}");
        assert_eq!(settled["status"], expected_status);
        assert_eq!(settled["executionBackend"], backend);
        assert_eq!(settled["sessionId"], "old-engine-session");
        assert!(settled["backendMigration"].is_null());
        let checkpoint = owner
            .service
            .store
            .checkpoint_boundary(&run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint["prepared"]["backend"], backend);
        assert_eq!(checkpoint["runtimeId"], "development");
        let next = id();
        let placement = placement::reserve(&owner.service, &settled, &next)
            .await
            .unwrap();
        assert_eq!(placement["runtimeId"], checkpoint["runtimeId"]);
        placement::release(&owner.service, &next).await.unwrap();
        assert_eq!(checkpoint["prepared"]["diskFormat"], "vm");
        let identity = runtime(&owner, destination, &run).await;
        if backend == "native" {
            assert_eq!(identity["backend"], "native", "{identity}");
            assert_eq!(identity["disk"], "vm");
            assert_eq!(identity["vmRuntimeId"], "development");
        } else {
            assert_eq!(identity, json!({ "runtimeId": "development" }));
        }
        assert_files(&owner, destination, &run, false).await;
        let saved_chat = owner.stored("chats", &chat).await.unwrap();
        assert_eq!(saved_chat["paused"], false);
        assert_ne!(saved_chat["sessionRestartRequested"], true);
    }

    // A disk created on the native node is converted once before a VM runs it.
    seed_ext4(
        &owner,
        &record,
        &native_node,
        &format!("http://{address}/"),
        true,
    )
    .await;
    let mut native_prepared = prepared.clone();
    native_prepared["backend"] = "native".into();
    owner
        .set_checkpoint(
            &run,
            json!({
                "nodeId": native_node, "runnerId": run, "runtimeId": "development",
                "completed": true, "terminalCode": 70, "providerFailed": true,
                "outputInterrupted": true, "lastError": "Retired native attempt failed",
                "finalizationFailureAttempt": run, "remainingMs": null,
                "prepared": native_prepared.clone()
            }),
        )
        .await;
    let mut native_snapshot = owner.run(&run).await["snapshot"].clone();
    native_snapshot["agent"]["access"]["backend"] = "native".into();
    owner
        .service
        .store
        .change_run_fixture(
            &run,
            json!({
                "snapshot": native_snapshot,
                "status": RunStatus::Failed,
                "nodeId": native_node,
                "executionBackend": "native",
                "nativePrincipals": ["fixture-owner"],
                "nativeAuthorityNodes": [native_node],
                "nativeProvenanceUnknown": false,
            }),
        )
        .await
        .unwrap();
    publication::capture(&owner.service, &owner.run(&run).await)
        .await
        .unwrap();
    let sending = json!({ "id": id(), "chatId": chat, "text": "stop this input", "createdAt": now(), "status": "sending" });
    let queued = json!({ "id": id(), "chatId": chat, "text": "independent queued follow-up", "createdAt": now() + 1, "status": "queued" });
    let (active_input, later_input) = (sending.clone(), queued.clone());
    let (sending, queued) = owner
        .service
        .store
        .transaction(move |db| {
            Ok((
                db.put_message(&active_input)?,
                db.put_message(&later_input)?,
            ))
        })
        .await
        .unwrap();
    let parent = id();
    let mut parent_run = run_record(&parent, RunStatus::Cancelled);
    parent_run["cancelRequestedAt"] = now().into();
    owner.add_run_fixture(&parent_run).await;
    owner
        .service
        .store
        .change_run_fixture(&run, json!({ "nativeParentRunId": parent }))
        .await
        .unwrap();
    let mut delegated_chat = owner.stored("chats", &chat).await.unwrap();
    delegated_chat["nativeParentRunId"] = parent.clone().into();
    owner.save_conversation_boundary(delegated_chat).await;
    let before = owner.run(&run).await;
    let (preview_status, preview) = owner
        .send(
            "GET",
            &format!("/api/nodes/placement/{run}/backend-migration"),
            Value::Null,
        )
        .await;
    assert_eq!(preview_status, StatusCode::OK, "{preview}");
    assert_eq!(
        preview["mission"], false,
        "A chat's run ID must resolve its actual conversation"
    );
    let resume_chat = || async {
        let mut current = owner.stored("chats", &chat).await.unwrap();
        current["paused"] = false.into();
        owner.save_conversation_boundary(current).await;
    };

    // The manager's own reserve bounds conversion staging.
    owner
        .put_node_fixture(
            json!({ "id": LOCAL_NODE_ID, "storage": { "reserveMiB": 16 * 1024 * 1024 } }),
        )
        .await;
    assert_eq!(move_to(&owner, &run, &vm_node).await["status"], "migrating");
    let refused = wait_migration(&owner, &run).await;
    assert!(
        refused["backendMigrationError"]
            .as_str()
            .unwrap()
            .contains("temporary disk space"),
        "{refused}"
    );
    assert_eq!(refused["nodeId"], native_node);
    assert_eq!(refused["executionBackend"], "native");
    assert!(refused["backendMigration"].is_null());
    owner
        .put_node_fixture(json!({ "id": LOCAL_NODE_ID, "storage": { "reserveMiB": 64 } }))
        .await;

    // A failed capture retains the original environment and allows a retry.
    resume_chat().await;
    faults.failed.store(true, Ordering::SeqCst);
    assert_eq!(move_to(&owner, &run, &vm_node).await["status"], "migrating");
    let failed = wait_migration(&owner, &run).await;
    assert!(failed["backendMigrationError"].is_string(), "{failed}");
    assert_eq!(failed["nodeId"], before["nodeId"]);
    assert!(
        failed["backendMigration"].is_null(),
        "a pre-conversion failure must allow repairing the source"
    );
    assert_files(&owner, &native_node, &run, true).await;
    faults.failed.store(false, Ordering::SeqCst);

    // Conversion demand-reads cold blocks of the acknowledged S3 base.
    partial_cold_write(&owner, &native_node, &run).await;
    resume_chat().await;
    let (status, migrating) = owner
        .send(
            "POST",
            &format!("/api/nodes/placement/{run}/backend-migration"),
            json!({"nodeId": vm_node, "backend": "firecracker", "confirm": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{migrating}");
    assert_eq!(migrating["status"], "migrating");
    let converted = wait_migration(&owner, &run).await;
    assert!(converted["backendMigrationError"].is_null(), "{converted}");
    assert_eq!(converted["nodeId"], vm_node, "{converted}");
    assert_eq!(converted["executionBackend"], "firecracker");
    assert!(converted["nativeParentRunId"].is_null());
    assert_eq!(converted["nativePrincipals"], json!(["fixture-owner"]));
    assert!(converted["sessionId"].is_null());
    let checkpoint = owner
        .service
        .store
        .checkpoint_boundary(&run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint["prepared"]["backend"], "firecracker");
    assert_eq!(checkpoint["prepared"]["diskFormat"], "vm");
    let saved_chat = owner.stored("chats", &chat).await.unwrap();
    assert_eq!(saved_chat["runId"], run);
    assert_eq!(saved_chat["title"], "same conversation");
    assert_eq!(saved_chat["paused"], false);
    assert_eq!(saved_chat["sessionRestartRequested"], true);
    assert!(saved_chat["nativeParentRunId"].is_null());
    let key = chat.clone();
    let messages = owner
        .service
        .store
        .read(move |db| db.messages(&key))
        .await
        .unwrap();
    let mut cancelled = sending.clone();
    cancelled["status"] = "cancelled".into();
    assert_ne!(messages[1]["revision"], sending["revision"]);
    cancelled["revision"] = messages[1]["revision"].clone();
    assert_eq!(messages, vec![message.clone(), cancelled, queued.clone()]);
    assert_files(&owner, &vm_node, &run, false).await;
    // The VM image constraint from conversion must not poison the next native
    // admission, including an idle move whose reservation has been released.
    assert!(owner.run(&run).await["sessionId"].is_null());
    // Keep the queued follow-up from starting a turn: the converted environment
    // must be movable through scheduler recovery before any fresh engine session.
    let mut paused_chat = owner.stored("chats", &chat).await.unwrap();
    paused_chat["paused"] = true.into();
    owner.save_conversation_boundary(paused_chat).await;
    for destination in [&native_node, &vm_node] {
        let before_move = owner.run(&run).await;
        let response = moves::request_by_agent(
            &owner.service,
            &owner.run(&run).await,
            &json!({ "nodeId": destination }),
        )
        .await
        .unwrap();
        assert_eq!(response["status"], "moving");
        let settled = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                owner.service.worker.tick(&owner.service).await.unwrap();
                let current = owner.run(&run).await;
                if current["moveRequest"].is_null() {
                    break current;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(settled["movementError"].is_null(), "{settled}");
        assert_eq!(settled["nodeId"], *destination);
        assert_eq!(settled["status"], RunStatus::Succeeded);
        assert_eq!(settled["startedAt"], before_move["startedAt"]);
        assert_eq!(settled["resumeCount"], before_move["resumeCount"]);
        assert!(
            settled["sessionId"].is_null(),
            "An idle move must not invent an engine session"
        );
        assert!(settled["requiredRuntime"].is_null());
        let next = id();
        let selected = placement::reserve(&owner.service, &settled, &next)
            .await
            .unwrap();
        assert_eq!(selected["runtimeId"], "development");
        placement::release(&owner.service, &next).await.unwrap();
    }

    // Missions have no chats record. An agent's native-born mission converts
    // and continues automatically in a fresh session with its visible history.
    let key = chat.clone();
    owner
        .service
        .store
        .transaction(move |db| db.remove_conversation(&key))
        .await
        .unwrap();
    seed_ext4(
        &owner,
        &record,
        &native_node,
        &format!("http://{address}/"),
        true,
    )
    .await;
    owner
        .set_checkpoint(
            &run,
            json!({
                "nodeId": native_node, "runnerId": run, "runtimeId": "development",
                "completed": false, "terminalCode": 70, "providerFailed": true,
                "outputInterrupted": true, "lastError": "Retired mission attempt failed",
                "remainingMs": 12345, "prepared": native_prepared
            }),
        )
        .await;
    let mission = owner.service.task_fixture(json!({"name": "Original mission", "prompt": "Continue the original mission", "agentId": agent}), None).await.unwrap();
    let mut snapshot = owner.run(&run).await["snapshot"].clone();
    snapshot["task"] = mission.clone();
    snapshot["agent"]["access"]["backend"] = "native".into();
    owner.service.store.change_run_fixture(&run, json!({ "nodeId": native_node, "executionBackend": "native", "status": RunStatus::Running, "trigger": "manual", "taskId": mission["id"], "chatExecution": null, "requiredRuntime": null, "snapshot": snapshot })).await.unwrap();
    owner.service.store.event(&run, &cairn_contracts::EventKind::ItemCompleted, "mission progress", Some(json!({ "item": { "id": "progress", "type": "agent_message", "text": "Already completed the first step" } }))).await.unwrap();
    let (status, preview) = owner
        .get(&format!("/api/nodes/placement/{run}/backend-migration"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["mission"], true);
    assert_eq!(preview["diskFormat"], "native");
    let refused = agent_move(
        &owner,
        &run,
        json!({"nodeId": vm_node, "requiredTags": ["missing"]}),
    )
    .await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(text(&refused["content"][0], "text").contains("required tags"));
    assert_eq!(owner.run(&run).await["status"], RunStatus::Running);
    faults.failed.store(true, Ordering::SeqCst);
    let response = agent_move(&owner, &run, json!({"nodeId": vm_node})).await;
    assert_ne!(response["isError"], true, "{response}");
    assert_eq!(response["structuredContent"]["status"], "migrating");
    let interrupted = wait_migration(&owner, &run).await;
    assert!(interrupted["backendMigrationError"].is_string());
    assert_eq!(interrupted["status"], RunStatus::Cancelled);
    assert!(interrupted["backendMigration"].is_null());
    faults.failed.store(false, Ordering::SeqCst);
    let (status, response) = owner
        .send(
            "POST",
            &format!("/api/nodes/placement/{run}/move"),
            json!({ "nodeId": vm_node, "requiredTags": ["cpu"], "waitSeconds": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["status"], "migrating");
    let mission = wait_migration(&owner, &run).await;
    assert!(mission["backendMigrationError"].is_null(), "{mission}");
    assert_eq!(mission["status"], RunStatus::Queued);
    assert_eq!(mission["executionBackend"], "firecracker");
    assert!(mission["sessionId"].is_null());
    assert_eq!(mission["recoveryPending"], true);
    assert_eq!(mission["requiredTags"], json!(["cpu"]));
    let mission_checkpoint = owner
        .service
        .store
        .checkpoint_boundary(&run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        mission_checkpoint["remainingMs"], 12345,
        "Conversion must not renew a mission's time budget"
    );
    assert!(mission_checkpoint["terminalCode"].is_null());
    assert_ne!(mission_checkpoint["providerFailed"], true);
    assert_ne!(mission_checkpoint["outputInterrupted"], true);
    assert!(mission_checkpoint["lastError"].is_null());
    let prompt = cairn_installation::run_output::prompt(&mission, false);
    assert!(prompt.contains("Continue the original mission"));
    assert!(prompt.contains("Already completed the first step"));
    assert_files(&owner, &vm_node, &run, false).await;
    // Drive real scheduler recovery, not moves::advance: a retained stopped
    // environment must relocate without replaying its old terminal turn.
    for (index, (status, destination)) in [
        (RunStatus::Failed, &native_node),
        (RunStatus::Cancelled, &vm_node),
    ]
    .into_iter()
    .enumerate()
    {
        let mut saved = owner
            .service
            .store
            .checkpoint_boundary(&run)
            .await
            .unwrap()
            .unwrap();
        saved["completed"] = true.into();
        saved["terminalCode"] = 70.into();
        saved["remainingMs"] = Value::Null;
        saved["runnerId"] = run.clone().into();
        saved["settled"] = json!({ "status": status, "summary": "Retained stopped work", "error": null, "finishedAt": now() });
        owner.set_checkpoint(&run, saved).await;
        owner.service.store.change_run_fixture(&run, json!({
            "status": status, "recoveryPending": false, "requiredTags": [],
            "cancelRequestedAt": if status == RunStatus::Cancelled { json!(now()) } else { Value::Null },
        })).await.unwrap();
        if index == 0 {
            owner
                .service
                .worker
                .start(owner.service.clone())
                .await
                .unwrap();
        }
        let before_move = owner.run(&run).await;
        let response = move_to(&owner, &run, destination).await;
        assert_eq!(response["status"], "moving");
        let settled = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let current = owner.run(&run).await;
                if current["moveRequest"].is_null() {
                    break current;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(settled["movementError"].is_null(), "{settled}");
        assert_eq!(
            settled["nodeId"], *destination,
            "Stopped move was discarded: {settled}"
        );
        assert_eq!(settled["status"], status);
        assert!(!settled["recoveryPending"].as_bool().unwrap());
        assert!(settled["sessionId"].is_null());
        assert_eq!(settled["startedAt"], before_move["startedAt"]);
        assert_eq!(settled["resumeCount"], before_move["resumeCount"]);
        assert_files(&owner, destination, &run, false).await;
    }
    owner.service.shutdown.cancel();
    owner.service.worker.close().await;
    stop.cancel();
    for task in tasks {
        task.abort();
    }
}
