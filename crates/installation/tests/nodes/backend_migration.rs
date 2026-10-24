use super::*;

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
        json!({ "runtimeId": "fixture", "backend": "firecracker", "storage": "ublk" })
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
async fn backend_moves_remount_vm_disks_and_convert_native_ones_once() {
    let (listener, address) = common::bind().await;
    let owner = Owner::at(address.to_string()).await;
    let (run, agent, chat, vm_node, native_node) = (id(), id(), id(), id(), id());
    // One agent may use both machines: each conversation keeps its own backend.
    let profile =
        json!({ "id": agent, "revision": 1, "access": { "nodes": [vm_node, native_node] } });
    owner.put("agents", profile.clone()).await;
    let mut record = run_record(&run, RunStatus::Succeeded);
    record["isolated"] = true.into();
    record["nodeId"] = vm_node.clone().into();
    record["sessionId"] = "old-engine-session".into();
    record["executionBackend"] = "firecracker".into();
    record["resources"] = limits(1, 512, 128);
    record["snapshot"] = json!({ "agent": profile });
    owner.add_run(&record).await;
    owner.put("chats", json!({ "id": chat, "runId": run, "agentId": agent, "paused": false, "title": "same conversation" })).await;
    let message = json!({ "id": id(), "chatId": chat, "text": "keep this history", "createdAt": now(), "status": "delivered" });
    let saved = message.clone();
    owner
        .service
        .store
        .transaction(move |db| db.put_message(&saved))
        .await
        .unwrap();
    let prepared = json!({ "backend": "firecracker", "isolated": true, "projectRoot": "/data/runs/test/workspace", "output": "/data/runs/test/output/result.md" });
    owner.set_checkpoint(&run, json!({ "nodeId": vm_node, "runnerId": run, "runtimeId": "fixture", "completed": true, "prepared": prepared })).await;
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
    native["runtimeId"] = "native-fixture".into();
    native["runtimeIds"] = json!(["native-fixture"]);
    native["runtimes"] = json!(["native-fixture"]);
    native["capabilities"]["backend"] = "native".into();
    native["capabilities"]["ublk"] = true.into();
    native["capabilities"]["vmDisks"] = true.into();
    native["nativeEnabled"] = true.into();
    native["nativeAccounts"] = json!(["fixture-owner"]);
    owner.put("nodes", native).await;
    let mut vm_record = owner.stored("nodes", &vm_node).await.unwrap();
    vm_record["capabilities"]["backend"] = "firecracker".into();
    vm_record["tags"] = json!(["cpu"]);
    owner.put("nodes", vm_record).await;
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
        let response = move_to(&owner, &run, destination).await;
        assert_eq!(response["backend"], backend, "{response}");
        let pending = owner.run(&run).await;
        assert_eq!(pending["moveRequest"]["idle"], true, "{pending}");
        assert_eq!(pending["moveRequest"]["backend"], backend);
        moves::advance(&owner.service, &pending).await.unwrap();
        let settled = owner.run(&run).await;
        assert!(settled["movementError"].is_null(), "{settled}");
        assert_eq!(settled["nodeId"], *destination, "{settled}");
        assert_eq!(settled["status"], RunStatus::Succeeded);
        assert_eq!(settled["executionBackend"], backend);
        assert_eq!(settled["sessionId"], "old-engine-session");
        assert!(settled["backendMigration"].is_null());
        let checkpoint = owner
            .service
            .store
            .kv(&format!("run-checkpoint:{run}"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint["prepared"]["backend"], backend);
        assert_eq!(
            checkpoint["runtimeId"],
            if backend == "native" {
                "native-fixture"
            } else {
                "fixture"
            }
        );
        let next = id();
        let placement = placement::reserve(&owner.service, &settled, &next)
            .await
            .unwrap();
        assert_eq!(placement["runtimeId"], checkpoint["runtimeId"]);
        placement::release(&owner.service, &next).await.unwrap();
        assert_eq!(checkpoint["diskFormat"], "vm");
        assert_eq!(checkpoint["diskRuntimeId"], "fixture");
        let identity = runtime(&owner, destination, &run).await;
        if backend == "native" {
            assert_eq!(identity["backend"], "native", "{identity}");
            assert_eq!(identity["disk"], "vm");
            assert_eq!(identity["vmRuntimeId"], "fixture");
        } else {
            assert_eq!(identity, json!({ "runtimeId": "fixture" }));
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
    owner.set_checkpoint(&run, json!({ "nodeId": native_node, "runnerId": run, "runtimeId": "native-fixture", "completed": true, "prepared": native_prepared.clone() })).await;
    owner
        .service
        .store
        .patch_run(
            &run,
            json!({
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
    owner
        .service
        .store
        .transaction(move |db| {
            db.put_message(&active_input)?;
            db.put_message(&later_input)?;
            Ok(())
        })
        .await
        .unwrap();
    let parent = id();
    let mut parent_run = run_record(&parent, RunStatus::Cancelled);
    parent_run["cancelRequestedAt"] = now().into();
    owner.add_run(&parent_run).await;
    owner
        .service
        .store
        .patch_run(&run, json!({ "nativeParentRunId": parent }))
        .await
        .unwrap();
    let mut delegated_chat = owner.stored("chats", &chat).await.unwrap();
    delegated_chat["nativeParentRunId"] = parent.clone().into();
    owner.put("chats", delegated_chat).await;
    let before = owner.run(&run).await;
    let resume_chat = || async {
        let mut current = owner.stored("chats", &chat).await.unwrap();
        current["paused"] = false.into();
        owner.put("chats", current).await;
    };

    // The manager's own reserve bounds conversion staging.
    owner
        .put(
            "nodes",
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
        .put(
            "nodes",
            json!({ "id": LOCAL_NODE_ID, "storage": { "reserveMiB": 64 } }),
        )
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
    assert_eq!(move_to(&owner, &run, &vm_node).await["status"], "migrating");
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
        .kv(&format!("run-checkpoint:{run}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint["prepared"]["backend"], "firecracker");
    assert_eq!(checkpoint["diskFormat"], "vm");
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
    assert_eq!(messages, vec![message.clone(), cancelled, queued.clone()]);
    assert_files(&owner, &vm_node, &run, false).await;
    // The VM image constraint from conversion must not poison the next native
    // admission, including an idle move whose reservation has been released.
    owner
        .service
        .store
        .patch_run(&run, json!({ "sessionId": "converted-session" }))
        .await
        .unwrap();
    for destination in [&native_node, &vm_node] {
        let response = moves::request_by_agent(
            &owner.service,
            &owner.run(&run).await,
            &json!({ "nodeId": destination }),
        )
        .await
        .unwrap();
        assert_eq!(response["status"], "moving");
        moves::advance(&owner.service, &owner.run(&run).await)
            .await
            .unwrap();
        let settled = owner.run(&run).await;
        assert!(settled["movementError"].is_null(), "{settled}");
        assert_eq!(settled["nodeId"], *destination);
        assert!(settled["requiredRuntime"].is_null());
        let next = id();
        let selected = placement::reserve(&owner.service, &settled, &next)
            .await
            .unwrap();
        assert_eq!(
            selected["runtimeId"],
            if destination == &native_node {
                "native-fixture"
            } else {
                "fixture"
            }
        );
        placement::release(&owner.service, &next).await.unwrap();
    }

    // Missions have no chats record. An agent's native-born mission converts
    // and continues automatically in a fresh session with its visible history.
    let key = chat.clone();
    owner
        .service
        .store
        .transaction(move |db| db.remove("chats", &key))
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
    owner.set_checkpoint(&run, json!({ "nodeId": native_node, "runnerId": run, "runtimeId": "native-fixture", "completed": false, "prepared": native_prepared })).await;
    let mut snapshot = owner.run(&run).await["snapshot"].clone();
    snapshot["task"] = json!({ "prompt": "Continue the original mission" });
    owner.service.store.patch_run(&run, json!({ "nodeId": native_node, "executionBackend": "native", "status": RunStatus::Running, "trigger": "manual", "chatExecution": null, "requiredRuntime": null, "snapshot": snapshot })).await.unwrap();
    owner.service.store.event(&run, "item.completed", "mission progress", Some(json!({ "item": { "id": "progress", "type": "agent_message", "text": "Already completed the first step" } }))).await.unwrap();
    let (status, preview) = owner
        .get(&format!("/api/nodes/placement/{run}/backend-migration"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["mission"], true);
    assert_eq!(preview["diskFormat"], "native");
    let refused = moves::request_by_agent(
        &owner.service,
        &owner.run(&run).await,
        &json!({ "nodeId": vm_node, "requiredTags": ["missing"] }),
    )
    .await
    .unwrap_err();
    assert!(refused.message.contains("required tags"));
    assert_eq!(owner.run(&run).await["status"], RunStatus::Running);
    faults.failed.store(true, Ordering::SeqCst);
    let response = moves::request_by_agent(
        &owner.service,
        &owner.run(&run).await,
        &json!({ "nodeId": vm_node }),
    )
    .await
    .unwrap();
    assert_eq!(response["status"], "migrating");
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
    let prompt = cairn_installation::run_output::prompt(&mission, false);
    assert!(prompt.contains("Continue the original mission"));
    assert!(prompt.contains("Already completed the first step"));
    assert_files(&owner, &vm_node, &run, false).await;
    stop.cancel();
    for task in tasks {
        task.abort();
    }
}
