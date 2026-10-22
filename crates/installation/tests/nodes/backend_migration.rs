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

async fn seed_ext4(owner: &Owner, record: &Value, node: &str, master: &str) {
    let run = record["id"].as_str().unwrap();
    let tree = owner.root().join("migration-tree");
    let workspace = tree.join("upper/data/runs/test/workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("uncommitted.txt"), MOVED_CONTENTS).unwrap();
    let home = tree.join("upper/home/node/.codex/sessions");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("history"), b"engine context").unwrap();
    std::fs::create_dir_all(tree.join("upper/data/runs/test/output")).unwrap();
    let raw = owner.root().join("migration-source.ext4");
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
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("runtime.json"),
        br#"{"runtimeId":"fixture","backend":"firecracker","storage":"ublk"}"#,
    )
    .unwrap();
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

#[tokio::test]
#[ignore = "requires loopback S3; run scripts/tests/node_s3_test.py --case backend_migration"]
async fn backend_migration_keeps_chat_and_files_both_directions_and_preserves_source_on_failure() {
    let (listener, address) = common::bind().await;
    let owner = Owner::at(address.to_string()).await;
    let (run, agent, chat, vm_node, native_node) = (id(), id(), id(), id(), id());
    let original_agent = json!({ "id": agent, "revision": 1, "access": { "backend": "firecracker", "nodes": [vm_node] } });
    owner.put("agents", original_agent.clone()).await;
    let mut record = run_record(&run, RunStatus::Succeeded);
    record["isolated"] = true.into();
    record["nodeId"] = vm_node.clone().into();
    record["sessionId"] = "old-engine-session".into();
    record["resources"] = limits(1, 512, 128);
    record["snapshot"] = json!({ "agent": original_agent });
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
    owner.set_checkpoint(&run, json!({ "nodeId": vm_node, "runnerId": run, "runtimeId": "fixture", "completed": true,
        "prepared": { "backend": "firecracker", "isolated": true, "projectRoot": "/data/runs/test/workspace", "output": "/data/runs/test/output/result.md" } })).await;
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
    let mut native = owner.stored("nodes", &native_node).await.unwrap();
    native["capabilities"]["backend"] = "native".into();
    native["capabilities"]["ublk"] = true.into();
    native["nativeEnabled"] = true.into();
    native["nativeAccounts"] = json!(["fixture-owner"]);
    owner.put("nodes", native).await;
    let mut vm_record = owner.stored("nodes", &vm_node).await.unwrap();
    vm_record["capabilities"]["backend"] = "firecracker".into();
    owner.put("nodes", vm_record).await;
    seed_ext4(&owner, &record, &vm_node, &format!("http://{address}/")).await;
    tasks.push(owner.serve(listener));
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
    for (backend, destination) in [("native", &native_node), ("firecracker", &vm_node)] {
        let source_node = if backend == "native" {
            &vm_node
        } else {
            &native_node
        };
        partial_cold_write(&owner, source_node, &run).await;
        owner
            .service
            .store
            .patch_run(&run, json!({ "status": RunStatus::Running }))
            .await
            .unwrap();
        owner.service.store.set("native-authority", json!({ "observedAt": now(), "accounts": { "fixture-owner": { "email": "fixture@example.test" } } }), None).await.unwrap();
        let mut profile = original_agent.clone();
        profile["access"]["backend"] = backend.into();
        profile["access"]["nodes"] = json!([destination]);
        owner.put("agents", profile).await;
        let source_context = LazyDisk::context(
            &owner
                .root()
                .join(source_node)
                .join("disks")
                .join(&run)
                .join("lazy"),
        )
        .unwrap();
        let credential = source_context["grant"].as_str().unwrap();
        assert!(
            disk_grants::authorize(&owner.service, credential)
                .await
                .is_err(),
            "normal source grants must not bypass the new profile"
        );
        let path = format!("/api/nodes/placement/{run}/backend-migration");
        let (status, preview) = owner.get(&path).await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["required"], true, "{preview}");
        assert!(
            preview["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["id"] == *destination),
            "{preview}"
        );
        let (denied, _) = owner
            .send(
                "POST",
                &path,
                json!({ "nodeId": destination, "backend": backend, "confirm": false }),
            )
            .await;
        assert_eq!(denied, StatusCode::BAD_REQUEST);
        let wrong_node = if backend == "native" {
            &vm_node
        } else {
            &native_node
        };
        let (denied, _) = owner
            .send(
                "POST",
                &path,
                json!({ "nodeId": wrong_node, "backend": backend, "confirm": true }),
            )
            .await;
        assert_eq!(denied, StatusCode::FORBIDDEN);
        let (status, response) = owner
            .send(
                "POST",
                &path,
                json!({ "nodeId": destination, "backend": backend, "confirm": true }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let settled = wait_migration(&owner, &run).await;
        assert!(settled["backendMigrationError"].is_null(), "{settled}");
        assert_eq!(settled["nodeId"], *destination, "{settled}");
        assert!(settled["sessionId"].is_null());
        assert_eq!(settled["snapshot"]["agent"]["access"]["backend"], backend);
        let saved_chat = owner.stored("chats", &chat).await.unwrap();
        assert_eq!(saved_chat["runId"], run);
        assert_eq!(saved_chat["title"], "same conversation");
        assert_eq!(saved_chat["paused"], false);
        assert_eq!(saved_chat["sessionRestartRequested"], true);
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
        assert!(
            disk_grants::authorize(&owner.service, credential)
                .await
                .is_err(),
            "source exception must end at commit"
        );
        assert_files(&owner, destination, &run, backend == "native").await;
    }
    let before = owner.run(&run).await;
    let mut profile = original_agent;
    profile["access"]["backend"] = "native".into();
    profile["access"]["nodes"] = json!([native_node]);
    owner.put("agents", profile).await;
    faults.failed.store(true, Ordering::SeqCst);
    let (status, response) = owner
        .send(
            "POST",
            &format!("/api/nodes/placement/{run}/backend-migration"),
            json!({ "nodeId": native_node, "backend": "native", "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let failed = wait_migration(&owner, &run).await;
    assert!(failed["backendMigrationError"].is_string(), "{failed}");
    assert_eq!(failed["nodeId"], before["nodeId"]);
    assert_eq!(failed["backup"]["id"], before["backup"]["id"]);
    assert_files(&owner, &vm_node, &run, false).await;
    faults.failed.store(false, Ordering::SeqCst);
    assert!(
        failed["backendMigration"].is_null(),
        "a pre-conversion failure must allow repairing the source"
    );
    let marker = json!({ "id": id(), "reservationId": id(), "process": "old-manager-process", "paused": false });
    owner
        .service
        .store
        .patch_run(&run, json!({ "backendMigration": marker }))
        .await
        .unwrap();
    let (status, response) = owner
        .send(
            "POST",
            &format!("/api/nodes/placement/{run}/backend-migration"),
            json!({ "nodeId": native_node, "backend": "native", "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let retried = wait_migration(&owner, &run).await;
    assert!(retried["backendMigrationError"].is_null(), "{retried}");
    assert_eq!(retried["nodeId"], native_node);
    assert_eq!(owner.stored("chats", &chat).await.unwrap()["paused"], false);
    let checkpoint_before = owner
        .service
        .store
        .kv(&format!("run-checkpoint:{run}"))
        .await
        .unwrap()
        .unwrap();
    let saved_point = retried["backup"]["id"].clone();
    let reservation = id();
    let mut pending = json!({ "id": id(), "reservationId": reservation, "process": "old-manager-process", "paused": false,
        "nodeId": native_node, "runtimeId": "fixture", "snapshot": retried["snapshot"], "convertedBackup": saved_point });
    pending["grant"] = json!({ "nativePrincipals": retried["nativePrincipals"], "nativeAuthorityNodes": retried["nativeAuthorityNodes"], "nativeProvenanceUnknown": false });
    owner.service.store.patch_run(&run, json!({ "snapshot": before["snapshot"], "nodeId": before["nodeId"], "backup": before["backup"], "backendMigration": pending })).await.unwrap();
    let mut source_checkpoint = checkpoint_before;
    source_checkpoint["nodeId"] = vm_node.clone().into();
    source_checkpoint["prepared"]["backend"] = "firecracker".into();
    owner.set_checkpoint(&run, source_checkpoint).await;
    let mut candidate = retried.clone();
    candidate["placementTransition"] = true.into();
    candidate["targetNodeId"] = native_node.clone().into();
    candidate["requiredRuntime"] = "fixture".into();
    placement::reserve(&owner.service, &candidate, &reservation)
        .await
        .unwrap();
    placement::materialize(&owner.service, &reservation)
        .await
        .unwrap();
    assert_ne!(
        owner.stored("node-attempts", &reservation).await.unwrap()["released"],
        true
    );
    let (status, response) = owner
        .send(
            "POST",
            &format!("/api/nodes/placement/{run}/backend-migration"),
            json!({ "nodeId": native_node, "backend": "native", "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let recovered = wait_migration(&owner, &run).await;
    assert!(recovered["backendMigrationError"].is_null(), "{recovered}");
    assert_eq!(recovered["backup"]["id"], saved_point);
    assert_eq!(
        owner.stored("node-attempts", &reservation).await.unwrap()["released"],
        true
    );
    assert_files(&owner, &native_node, &run, true).await;
    // A permissive remote destination must not spend the manager's reserve.
    let mut vm_profile = owner.stored("agents", &agent).await.unwrap();
    vm_profile["access"]["backend"] = "firecracker".into();
    vm_profile["access"]["nodes"] = json!([vm_node]);
    owner.put("agents", vm_profile).await;
    owner
        .put(
            "nodes",
            json!({ "id": LOCAL_NODE_ID, "storage": { "reserveMiB": 16 * 1024 * 1024 } }),
        )
        .await;
    let (status, response) = owner
        .send(
            "POST",
            &format!("/api/nodes/placement/{run}/backend-migration"),
            json!({ "nodeId": vm_node, "backend": "firecracker", "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let refused = wait_migration(&owner, &run).await;
    assert!(
        refused["backendMigrationError"]
            .as_str()
            .unwrap()
            .contains("temporary disk space"),
        "{refused}"
    );
    assert_eq!(refused["nodeId"], native_node);
    assert_eq!(refused["snapshot"]["agent"]["access"]["backend"], "native");
    assert!(refused["backendMigration"].is_null());
    let mut native_profile = owner.stored("agents", &agent).await.unwrap();
    native_profile["access"]["backend"] = "native".into();
    native_profile["access"]["nodes"] = json!([native_node]);
    owner.put("agents", native_profile).await;
    assert_files(&owner, &native_node, &run, true).await;
    stop.cancel();
    for task in tasks {
        task.abort();
    }
}
