use super::{
    diagnostic_tests::{peer, service_with_runner},
    *,
};
use crate::{
    node_fixture::StoreNodeFixture, object_storage::tests::fixture_storage_at, storage::digest,
};
use axum::{Json, Router, body::to_bytes, extract::Request, response::IntoResponse};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn lost_ack_is_fenced_before_purge_or_expiry_releases_its_dependencies() {
    use std::sync::atomic::{AtomicBool, Ordering};

    for permanently_deleted in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let run = id();
        let publication = id();
        let object = id();
        let chat = json!({ "id": id(), "runId": run });
        let fail_fence = Arc::new(AtomicBool::new(true));
        let state = root.path().join("node");
        let controller = Router::new().fallback({
            let (fail_fence, state) = (fail_fence.clone(), state.clone());
            move |request: Request| {
                let (fail_fence, state) = (fail_fence.clone(), state.clone());
                async move {
                    if request.uri().path().ends_with("/delete") {
                        return Json(json!({ "deleted": true })).into_response();
                    }
                    assert!(request.uri().path().ends_with("/publication-fence"));
                    if fail_fence.load(Ordering::Relaxed) {
                        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let path = request.uri().path().to_owned();
                    let run = path.split('/').nth(2).unwrap();
                    let body = to_bytes(request.into_body(), 4096).await.unwrap();
                    let input: Value = serde_json::from_slice(&body).unwrap();
                    let publication = input["backupId"].as_str().unwrap();
                    crate::storage::runtime::fence_publication(&state, run, publication)
                        .await
                        .unwrap();
                    Json(json!({ "fenced": publication })).into_response()
                }
            }
        });
        let (controller, controller_task) = peer(controller).await;
        let s = service_with_runner(root.path(), controller).await;
        let (saved_run, saved_publication, saved_object, saved_chat) = (
            run.clone(),
            publication.clone(),
            object.clone(),
            chat.clone(),
        );
        s.store.transaction(move |db| {
            db.add_run_fixture(&json!({
                "id": saved_run, "taskId": id(), "status": "succeeded", "createdAt": 0
            }), None)?;
            db.put("chats", &saved_chat)?;
            db.save_checkpoint_boundary(&saved_run, &json!({"nodeId": super::super::LOCAL_NODE_ID}))?;
            db.0.execute("INSERT INTO shared_publications VALUES(?1,?2)", rusqlite::params![saved_publication, saved_run])?;
            db.0.execute("INSERT INTO shared_objects(id,destination,hash,size,location,state) VALUES(?1,'fixture',?2,4096,'{}','ready')", rusqlite::params![saved_object, digest::block(&[4;4096])])?;
            db.0.execute("INSERT INTO shared_references VALUES(?1,?2)", rusqlite::params![saved_publication, saved_object])?;
            db.0.execute("INSERT INTO direct_journal_acknowledgements(publication,run_id,node_id) VALUES(?1,?2,?3)", rusqlite::params![saved_publication, saved_run, super::super::LOCAL_NODE_ID])?;
            Ok(())
        }).await.unwrap();

        let retire = || async {
            if permanently_deleted {
                crate::conversation_deletion::purge(&s, chat.clone()).await
            } else {
                crate::conversation_deletion::expire_environment(&s, chat.clone()).await
            }
        };
        // Even a successful disk DELETE does not prove that a detached ACK has
        // finished. Failed fencing must preserve both the run and its pins.
        assert!(retire().await.is_err());
        assert!(s.store.run(&run).await.is_ok());
        s.store
            .read(|db| {
                assert_eq!(
                    db.0.query_row("SELECT count(*) FROM shared_references", [], |r| r
                        .get::<_, i64>(0))?,
                    1
                );
                assert_eq!(
                    db.0.query_row(
                        "SELECT count(*) FROM direct_journal_acknowledgements",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();

        fail_fence.store(false, Ordering::Relaxed);
        retire().await.unwrap();
        let fences: Value = serde_json::from_slice(
            &std::fs::read(state.join("publication-fences/direct-read-failures.json")).unwrap(),
        )
        .unwrap();
        assert!(
            fences["rejected"]
                .as_array()
                .unwrap()
                .contains(&json!(publication))
        );
        s.store
            .read(|db| {
                assert_eq!(
                    db.0.query_row(
                        "SELECT count(*) FROM direct_journal_acknowledgements",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                assert_eq!(
                    db.0.query_row("SELECT count(*) FROM shared_references", [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
                assert_eq!(
                    db.0.query_row(
                        "SELECT count(*) FROM shared_objects WHERE unused_at IS NOT NULL",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
        controller_task.abort();
        if permanently_deleted {
            assert!(s.store.run(&run).await.is_err());
            continue;
        }

        assert_eq!(
            s.store.run(&run).await.unwrap()["environmentState"],
            "expired"
        );
        // The old controller has disappeared. A fresh placement can recover and
        // admit a new ACK inventory without contacting that retired controller.
        super::super::journal_acknowledgements::recover(&s, &run)
            .await
            .unwrap();
        let next = json!({"id": id(), "runId": run, "nodeId": id()});
        s.store
            .transaction(move |db| {
                db.0.execute(
                    "INSERT INTO shared_publications VALUES(?1,?2)",
                    rusqlite::params![text(&next, "id"), text(&next, "runId")],
                )?;
                super::super::journal_acknowledgements::begin(db, &next)
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn permanently_revoked_identity_cannot_pin_a_retired_environment() {
    let root = tempfile::tempdir().unwrap();
    let s = service_with_runner(root.path(), String::new()).await;
    let (run, publication, node) = (id(), id(), id());
    s.store
        .put_node_fixture(json!({"id":node,"revoked":true}))
        .await
        .unwrap();
    let saved_run = run.clone();
    s.store.transaction(move |db| {
        db.0.execute("INSERT INTO shared_publications VALUES(?1,?2)", rusqlite::params![publication, saved_run])?;
        db.0.execute("INSERT INTO direct_journal_acknowledgements(publication,run_id,node_id,retired) VALUES(?1,?2,?3,1)", rusqlite::params![publication, saved_run, node])?;
        Ok(())
    }).await.unwrap();
    expire_environment(&s, &run).await.unwrap();
    s.store
        .read(|db| {
            assert_eq!(
                db.0.query_row(
                    "SELECT count(*) FROM direct_journal_acknowledgements",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.0.query_row("SELECT count(*) FROM shared_publications", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn manager_conversion_quarantines_v3_corruption_before_other_nodes_can_acknowledge() {
    let root = tempfile::tempdir().unwrap();
    let reading_run = id();
    let publishing_run = id();
    let reading_point = id();
    let publishing_point = id();
    let object = id();
    let hash = digest::block(&[37; 4096]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = CancellationToken::new();
    let controller = Router::new().fallback({
        let (entered, release, point, run, state) = (
            entered.clone(),
            release.clone(),
            publishing_point.clone(),
            publishing_run.clone(),
            root.path().join("node"),
        );
        move |request: Request| {
            let (entered, release, point, run, state) = (
                entered.clone(),
                release.clone(),
                point.clone(),
                run.clone(),
                state.clone(),
            );
            async move {
                assert!(request.uri().path().ends_with("/publication-fence"));
                let body = to_bytes(request.into_body(), 4096).await.unwrap();
                let fence: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(fence["backupId"], point);
                entered.notify_one();
                release.cancelled().await;
                crate::storage::runtime::fence_publication(&state, &run, &point)
                    .await
                    .unwrap();
                Json(json!({ "fenced": point })).into_response()
            }
        }
    });
    let (controller, controller_task) = peer(controller).await;
    let s = service_with_runner(root.path(), controller).await;
    let provider = Router::new().fallback(|| async { "corrupt v3 envelope" });
    let (provider, provider_task) = peer(provider).await;
    let logical = "https://conversion-fixture.example";
    std::fs::write(
        s.config.data_dir.join("storage-s3.json"),
        json!({
            "bucket": "fixture",
            "endpoint": logical,
            "region": "us-east-1"
        })
        .to_string(),
    )
    .unwrap();
    fixture_storage_at(&provider, logical, "fixture", s.hot_s3.clone()).await;
    let (reader, publisher, first, second, retained, saved_hash) = (
        reading_run.clone(),
        publishing_run.clone(),
        reading_point.clone(),
        publishing_point.clone(),
        object.clone(),
        hash.clone(),
    );
    s.store.transaction(move |db| {
        for (run, point) in [(&reader, &first), (&publisher, &second)] {
            let record = json!({
                "id": run,
                "taskId": id(),
                "status": "succeeded",
                "createdAt": 0,
                "backup": { "snapshotId": "retained-baseline" }
            });
            db.add_run_fixture(&record, None)?;
            db.0.execute("INSERT INTO shared_publications VALUES(?1,?2)", rusqlite::params![point, run])?;
        }
        db.0.execute("INSERT INTO shared_objects(id,destination,hash,size,location,state) VALUES(?1,'bucket',?2,4096,'{}','ready')", rusqlite::params![retained, saved_hash])?;
        for point in [&first, &second] {
            db.0.execute("INSERT INTO shared_references VALUES(?1,?2)", rusqlite::params![point, retained])?;
        }
        db.0.execute("INSERT INTO direct_journal_acknowledgements(publication,run_id,node_id) VALUES(?1,?2,?3)", rusqlite::params![second, publisher, super::super::LOCAL_NODE_ID])?;
        Ok(())
    }).await.unwrap();
    let backup = json!({
        "id": reading_point,
        "runId": reading_run,
        "destination": "s3",
        "bucket": "fixture",
        "endpoint": logical
    });
    let block = json!({ "hash": hash, "object": object, "size": 4096, "envelope": 3 });
    // Exercise the same authenticated-block reader used by backend conversion.
    let mut reading = Box::pin(read_manifest_block(&s, &backup, &block));
    tokio::select! {
        _ = &mut reading => panic!("Corruption must wait for every ambiguous dependent ACK fence"),
        () = entered.notified() => {},
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("Missing durable publication fence"),
    }
    let observed = object.clone();
    s.store
        .read(move |db| {
            assert_eq!(
                db.0.query_row(
                    "SELECT state FROM shared_objects WHERE id=?1",
                    [&observed],
                    |r| r.get::<_, String>(0)
                )?,
                "ready"
            );
            assert_eq!(
                db.0.query_row(
                    "SELECT count(*) FROM direct_object_quarantines WHERE object=?1",
                    [observed],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    release.cancel();
    assert_eq!(reading.await.unwrap_err().status, 409);
    s.store
        .read(move |db| {
            assert_eq!(
                db.0.query_row(
                    "SELECT state FROM shared_objects WHERE id=?1",
                    [object],
                    |r| r.get::<_, String>(0)
                )?,
                "invalid"
            );
            assert_eq!(
                db.0.query_row(
                    "SELECT count(*) FROM direct_journal_acknowledgements",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            for run in [&reading_run, &publishing_run] {
                assert!(db.run(run)?.unwrap()["backup"]["snapshotId"].is_null());
            }
            Ok(())
        })
        .await
        .unwrap();
    controller_task.abort();
    provider_task.abort();
}
