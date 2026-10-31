//! Faults cross the actual grant transaction, owned node work and receipt path.
use super::*;
use crate::storage::{Disk, environment, runtime};

impl Fixture {
    async fn demand(blocks: usize) -> Self {
        let mut fixture = Self::new(blocks).await;
        let run = text(&fixture.run, "id");
        let disk_directory = fixture.root.path().join("disks").join(run);
        let base = json!({
            "version": 1,
            "size": blocks as u64 * snapshots::BLOCK,
            "blockSize": snapshots::BLOCK,
            "blocks": (0..blocks).map(|index| json!({
                "offset": index as u64 * snapshots::BLOCK,
                "size": snapshots::BLOCK,
                "hash": null
            })).collect::<Vec<_>>()
        });
        let disk = runtime::create(
            &disk_directory.join("lazy"),
            &base,
            &json!({ "master": "http://127.0.0.1:9/", "grant": "fixture" }),
        )
        .await
        .unwrap();
        for index in 0..blocks {
            disk.write_at(
                index as u64 * snapshots::BLOCK,
                &Contents::Sparse.block(index),
            )
            .unwrap();
        }
        let generation = disk.seal().unwrap();
        let mut manifest = disk.capture(generation).unwrap();
        manifest["onDemand"] = true.into();
        manifest["generation"] = generation.into();
        manifest["capturedAt"] = now().into();
        drop(disk);
        let capture = fixture
            .root
            .path()
            .join("snapshots")
            .join(&fixture.snapshot_id);
        tokio::fs::write(capture.join("run"), run).await.unwrap();
        tokio::fs::write(capture.join("manifest.json"), manifest.to_string())
            .await
            .unwrap();
        let credential = super::super::super::disk_grants::new_disk(
            &fixture.service,
            &fixture.run,
            fixture.node.as_str().unwrap(),
        )
        .await
        .unwrap();
        fixture.snapshot["manifest"] = manifest;
        fixture.snapshot["grantId"] = crate::auth::digest(&credential).into();
        *fixture.capture_reply.lock().unwrap() = fixture.snapshot.clone();
        fixture
    }

    async fn wait_for_drain_or_return(&self, returned: &AtomicBool) {
        // Use the existing stage only to schedule gate release, never as a
        // latency assertion or a substitute for the durable/ownership checks.
        loop {
            if returned.load(Ordering::SeqCst) {
                return;
            }
            let draining = self
                .manager_metrics
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|metrics| {
                    metrics.snapshot()["stages"]["uploadDrain"]["inFlight"] == 1
                });
            if draining {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn released_journal_is_unchanged(&self) {
        let run = text(&self.run, "id");
        let lease = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match environment::lock(self.root.path(), run, "fixture reader retained").await {
                    Ok(lease) => break lease,
                    Err(error) if error.status == 409 => tokio::task::yield_now().await,
                    Err(error) => panic!("Unexpected physical release error: {}", error.message),
                }
            }
        })
        .await
        .expect("every accepted transfer must release the physical journal after drain");
        let volume = runtime::load(&lease.directory).await.unwrap();
        let mut bytes = [0; 4096];
        volume.disk.read_at(0, &mut bytes).unwrap();
        assert_eq!(bytes.as_slice(), &Contents::Sparse.block(0)[..4096]);
        let status = volume.disk.accounting().unwrap();
        assert!(status["dirtyBytes"].as_u64().unwrap() > 0);
        assert!(status["published"].is_null());
    }

    async fn grant_counts(&self) -> (i64, i64) {
        self.service
            .store
            .read(|db| {
                Ok(db.0.query_row(
                    "SELECT count(*),sum(settled),min(expires_at) FROM direct_storage_grants",
                    [],
                    |row| {
                        let count: i64 = row.get(0)?;
                        let settled: i64 = row.get(1)?;
                        let expires: i64 = row.get(2)?;
                        assert!(expires > now());
                        Ok((count, settled))
                    },
                )?)
            })
            .await
            .unwrap()
    }
}

#[derive(Clone, Copy)]
enum Revocation {
    Grant,
    Node,
    Checkpoint,
}

#[tokio::test]
async fn candidate_revalidates_before_refill_and_preserves_admitted_work() {
    isolated("nodes::publication::pipeline_tests::safety::revocation_child").await;
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn revocation_child() {
    for revoke in [Revocation::Grant, Revocation::Node, Revocation::Checkpoint] {
        let fixture = Fixture::demand(17).await;
        *fixture.effects.hold_reply.lock().unwrap() = Some(2);
        let returned = AtomicBool::new(false);
        let (result, (returned_early, physical_held)) =
            tokio::time::timeout(Duration::from_secs(20), async {
                tokio::join!(
                    async {
                        let result = fixture.transfer_measured(Some(2)).await;
                        returned.store(true, Ordering::SeqCst);
                        result
                    },
                    async {
                        fixture
                            .effects
                            .reply_ready
                            .acquire()
                            .await
                            .unwrap()
                            .forget();
                        match revoke {
                            Revocation::Grant => {
                                let grant = text(&fixture.snapshot, "grantId").to_owned();
                                fixture
                                    .service
                                    .store
                                    .write(move |db| db.remove("node-disk-grants", &grant))
                                    .await
                                    .unwrap();
                            }
                            Revocation::Node => {
                                fixture
                                    .service
                                    .store
                                    .put_node_fixture(
                                        json!({ "id": fixture.node, "revoked": true }),
                                    )
                                    .await
                                    .unwrap();
                            }
                            Revocation::Checkpoint => {
                                fixture
                                    .service
                                    .store
                                    .save_checkpoint_boundary(
                                        text(&fixture.run, "id"),
                                        json!({ "nodeId": id(), "runnerId": fixture.attempt }),
                                    )
                                    .await
                                    .unwrap();
                            }
                        }
                        fixture.effects.release_reply.add_permits(1);
                        fixture.wait_for_drain_or_return(&returned).await;
                        let early = returned.load(Ordering::SeqCst);
                        let held = environment::lock(
                            fixture.root.path(),
                            text(&fixture.run, "id"),
                            "fixture",
                        )
                        .await
                        .is_err();
                        fixture.object_server.release.add_permits(1);
                        (early, held)
                    }
                )
            })
            .await
            .expect("revocation fixture must drain every admitted request");
        assert!(result.is_err());
        assert!(
            !returned_early,
            "authorization failure released the operation before its admitted PUT drained"
        );
        assert!(physical_held);
        assert_eq!(fixture.effects.posts.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.object_server.started.load(Ordering::SeqCst), 8);
        assert_eq!(fixture.object_server.in_flight.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.grant_counts().await, (8, 8));
        fixture.released_journal_is_unchanged().await;
    }
}

#[tokio::test]
async fn candidate_rejects_invalid_receipts_without_releasing_other_batches() {
    isolated("nodes::publication::pipeline_tests::safety::receipt_faults_child").await;
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn receipt_faults_child() {
    for fault in [
        ReceiptFault::WrongPublication,
        ReceiptFault::MissingObject,
        ReceiptFault::DuplicateObject,
        ReceiptFault::WrongPlaintext,
        ReceiptFault::OversizedEncoding,
        ReceiptFault::MissingFields,
        ReceiptFault::LostResponse,
    ] {
        let fixture = Fixture::demand(9).await;
        *fixture.effects.fault.lock().unwrap() = Some((2, fault));
        let returned = AtomicBool::new(false);
        let (result, returned_early) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(
                async {
                    let result = fixture.transfer_measured(Some(2)).await;
                    returned.store(true, Ordering::SeqCst);
                    result
                },
                async {
                    fixture.wait_for_drain_or_return(&returned).await;
                    let early = returned.load(Ordering::SeqCst);
                    fixture.object_server.release.add_permits(1);
                    early
                }
            )
        })
        .await
        .expect("receipt errors must drain accepted node work");
        assert!(result.is_err());
        assert!(!returned_early);
        assert_eq!(fixture.effects.posts.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.object_server.in_flight.load(Ordering::SeqCst), 0);
        assert_eq!(
            fixture.grant_counts().await,
            (8, 4),
            "invalid or ambiguous receipts must not settle capabilities"
        );
        fixture.released_journal_is_unchanged().await;
    }
}

#[tokio::test]
async fn candidate_replays_ambiguous_writes_with_authenticated_create_only_objects() {
    isolated("nodes::publication::pipeline_tests::safety::replay_child").await;
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn replay_child() {
    let fixture = Fixture::new(9).await;
    *fixture.object_server.blocked_object.lock().unwrap() = None;
    *fixture.effects.fault.lock().unwrap() = Some((2, ReceiptFault::LostResponse));
    assert!(fixture.transfer_measured(Some(2)).await.is_err());
    assert_eq!(fixture.grant_counts().await, (8, 4));
    let original = fixture.object_server.bodies.lock().await.clone();
    *fixture.effects.fault.lock().unwrap() = None;
    assert_eq!(
        fixture.transfer_measured(Some(2)).await.unwrap().0,
        9 * snapshots::BLOCK
    );
    assert_eq!(
        fixture.object_server.preconditions.load(Ordering::SeqCst),
        8
    );
    assert_eq!(fixture.grant_counts().await, (9, 9));
    let bodies = fixture.object_server.bodies.lock().await;
    for (path, bytes) in original {
        assert_eq!(
            bodies[&path], bytes,
            "412 must retain the authenticated original ciphertext"
        );
    }
    drop(bodies);
    // The same path with different ciphertext must fail its authenticated GET;
    // a create-only precondition response is not itself a successful receipt.
    let hash = &fixture.objects[4].hash;
    let mut bodies = fixture.object_server.bodies.lock().await;
    let corrupt = bodies
        .iter_mut()
        .find(|(path, _)| path.split('/').any(|part| part == hash))
        .unwrap()
        .1;
    corrupt[0] ^= 1;
    drop(bodies);
    assert!(fixture.transfer_measured(Some(2)).await.is_err());
    assert_eq!(fixture.object_server.in_flight.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.effects.active.load(Ordering::SeqCst), 0);
    assert!(!fixture.effects.terminal_overlap.load(Ordering::SeqCst));
}

#[tokio::test]
async fn candidate_drains_puts_after_reader_loss_or_object_rejection() {
    isolated("nodes::publication::pipeline_tests::safety::transport_faults_child").await;
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn transport_faults_child() {
    for reader_loss in [true, false] {
        let fixture = Fixture::demand(17).await;
        if reader_loss {
            *fixture.effects.hold_reply.lock().unwrap() = Some(2);
        } else {
            *fixture.object_server.fail_index.lock().unwrap() = Some(4);
        }
        let returned = AtomicBool::new(false);
        let (result, (early, held)) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(
                async {
                    let result = fixture.transfer_measured(Some(2)).await;
                    returned.store(true, Ordering::SeqCst);
                    result
                },
                async {
                    if reader_loss {
                        fixture
                            .effects
                            .reply_ready
                            .acquire()
                            .await
                            .unwrap()
                            .forget();
                        fixture.effects.reader_disconnect.cancel();
                        fixture.effects.release_reply.add_permits(1);
                    }
                    fixture.wait_for_drain_or_return(&returned).await;
                    let early = returned.load(Ordering::SeqCst);
                    let held =
                        environment::lock(fixture.root.path(), text(&fixture.run, "id"), "fixture")
                            .await
                            .is_err();
                    fixture.object_server.release.add_permits(1);
                    (early, held)
                }
            )
        })
        .await
        .expect("transport fault must drain admitted work");
        assert!(result.is_err());
        assert!(!early);
        assert!(held);
        assert!(fixture.effects.posts.load(Ordering::SeqCst) <= 3);
        assert_eq!(fixture.object_server.in_flight.load(Ordering::SeqCst), 0);
        fixture.released_journal_is_unchanged().await;
    }
}

#[tokio::test]
async fn two_publications_share_the_unchanged_global_put_budget() {
    isolated("nodes::publication::pipeline_tests::safety::fairness_child").await;
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn fairness_child() {
    let first = Fixture::new(17).await;
    let second = Fixture::new(9).await;
    let budget = Arc::new(SharedPutBudget::default());
    *first.object_server.shared_budget.lock().unwrap() = Some(budget.clone());
    *second.object_server.shared_budget.lock().unwrap() = Some(budget.clone());
    *second.object_server.blocked_object.lock().unwrap() = None;
    let (first_result, second_bytes) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(first.transfer_measured(Some(2)), async {
            first
                .object_server
                .completed
                .acquire_many(3)
                .await
                .unwrap()
                .forget();
            let bytes = second.transfer_measured(Some(2)).await.unwrap().0;
            assert_eq!(first.object_server.in_flight.load(Ordering::SeqCst), 1);
            first.object_server.release.add_permits(1);
            bytes
        })
    })
    .await
    .expect("one straggler must not starve another publication");
    assert_eq!(first_result.unwrap().0, 17 * snapshots::BLOCK);
    assert_eq!(second_bytes, 9 * snapshots::BLOCK);
    assert!(budget.maximum.load(Ordering::SeqCst) <= direct::BATCH);
    assert_eq!(budget.active.load(Ordering::SeqCst), 0);
    assert!(first.effects.maximum.load(Ordering::SeqCst) <= 2);
    assert!(second.effects.maximum.load(Ordering::SeqCst) <= 2);
    assert!(!first.effects.terminal_overlap.load(Ordering::SeqCst));
    assert!(!second.effects.terminal_overlap.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancelled_capture_caller_keeps_the_operation_until_true_publication_completion() {
    isolated("nodes::publication::pipeline_tests::safety::capture_cancellation_child").await;
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn capture_cancellation_child() {
    let fixture = Fixture::new(9).await;
    *fixture.object_server.blocked_object.lock().unwrap() = Some(fixture.objects[0].hash.clone());
    let mut service = (*fixture.service).clone();
    service.config.runner_url = fixture.controller.clone();
    tokio::fs::write(service.config.data_dir.join("runner-secret"), "fixture")
        .await
        .unwrap();
    tokio::fs::write(service.config.data_dir.join("storage-s3.json"), json!({
        "bucket": "pipeline-fixture", "region": "us-east-1", "endpoint": fixture.storage.endpoint
    }).to_string()).await.unwrap();
    let service = Arc::new(service);
    let run_id = text(&fixture.run, "id").to_owned();
    let caller = {
        let service = service.clone();
        let run = fixture.run.clone();
        tokio::spawn(async move { super::super::capture(&service, &run).await })
    };
    tokio::time::timeout(
        Duration::from_secs(20),
        fixture.object_server.completed.acquire_many(3),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            service.node_backup_operation.lock(&run_id)
        )
        .await
        .is_err(),
        "cancelling an HTTP caller cannot release the run publication lock while its PUT remains live"
    );
    assert_ne!(
        service.store.run(&run_id).await.unwrap()["backup"]["status"],
        "ready"
    );
    fixture.object_server.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let run = service.store.run(&run_id).await.unwrap();
            if run["backup"]["status"] == "error" {
                panic!("owned capture failed: {}", run["backup"]["error"]);
            }
            if run["backup"]["status"] == "ready" {
                let publication = text(&run["backup"], "id").to_owned();
                service
                    .store
                    .read(move |db| shared_blocks::verified(db, &publication))
                    .await
                    .unwrap();
                break;
            }
            tokio::task::yield_now().await;
        }
        let _released = service.node_backup_operation.lock(&run_id).await;
    })
    .await
    .expect("the detached owned capture must finish and release its operation");
    assert_eq!(fixture.object_server.started.load(Ordering::SeqCst), 9);
    assert_eq!(fixture.object_server.in_flight.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.effects.active.load(Ordering::SeqCst), 0);
    assert!(!fixture.effects.terminal_overlap.load(Ordering::SeqCst));
    assert_eq!(fixture.grant_counts().await, (9, 9));
    let directory = fixture
        .root
        .path()
        .join("snapshots")
        .join(&fixture.snapshot_id);
    assert!(
        !directory.exists(),
        "publication must close its reader before successful snapshot discard"
    );
}

#[tokio::test]
async fn ambiguous_control_failure_retains_physical_ownership_and_unsettled_grants() {
    isolated("nodes::publication::pipeline_tests::safety::early_response_child").await;
}

#[tokio::test]
#[ignore = "isolated process-global PUT budget child"]
async fn early_response_child() {
    let fixture = Fixture::demand(9).await;
    fixture.effects.early_response.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_secs(20), fixture.transfer_measured(Some(2)))
        .await
        .expect("an interrupted control reply must report ambiguity");
    assert!(result.is_err());
    assert_eq!(fixture.object_server.in_flight.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.grant_counts().await, (8, 4));
    let physical = environment::lock(fixture.root.path(), text(&fixture.run, "id"), "fixture")
        .await
        .err()
        .unwrap();
    assert_eq!(physical.status, 409);
    let capture = fixture
        .root
        .path()
        .join("snapshots")
        .join(&fixture.snapshot_id);
    assert!(
        snapshots::discard(&capture).await.is_err(),
        "a superseding capture cannot discard a still-owned reader"
    );
    assert!(capture.join("manifest.json").is_file());
    assert_ne!(
        fixture
            .service
            .store
            .run(text(&fixture.run, "id"))
            .await
            .unwrap()["backup"]["status"],
        "ready"
    );
    fixture.object_server.release.add_permits(1);
    fixture.released_journal_is_unchanged().await;
    assert_eq!(fixture.object_server.in_flight.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.grant_counts().await,
        (8, 4),
        "a detached PUT without receipt stays unsettled until verified replay"
    );
}
