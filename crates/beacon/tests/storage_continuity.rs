mod common;

use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode},
    response::IntoResponse,
};
use common::RelayedInstallation;
use cairn_installation::{
    config::id,
    nodes::{LOCAL_NODE_ID, disk_grants, publication, shared_blocks},
    object_storage::Storage,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

#[tokio::test]
#[ignore = "requires the installer container's real Garage and external HTTPS S3 fixtures"]
async fn existing_disks_keep_publishing_reading_and_purging_on_their_original_storage() {
    let bytes = Arc::new(Mutex::new(vec![41u8; 4096]));
    let source = bytes.clone();
    let controller = Router::new().fallback(move |request: Request<Body>| {
        let source = source.clone();
        async move {
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
                            "hash": hex::encode(Sha256::digest(&data))
                        }]
                    }
                }))
                .into_response();
            }
            if request.uri().path().ends_with("/blocks")
                || request.uri().path().ends_with("/publication")
            {
                // Exercise the supported single-block transport, without a VM.
                return StatusCode::NOT_FOUND.into_response();
            }
            source.lock().unwrap().clone().into_response()
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
    let record = json!({
        "id": run_id,
        "taskId": run_id,
        "createdAt": 0,
        "status": "succeeded",
        "nodeId": LOCAL_NODE_ID
    });
    service
        .store
        .write(move |db| db.add_run(&record, None))
        .await
        .unwrap();
    service
        .store
        .set(
            &format!("run-checkpoint:{run_id}"),
            json!({ "nodeId": LOCAL_NODE_ID, "runnerId": id() }),
            None,
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
        objects.push(shared_blocks::key(hash, block["object"].as_str().unwrap()));
    }
    publication::purge(service, &run_id).await.unwrap();
    // Advance the existing GC grace period; production purge is asynchronous.
    service
        .store
        .write(|db| {
            db.0.execute(
                "UPDATE shared_objects SET unused_at=0 WHERE unused_at IS NOT NULL",
                [],
            )?;
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
