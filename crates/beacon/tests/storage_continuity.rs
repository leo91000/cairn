mod common;
#[path = "../../installation/tests/common/node_fixture.rs"]
mod node_fixture;

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use cairn_installation::{
    config::id,
    nodes::{LOCAL_NODE_ID, disk_grants, publication, shared_blocks},
    object_storage::Storage,
};
use common::RelayedInstallation;
use node_fixture::{DbNodeFixture, direct_snapshot_upload};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[tokio::test]
#[ignore = "requires the installer container's real Garage and external HTTPS S3 fixtures"]
async fn existing_disks_keep_publishing_reading_and_purging_on_their_original_storage() {
    let bytes = Arc::new(Mutex::new(vec![41u8; 4096]));
    let source = bytes.clone();
    let controller = Router::new().fallback(move |request: Request<Body>| {
        let source = source.clone();
        async move {
            if request.method() == "DELETE" && request.uri().path().ends_with("/direct-reader") {
                return node_fixture::direct_reader_closed(request.uri().path());
            }

            if request.method() == "DELETE" {
                return Json(json!({})).into_response();
            }
            if request.uri().path().ends_with("/snapshot") {
                let data = source.lock().unwrap().clone();
                return Json(json!({
                    "id": id(),
                    "manifest": {
                        "version": 1,
                        "size": data.len(),
                        "blockSize": 4_194_304,
                        "blocks": [{
                            "offset": 0,
                            "size": data.len(),
                            "hash": cairn_installation::storage::digest::block(&data)
                        }]
                    }
                }))
                .into_response();
            }
            if request.method() == "POST" && request.uri().path().ends_with("/publication") {
                let body = to_bytes(request.into_body(), 4096).await.unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                let data = source.lock().unwrap().clone();
                assert_eq!(
                    request["hashes"],
                    json!([cairn_installation::storage::digest::block(&data)])
                );
                return data.into_response();
            }

            if request.method() == "POST" && request.uri().path().ends_with("/direct-reader") {
                return node_fixture::direct_reader_response(request.uri().path());
            }
            if request.method() == "POST" && request.uri().path().ends_with("/direct-upload") {
                let body = to_bytes(request.into_body(), 131_072).await.unwrap();
                let data = source.lock().unwrap().clone();
                let root = tempfile::tempdir().unwrap();
                let disk = root.path().join("source");
                tokio::fs::write(&disk, &data).await.unwrap();
                let manifest = cairn_installation::nodes::snapshots::index(&disk)
                    .await
                    .unwrap();
                return direct_snapshot_upload(&disk, manifest, &body, false).await;
            }
            StatusCode::METHOD_NOT_ALLOWED.into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let runner_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, controller).await.unwrap() });
    let fixture = RelayedInstallation::with_runner_url(Router::new(), runner_url).await;
    let service = &fixture.installation;
    let integrated: Value = serde_json::from_slice(
        &std::fs::read(std::env::var("CAIRN_INSTALLER_TEST_STORAGE_CONFIG").unwrap()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        service.config.data_dir.join("storage-s3.json"),
        integrated.to_string(),
    )
    .unwrap();
    let original_storage = Storage::configured(service).unwrap();
    let run_id = id();
    let agent_id = id();
    let record = json!({
        "id": run_id,
        "taskId": run_id,
        "createdAt": 0,
        "status": "succeeded",
        "nodeId": LOCAL_NODE_ID
    });
    service
        .store
        .write(move |db| {
            db.put_node_fixture(&json!({"id": LOCAL_NODE_ID, "local": true}))?;
            let settings = serde_json::from_value(json!({
                "name": "Storage continuity fixture",
                "access": {"nodes": [LOCAL_NODE_ID], "mcpTools": {}},
                "exclusiveNodes": [], "plugins": []
            }))?;
            let agent = cairn_installation::agents::new_agent(
                settings,
                Some(serde_json::from_value(json!(agent_id))?),
            );
            db.save_agent(&agent)?;
            let snapshot = cairn_contracts::RunSnapshot {
                task: serde_json::from_value(json!({
                    "id": record["id"],
                    "name": "Storage continuity fixture",
                    "prompt": "Verify retained storage",
                    "agentId": agent_id
                }))?,
                agent,
                project: None,
                projects: vec![],
                available_projects: vec![],
                skills: vec![],
                delegation: None,
            };
            db.admit_run(
                &serde_json::from_value(record)?,
                Some(&snapshot),
                &json!({"nodeId": LOCAL_NODE_ID}),
                None,
            )
        })
        .await
        .unwrap();
    service
        .store
        .save_checkpoint_boundary(
            &run_id,
            json!({ "nodeId": LOCAL_NODE_ID, "runnerId": id() }),
        )
        .await
        .unwrap();

    // The existing public publication seam drives actual encrypted S3 uploads.
    let first = publication::capture(service, &service.store.run(&run_id).await.unwrap())
        .await
        .unwrap();
    // A mounted disk retains its previous base until its node acknowledges a new one.
    let first_point = service
        .get("node-backups", first["id"].as_str().unwrap())
        .await
        .unwrap();
    disk_grants::issue(
        service,
        &service.store.run(&run_id).await.unwrap(),
        LOCAL_NODE_ID,
        &first_point,
    )
    .await
    .unwrap();
    let response = fixture
        .app
        .client
        .put(format!("{}/settings/storage", fixture.base))
        .header("cookie", &fixture.cookie)
        .header("origin", &fixture.app.url)
        .header("x-csrf-token", fixture.session["csrf"].as_str().unwrap())
        .json(&json!({
            "bucket": "cairn-external",
            "endpoint": std::env::var("CAIRN_INSTALLER_TEST_EXTERNAL_ENDPOINT").unwrap(),
            "region": "us-east-1",
            "accessKeyId": "external-fixture",
            "secretAccessKey": "external-fixture-secret"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());

    *bytes.lock().unwrap() = vec![42u8; 4096];
    let second = publication::capture(service, &service.store.run(&run_id).await.unwrap())
        .await
        .unwrap();
    let response = fixture
        .get(&format!("/nodes/backups/{run_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let points: Vec<Value> = response.json().await.unwrap();
    assert!(points.iter().any(|point| point["id"] == second["id"]));
    assert!(
        points
            .iter()
            .all(|point| point["endpoint"] == "http://garage:3900")
    );

    // Evict local cached bytes so reads must use each point's original S3 credentials.
    std::fs::remove_dir_all(
        service
            .config
            .data_dir
            .join("node-backups")
            .join(&run_id)
            .join("blocks"),
    )
    .unwrap();
    let mut objects = Vec::new();
    for (point_id, expected) in [(&first["id"], 41u8), (&second["id"], 42u8)] {
        let point = service
            .get("node-backups", point_id.as_str().unwrap())
            .await
            .unwrap();
        objects.push(format!(
            "node-backups/{run_id}/{}.json",
            point_id.as_str().unwrap()
        ));
        let manifest = publication::manifest(service, &point).await.unwrap();
        let block = &manifest["blocks"][0];
        let hash = block["hash"].as_str().unwrap();
        assert_eq!(
            publication::read_block(service, &point, hash)
                .await
                .unwrap(),
            vec![expected; 4096]
        );
        objects.push(shared_blocks::object_key(
            3,
            hash,
            block["object"].as_str().unwrap(),
        ));
    }
    publication::purge(service, &run_id).await.unwrap();
    // Advance both the GC grace period and the issued PUT capabilities' expiry;
    // production purge must retain objects while a delayed PUT could recreate them.
    service
        .store
        .write(|db| {
            db.0.execute(
                "UPDATE shared_objects SET unused_at=0 WHERE unused_at IS NOT NULL",
                [],
            )?;
            db.0.execute("UPDATE direct_storage_grants SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    while shared_blocks::collect(service).await.unwrap() > 0 {}
    for object in objects {
        assert_eq!(
            original_storage
                .download_bytes(&object, 65536)
                .await
                .unwrap_err()
                .status,
            409
        );
    }
    let response = fixture
        .get(&format!("/nodes/backups/{run_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.json::<Value>().await.unwrap(), json!([]));
    fixture.close().await;
    server.abort();
}
