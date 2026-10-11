mod common;

use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_installation::{
    config::{Config, id},
    nodes::{publication, shared_blocks, snapshots},
    object_storage::Storage,
    service::Service,
    storage::{block_codec::MAX_ENCODED_BYTES, digest},
    store::Store,
    vault::Vault,
};
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};

const COMPLETE: &str = "migration:disk-blocks-v2";

fn config(root: &Path) -> Config {
    let config = common::config(root);
    common::managed_codex_home(&config.home);
    config
}

fn binary(vault: &Vault, scope: &str, bytes: &[u8]) -> Vec<u8> {
    let mut encoded = b"CAIRNB\x01\0".to_vec();
    encoded.extend(vault.encrypt_bytes(scope, bytes).unwrap());
    encoded
}

fn point(
    vault: &Vault,
    run: &str,
    hash: &str,
    size: usize,
    object: Option<&str>,
    s3: bool,
) -> Value {
    let point = id();
    let mut block = json!({ "offset": 0, "size": size, "hash": hash });
    if let Some(object) = object {
        block["object"] = object.into();
    }
    let manifest = json!({
        "version": 1,
        "size": size,
        "blockSize": snapshots::BLOCK,
        "blocks": [block]
    });
    let mut value = json!({
        "id": point,
        "runId": run,
        "destination": if s3 { "s3" } else { "master" },
        "bucket": "cairn-node-test",
        "endpoint": null,
        "manifest": vault.encrypt(&format!("backup:{point}"), &manifest).unwrap()
    });
    if object.is_some() {
        value["blockFormat"] = "shared-v1".into();
    }
    value
}

#[tokio::test]
async fn startup_migrates_binary_json_and_current_envelopes_before_returning() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let store = Store::open(&config.data_dir).unwrap();
    let vault = Vault::new(store.clone(), &config.data_dir).unwrap();
    let codec = cairn_installation::storage::block_codec::BlockCodec::default();
    let mut expected = Vec::new();
    for format in 0..3 {
        let run = id();
        let bytes = vec![37 + format; 256 * 1024];
        let hash = if format == 1 {
            hex::encode(Sha256::digest(&bytes))
        } else {
            digest::block(&bytes)
        };
        let source = format!("node-backups/{run}/blocks/{hash}");
        let target = format!("node-backups/{run}/blocks-v2/{hash}");
        let encoded = match format {
            0 => binary(&vault, &source, &bytes),
            1 => serde_json::to_vec(
                &vault
                    .encrypt(&source, &json!(STANDARD.encode(&bytes)))
                    .unwrap(),
            )
            .unwrap(),
            _ => codec
                .encode(vault.clone(), source, bytes.clone())
                .await
                .unwrap(),
        };
        let path = config
            .data_dir
            .join("node-backups")
            .join(&run)
            .join("blocks")
            .join(&hash);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, encoded).unwrap();
        let point = point(&vault, &run, &hash, bytes.len(), None, false);
        store.put("node-backups", point.clone()).await.unwrap();
        expected.push((point, path, target, hash, bytes));
    }
    store
        .transaction(|db| {
            db.0.execute_batch("PRAGMA user_version=5")?;
            Ok(())
        })
        .await
        .unwrap();
    let s = Service::new(config.clone()).await.unwrap();
    assert_eq!(
        s.store.kv(COMPLETE).await.unwrap().unwrap()["complete"],
        true
    );
    for (point, path, scope, hash, bytes) in expected {
        let encoded = std::fs::read(&path).unwrap();
        assert!(encoded.starts_with(b"CAIRNB\x02\0"));
        assert!(encoded.len() < bytes.len() / 100);
        assert_eq!(
            cairn_installation::storage::block_codec::BlockCodec::default()
                .decode(
                    s.vault.clone(),
                    scope,
                    hash.clone(),
                    Some(bytes.len() as u64),
                    encoded
                )
                .await
                .unwrap(),
            bytes
        );
        assert_eq!(
            publication::read_block(&s, &point, &hash).await.unwrap(),
            bytes
        );
    }
    // Completed startup is idempotent and needs neither a legacy scan nor S3 access.
    let summary = s.store.kv(COMPLETE).await.unwrap();
    let restarted = Service::new(config).await.unwrap();
    assert_eq!(restarted.store.kv(COMPLETE).await.unwrap(), summary);
}

#[tokio::test]
async fn corrupt_input_blocks_startup_and_resume_preserves_verified_work() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let store = Store::open(&config.data_dir).unwrap();
    let vault = Vault::new(store.clone(), &config.data_dir).unwrap();
    let mut inputs = Vec::new();
    for index in 0..2 {
        let run = id();
        let bytes = vec![index + 1; 4096];
        let hash = digest::block(&bytes);
        let scope = format!("node-backups/{run}/blocks/{hash}");
        let path = config.data_dir.join(&scope);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let encoded = binary(&vault, &scope, &bytes);
        std::fs::write(&path, &encoded).unwrap();
        store
            .put(
                "node-backups",
                point(&vault, &run, &hash, bytes.len(), None, false),
            )
            .await
            .unwrap();
        inputs.push((path, encoded));
    }
    // Either planning order must preserve verified work and the corrupt source.
    std::fs::write(&inputs[0].0, b"truncated ciphertext").unwrap();
    let error = Service::new(config.clone()).await.err().unwrap();
    assert_eq!(error.status, 409);
    assert!(store.kv(COMPLETE).await.unwrap().is_none());
    let rows = store
        .read(|db| {
            Ok(db
                .0
                .query_row("SELECT COUNT(*) FROM disk_block_migration", [], |row| {
                    row.get::<_, i64>(0)
                })?)
        })
        .await
        .unwrap();
    assert_eq!(rows, 2);
    std::fs::write(&inputs[0].0, &inputs[0].1).unwrap();
    let s = Service::new(config).await.unwrap();
    assert!(s.store.kv(COMPLETE).await.unwrap().is_some());
    for (path, _) in inputs {
        assert!(std::fs::read(path).unwrap().starts_with(b"CAIRNB\x02\0"));
    }
}

#[tokio::test]
async fn local_atomic_rename_before_checkpoint_resumes_without_old_scope() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let store = Store::open(&config.data_dir).unwrap();
    let vault = Vault::new(store.clone(), &config.data_dir).unwrap();
    let run = id();
    let bytes = vec![7; 4096];
    let hash = digest::block(&bytes);
    let source = format!("node-backups/{run}/blocks/{hash}");
    let target = format!("node-backups/{run}/blocks-v2/{hash}");
    let path = config.data_dir.join(&source);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let encoded = cairn_installation::storage::block_codec::BlockCodec::default()
        .encode(vault, target.clone(), bytes.clone())
        .await
        .unwrap();
    std::fs::write(&path, &encoded).unwrap();
    let plan = json!({
        "source": source, "target": target, "hash": hash, "size": 4096,
        "location": null, "local_run": run, "discard": false
    });
    store
        .transaction(move |db| {
            db.0.execute(
                "INSERT INTO disk_block_migration(id,plan) VALUES('interrupted',?1)",
                [plan.to_string()],
            )?;
            db.set("migration:disk-blocks-v2-planned", &json!(true), None)
        })
        .await
        .unwrap();
    Service::new(config).await.unwrap();
    assert_eq!(
        std::fs::read(path).unwrap(),
        encoded,
        "An already-renamed object must not be re-encrypted"
    );
    assert!(store.kv(COMPLETE).await.unwrap().is_some());
}

async fn s3_fixture() -> (tempfile::TempDir, Arc<Service>, Storage) {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    std::fs::create_dir_all(&config.data_dir).unwrap();
    std::fs::write(
        config.data_dir.join("archive-s3.json"),
        json!({ "bucket": "cairn-node-test" }).to_string(),
    )
    .unwrap();
    let s = Service::new(config).await.unwrap();
    let storage = Storage::configured(&s).unwrap();
    (root, s, storage)
}

async fn ready_object(s: &Service, object: &str, hash: &str, size: usize) {
    let location =
        json!({ "destination": "s3", "bucket": "cairn-node-test", "endpoint": null }).to_string();
    let destination = hex::encode(Sha256::digest(location.as_bytes()));
    let (object, hash) = (object.to_owned(), hash.to_owned());
    s.store.write(move |db| {
        db.0.execute("INSERT INTO shared_objects(id,destination,hash,size,location,state) VALUES(?1,?2,?3,?4,?5,'ready')", params![object, destination, hash, size as i64, location])?;
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
#[ignore = "requires isolated loopback S3; run scripts/tests/node_s3_test.py --migration-tests"]
async fn startup_migration_verifies_shared_and_run_scoped_objects_before_removing_old_keys() {
    let (_root, s, storage) = s3_fixture().await;
    let object = id();
    let bytes = vec![7; snapshots::BLOCK as usize];
    let hash = digest::block(&bytes);
    let source = format!("shared-blocks/v1/{hash}/{object}");
    let target = shared_blocks::key(&hash, &object);
    storage
        .upload_bytes(binary(&s.vault, &source, &bytes), &source)
        .await
        .unwrap();
    // A versioned bucket must reclaim every old source version, not just the latest.
    storage
        .upload_bytes(binary(&s.vault, &source, &bytes), &source)
        .await
        .unwrap();
    ready_object(&s, &object, &hash, bytes.len()).await;
    let mut points = Vec::new();
    for _ in 0..2 {
        let point = point(&s.vault, &id(), &hash, bytes.len(), Some(&object), true);
        s.store.put("node-backups", point.clone()).await.unwrap();
        let (publication, run) = (
            point["id"].as_str().unwrap().to_owned(),
            point["runId"].as_str().unwrap().to_owned(),
        );
        let object = object.clone();
        s.store
            .write(move |db| {
                db.add_run(
                    &json!({
                        "id": run,
                        "taskId": "migration-fixture",
                        "status": "succeeded",
                        "createdAt": 0,
                        "backup": { "snapshotId": "before-upgrade" }
                    }),
                    None,
                )?;
                db.0.execute(
                    "INSERT INTO shared_publications VALUES(?1,?2)",
                    params![publication, run],
                )?;
                db.0.execute(
                    "INSERT INTO shared_references VALUES(?1,?2)",
                    params![publication, object],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        points.push(point);
    }
    let run = id();
    let json_bytes = b"old JSON/base64 partial disk block";
    let json_hash = hex::encode(Sha256::digest(json_bytes));
    let json_source = format!("node-backups/{run}/blocks/{json_hash}");
    let legacy = s
        .vault
        .encrypt(&json_source, &json!(STANDARD.encode(json_bytes)))
        .unwrap();
    storage
        .upload_bytes(serde_json::to_vec(&legacy).unwrap(), &json_source)
        .await
        .unwrap();
    let legacy_point = point(&s.vault, &run, &json_hash, json_bytes.len(), None, true);
    s.store
        .put("node-backups", legacy_point.clone())
        .await
        .unwrap();
    // A corrupt replacement must never authorize deletion of the intact source.
    storage
        .upload_bytes(b"bad replacement".to_vec(), &target)
        .await
        .unwrap();
    s.store.delete(COMPLETE).await.unwrap();
    assert!(Service::new(s.config.clone()).await.is_err());
    assert!(s.store.kv(COMPLETE).await.unwrap().is_none());
    assert!(
        storage
            .download_bytes(&source, MAX_ENCODED_BYTES)
            .await
            .is_ok()
    );
    storage.purge_key(&target).await.unwrap();
    let already_uploaded = cairn_installation::storage::block_codec::BlockCodec::default()
        .encode(s.vault.clone(), target.clone(), bytes.clone())
        .await
        .unwrap();
    storage
        .upload_bytes(already_uploaded.clone(), &target)
        .await
        .unwrap();
    let migrated = Service::new(s.config.clone()).await.unwrap();
    for point in &points {
        assert_eq!(
            migrated
                .store
                .get("node-backups", point["id"].as_str().unwrap())
                .await
                .unwrap(),
            Some(point.clone()),
            "Authenticated manifests and grant dependencies must stay identical"
        );
        assert_eq!(
            publication::read_block(&migrated, point, &hash)
                .await
                .unwrap(),
            bytes
        );
    }
    assert_eq!(
        publication::read_block(&migrated, &legacy_point, &json_hash)
            .await
            .unwrap(),
        json_bytes
    );
    assert!(
        storage
            .download_bytes(&source, MAX_ENCODED_BYTES)
            .await
            .is_err()
    );
    assert!(
        storage
            .download_bytes(&json_source, MAX_ENCODED_BYTES)
            .await
            .is_err()
    );
    let encoded = storage
        .download_bytes(&target, MAX_ENCODED_BYTES)
        .await
        .unwrap();
    assert!(encoded.len() < 4096);
    assert_eq!(
        encoded, already_uploaded,
        "A PUT completed before its checkpoint must be reused without overwrite"
    );
    assert_eq!(
        migrated.store.kv(COMPLETE).await.unwrap().unwrap()["objects"],
        2
    );
    // Re-uploading an old envelope at a new locator cannot re-enable legacy reads.
    storage
        .upload_bytes(binary(&s.vault, &target, &bytes), &target)
        .await
        .unwrap();
    assert_eq!(
        publication::read_block(&migrated, &points[0], &hash)
            .await
            .unwrap_err()
            .status,
        409
    );
    storage.purge_key(&target).await.unwrap();
    storage
        .purge_key(&format!("node-backups/{run}/blocks-v2/{json_hash}"))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires isolated loopback S3; run scripts/tests/node_s3_test.py --migration-tests"]
async fn startup_migration_resumes_after_source_deletion_before_checkpoint() {
    let (_root, s, storage) = s3_fixture().await;
    let object = id();
    let bytes = vec![19; 4096];
    let hash = digest::block(&bytes);
    let source = format!("shared-blocks/v1/{hash}/{object}");
    let target = shared_blocks::key(&hash, &object);
    let output = cairn_installation::storage::block_codec::BlockCodec::default()
        .encode(s.vault.clone(), target.clone(), bytes.clone())
        .await
        .unwrap();
    storage.upload_bytes(output.clone(), &target).await.unwrap();
    ready_object(&s, &object, &hash, bytes.len()).await;
    let point = point(&s.vault, &id(), &hash, bytes.len(), Some(&object), true);
    s.store.put("node-backups", point.clone()).await.unwrap();
    s.store.delete(COMPLETE).await.unwrap();
    let plan = json!({
        "source": source, "target": target, "hash": hash, "size": bytes.len(),
        "location": { "destination": "s3", "bucket": "cairn-node-test", "endpoint": null },
        "local_run": null, "discard": false
    });
    s.store
        .transaction(move |db| {
            db.0.execute(
                "INSERT INTO disk_block_migration(id,plan,state) VALUES('interrupted',?1,1)",
                [plan.to_string()],
            )?;
            db.set("migration:disk-blocks-v2-planned", &json!(true), None)
        })
        .await
        .unwrap();
    let migrated = Service::new(s.config.clone()).await.unwrap();
    assert_eq!(
        publication::read_block(&migrated, &point, &hash)
            .await
            .unwrap(),
        bytes
    );
    assert_eq!(
        storage
            .download_bytes(&target, MAX_ENCODED_BYTES)
            .await
            .unwrap(),
        output
    );
    storage.purge_key(&target).await.unwrap();
}

fn cpu_ms() -> f64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
        0
    );
    time.tv_sec as f64 * 1000.0 + time.tv_nsec as f64 / 1_000_000.0
}

#[tokio::test]
#[ignore = "release reference; run scripts/tests/node_s3_test.py --compression-benchmark"]
async fn blocking_upgrade_performance() {
    use rand::{RngCore, SeedableRng};
    use std::time::Instant;
    let mut random = vec![0; snapshots::BLOCK as usize];
    rand::rngs::StdRng::seed_from_u64(42).fill_bytes(&mut random);
    let source = include_bytes!("fixtures/block-codec-source.txt");
    let repeated: Vec<u8> = source
        .iter()
        .copied()
        .cycle()
        .take(snapshots::BLOCK as usize)
        .collect();
    let mut sparse = vec![0; snapshots::BLOCK as usize];
    sparse[..4096].fill(37);
    let mut mixed = repeated.clone();
    mixed[..random.len() / 2].copy_from_slice(&random[..random.len() / 2]);
    let inputs = [
        ("sparse", sparse),
        ("source", repeated),
        ("mixed", mixed),
        ("incompressible", random),
    ];
    for trial in 0..3 {
        let (_root, s, storage) = s3_fixture().await;
        let mut keys = Vec::new();
        let mut input_hashes = json!({});
        for (case, bytes) in &inputs {
            let object = id();
            let hash = digest::block(bytes);
            input_hashes[*case] = hash.clone().into();
            let source = format!("shared-blocks/v1/{hash}/{object}");
            let target = shared_blocks::key(&hash, &object);
            storage
                .upload_bytes(binary(&s.vault, &source, bytes), &source)
                .await
                .unwrap();
            ready_object(&s, &object, &hash, bytes.len()).await;
            let point = point(&s.vault, &id(), &hash, bytes.len(), Some(&object), true);
            s.store.put("node-backups", point).await.unwrap();
            keys.push((source, target));
        }
        let before_cpu = cpu_ms();
        let before = Instant::now();
        let baseline = Service::new(s.config.clone()).await.unwrap();
        let baseline_ms = before.elapsed().as_secs_f64() * 1000.0;
        let baseline_cpu_ms = cpu_ms() - before_cpu;
        baseline.shutdown.cancel();
        s.store.delete(COMPLETE).await.unwrap();
        let before_cpu = cpu_ms();
        let before = Instant::now();
        let migrated = Service::new(s.config.clone()).await.unwrap();
        let migration_ms = before.elapsed().as_secs_f64() * 1000.0;
        let migration_cpu_ms = cpu_ms() - before_cpu;
        let summary = migrated.store.kv(COMPLETE).await.unwrap().unwrap();
        assert_eq!(summary["objects"].as_u64(), Some(inputs.len() as u64));
        for (source, target) in keys {
            assert!(
                storage
                    .download_bytes(&source, MAX_ENCODED_BYTES)
                    .await
                    .is_err()
            );
            storage.purge_key(&target).await.unwrap();
        }
        println!(
            "STORAGE_MIGRATION_PERF {}",
            json!({
                "trial": trial,
                "inputHashes": input_hashes,
                "objects": inputs.len(),
                "plaintextBytes": inputs.len() as u64 * snapshots::BLOCK,
                "oldEncodedBytes": summary["oldEncodedBytes"],
                "newEncodedBytes": summary["newEncodedBytes"],
                "startupWithoutMigrationMs": baseline_ms,
                "startupWithoutMigrationCpuMs": baseline_cpu_ms,
                "startupWithMigrationMs": migration_ms,
                "startupWithMigrationCpuMs": migration_cpu_ms,
                "scope": "Manager startup with loopback S3, verified copy and source/version purge; excludes Moto CPU and WAN."
            })
        );
        migrated.shutdown.cancel();
        s.shutdown.cancel();
    }
}
