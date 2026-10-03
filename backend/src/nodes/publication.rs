//! Publish the current disk to S3, then collect unreachable generations.
//! New publications share immutable objects; existing run-scoped objects remain readable.
//! Persisted record and object names remain compatible with existing disks.
use super::{shared_blocks, snapshots};
use crate::{
    config::{id, now},
    error::{Error, Result},
    object_storage::Storage,
    performance::Operation,
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::task::{AbortHandle, JoinSet};

// Versioned binary blocks avoid both JSON/base64 layers used by legacy records.
const BINARY_BLOCK_HEADER: &[u8] = b"LEOBLK\x01\0";

const MIB: u64 = 1024 * 1024;

const DEFAULT_BUDGET_MIB: u64 = 102_400;

const CLEANUP_INTERVAL_MS: i64 = 60_000;

const S3: &str = "s3";

/// `blockFormat` of publications whose blocks are shared objects. Legacy points have none.
const SHARED_V1: &str = "shared-v1";

/// Conversations the scheduler may publish: running ones, and completed on-demand
/// disks that still have unpublished writes or an unfinished publication.
const SYNC_CANDIDATES: &str =
    "SELECT data FROM runs WHERE status='running'
        OR json_extract(data,'$.backup.requestedRevision') > COALESCE(json_extract(data,'$.backup.acknowledgedRevision'),0)
        OR (status IN ('succeeded','queued')
             AND json_extract(data,'$.storage.mode')='on-demand'
             AND (json_extract(data,'$.storage.dirtyBytes')>0
                  OR json_extract(data,'$.backup.status') IN ('pending','saving','error')))";

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Settings {
    pub interval_seconds: u64,
    pub budget_mi_b: u64,
    pub disconnect_timeout_seconds: u64,
    pub shutdown_timeout_seconds: u64,
    pub max_capacity_wait_seconds: u64,
    #[serde(skip_serializing)]
    pub s3_configured: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            interval_seconds: 60,
            budget_mi_b: DEFAULT_BUDGET_MIB,
            disconnect_timeout_seconds: 60,
            shutdown_timeout_seconds: 300,
            max_capacity_wait_seconds: 3600,
            s3_configured: None,
        }
    }
}

impl Settings {
    fn validate(&self) -> Result<()> {
        if !(5..=3600).contains(&self.interval_seconds)
            || !(128..=1_048_576).contains(&self.budget_mi_b)
            || !(10..=300).contains(&self.disconnect_timeout_seconds)
            || !(30..=300).contains(&self.shutdown_timeout_seconds)
            || self.max_capacity_wait_seconds > 3600
        {
            return Err(Error::bad("Invalid synchronization settings."));
        }
        Ok(())
    }
}

pub async fn settings(s: &Service) -> Result<Value> {
    let mut value = serde_json::to_value(Settings::default())?;
    if let Some(saved) = s.store.kv("node-backup-settings").await? {
        // Import only supported settings. Old destination/retention values have no effect.
        for (key, field) in value.as_object_mut().unwrap() {
            if let Some(saved) = saved.get(key) {
                *field = saved.clone();
            }
        }
    }
    Ok(value)
}

/// How long an execution may continue without hearing from its master.
pub(crate) async fn lease_ms(s: &Service) -> Result<u64> {
    Ok(settings(s).await?["disconnectTimeoutSeconds"]
        .as_u64()
        .unwrap_or(60)
        * 1000)
}

fn budget_bytes(settings: &Value) -> u64 {
    settings["budgetMiB"].as_u64().unwrap_or(DEFAULT_BUDGET_MIB) * MIB
}

pub async fn configure(s: &Service, input: Value) -> Result<Value> {
    let settings: Settings =
        serde_json::from_value(input).map_err(|e| Error::bad(e.to_string()))?;
    settings.validate()?;
    let value = serde_json::to_value(settings)?;
    s.store
        .set("node-backup-settings", value.clone(), None)
        .await?;
    s.store
        .audit("node.synchronization.configured", value.clone())
        .await?;
    Ok(value)
}

fn root(s: &Service, run: &str) -> PathBuf {
    s.config.data_dir.join("node-backups").join(run)
}

fn key(run: &str, hash: &str) -> String {
    format!("node-backups/{run}/blocks/{hash}")
}

fn manifest_key(run: &str, point: &str) -> String {
    format!("node-backups/{run}/{point}.json")
}

/// Whether a publication (or receipt location) is stored in S3.
pub(crate) fn in_s3(point: &Value) -> bool {
    point["destination"] == S3
}

fn shared_format(point: &Value) -> bool {
    point["blockFormat"] == SHARED_V1
}

/// Where an S3 copy lives. Receipt names hash this exact shape.
fn s3_location(bucket: impl Serialize, endpoint: impl Serialize) -> Value {
    json!({
        "destination": S3,
        "bucket": bucket,
        "endpoint": endpoint
    })
}

/// The bucket and endpoint of a point, as queued for remote deletion.
fn deletion_location(point: &Value) -> Value {
    json!({
        "bucket": point["bucket"],
        "endpoint": point["endpoint"]
    })
}

/// Progress of the latest publication, persisted in `run.backup.status`.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BackupStatus {
    Pending,
    Saving,
    Ready,
    Error,
}

impl BackupStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Saving => "saving",
            Self::Ready => "ready",
            Self::Error => "error",
        }
    }
}

/// The run's pointer to its current published disk (`run.backup`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublishedPointer<'a> {
    pub id: &'a Value,
    pub snapshot_id: &'a Value,
    pub captured_at: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uploaded_bytes: Option<u64>,
    pub status: BackupStatus,
    pub error: Option<String>,
}

pub async fn capture(s: &Service, run: &Value) -> Result<Value> {
    // Cancellation of an HTTP caller must not drop the operation lock while its
    // PUTs are still executing. The owned task finishes publication or recovery
    // bookkeeping; process death leaves durable reservations for the next boot.
    let (s, run) = (s.clone(), run.clone());
    tokio::spawn(async move { capture_owned(&s, &run).await })
        .await
        .map_err(Error::internal)?
}

async fn capture_owned(s: &Service, run: &Value) -> Result<Value> {
    let result = Box::pin(publish(s, run)).await;
    tracing::info!(
        target: "leo_performance",
        operation = "s3_totals",
        id = text(run, "id"),
        success = result.is_ok(),
        metrics = %s.hot_s3.performance()
    );

    // A capture continuing from a baseline may rely on blocks the master no longer
    // holds: forget the baseline so the next capture copies the whole disk. A VM that
    // is not ready yet (425) keeps its baseline.
    let failed = result.as_ref().is_err_and(|error| error.status != 425);
    if failed
        && run["backup"]["snapshotId"].is_string()
        && let Err(error) = forget_baseline(s, text(run, "id")).await
    {
        tracing::warn!(error = %error, "failed to forget the publication baseline");
    }
    result
}

async fn forget_baseline(s: &Service, run_id: &str) -> Result<()> {
    update_status(s, run_id, json!({ "snapshotId": null })).await
}

/// One publication of a node snapshot, from block upload to the run's new pointer.
struct Publication<'a> {
    s: &'a Service,
    run: &'a Value,
    run_id: &'a str,
    attempt: &'a str,
    node: &'a Value,
    storage: &'a Storage,
    base: &'a str,
    credential: &'a str,
    snapshot: &'a Value,
    snapshot_id: &'a str,
    budget: u64,
}

/// The durable upload intent of a publication and the shared objects it reserved.
struct Intent {
    backup_id: String,
    path: PathBuf,
    value: Value,
    manifest: Value,
    objects: Vec<shared_blocks::Object>,
}

async fn publish(s: &Service, run: &Value) -> Result<Value> {
    let run_id = text(run, "id");
    let mut timing = Operation::new("disk_publication", run_id, "queue");
    crate::validation::uuid(run_id)?;
    let _operation = s.node_backup_operation.lock(run_id).await;
    let _transfer = s.node_backup_operation.transfer().await;
    // A queued demand may have outlived its originating turn. Capture the live
    // attempt, not a stale stopped-run endpoint or publication revision.
    let current_run = s.store.run(run_id).await?;
    let run = &current_run;

    timing.next("collect_before");
    let checkpoint = super::checkpoint(s, run_id).await?;
    let attempt = text(&checkpoint, "runnerId");
    crate::validation::uuid(attempt)?;
    let settings = settings(s).await?;
    let storage = Storage::configured(s)?;
    collect_unused(s, run_id).await?;

    timing.next("snapshot");
    let base = super::transport::url(s, run_id).await?;
    let credential = super::runner_secret(s).await?;
    let snapshot = request_snapshot(s, run, &base, attempt, &credential).await?;

    if snapshot["alreadyPublished"] == true {
        let point = s
            .get("node-backups", text(&snapshot["published"], "backupId"))
            .await?;
        let grant = s
            .get("node-disk-grants", text(&snapshot, "grantId"))
            .await?;
        if point["runId"] != run_id
            || point["nodeId"] != checkpoint["nodeId"]
            || grant["runId"] != run_id
            || grant["nodeId"] != checkpoint["nodeId"]
        {
            return Err(Error::conflict(
                "Completed publication belongs to another disk owner.",
            ));
        }
        super::disk_grants::acknowledged(s, text(&snapshot, "grantId"), &point).await?;
        super::storage::refresh(s, run).await?;
        complete_request(
            s,
            run_id,
            run["backup"]["requestedRevision"].as_u64().unwrap_or(0),
        )
        .await?;
        timing.finish();
        return Ok(public(point));
    }

    timing.next("prepare_upload");
    let snapshot_id = text(&snapshot, "id");
    crate::validation::uuid(snapshot_id)?;
    let publication = Publication {
        s,
        run,
        run_id,
        attempt,
        node: &checkpoint["nodeId"],
        storage: &storage,
        base: &base,
        credential: &credential,
        snapshot: &snapshot,
        snapshot_id,
        budget: budget_bytes(&settings),
    };
    let result = publication.run(&mut timing).await;

    if result.is_ok() {
        timing.next("discard_snapshot");
    }
    // Best effort: the node replaces a leftover capture of this run on its next snapshot.
    let _ = s
        .http
        .delete(format!("{base}/snapshots/{snapshot_id}/discard"))
        .bearer_auth(credential)
        .timeout(Duration::from_secs(20))
        .send()
        .await;
    if result.is_ok() {
        timing.finish();
    }
    result
}

async fn request_snapshot(
    s: &Service,
    run: &Value,
    base: &str,
    attempt: &str,
    credential: &str,
) -> Result<Value> {
    let run_id = text(run, "id");
    if requested(run) {
        let response = s
            .http
            .post(format!("{base}/disks/{run_id}/snapshot-completed"))
            .bearer_auth(credential)
            .json(&json!({}))
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|_| Error::unavailable("Completed snapshot capture interrupted."))?;
        if response.status().is_success() {
            return response.json().await.map_err(Error::internal);
        }
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Err(Error::new(425, "Waiting for completed generation capture."));
        }
        if !matches!(response.status().as_u16(), 404 | 412) {
            return Err(Error::unavailable(
                "Unable to capture the completed disk generation.",
            ));
        }
        // Older nodes and pre-upgrade completed turns use the existing coherent
        // capture path. New nodes never pause a following turn for final upload.
    }
    let stopped = run["status"] != RunStatus::Running || run["moveRequest"]["idle"] == true;
    let capture_path = if stopped {
        format!("{base}/disks/{run_id}/snapshot")
    } else {
        format!("{base}/runs/{attempt}/snapshot")
    };
    let response = s
        .http
        .post(capture_path)
        .bearer_auth(credential)
        // The node copies only blocks written since this published point if it still tracks it.
        .json(&snapshot_request(run, stopped))
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|_| Error::unavailable("Snapshot capture interrupted."))?;
    if response.status() == reqwest::StatusCode::CONFLICT {
        // Startup, shutdown and an overlapping capture are temporary ownership
        // conflicts. Keep them distinct from broken storage or S3 configuration.
        return Err(Error::new(
            425,
            "Waiting for the VM to be ready for synchronization.",
        ));
    }
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Unable to capture a coherent VM snapshot.",
        ));
    }
    response.json().await.map_err(Error::internal)
}

fn snapshot_request(run: &Value, stopped: bool) -> Value {
    let periodic = !stopped && !requested(run) && !run["moveRequest"].is_object();
    json!({
        "baseline": run["backup"]["snapshotId"],
        "consistency": if periodic { "crash" } else { "filesystem" }
    })
}

impl Publication<'_> {
    async fn run(&self, timing: &mut Operation) -> Result<Value> {
        let s = self.s;
        let manifest = self.snapshot["manifest"].clone();
        snapshots::validate(&manifest)?;
        let directory = root(s, self.run_id);
        crate::skills::private_dir(&directory.join("blocks")).await?;
        let mut intent = self.write_intent(&directory, manifest).await?;

        timing.next("transfer_blocks");
        let uploaded = self
            .transfer_blocks(&intent.manifest, &intent.objects)
            .await?;

        timing.next("publish_manifest");
        self.publish_manifest(&mut intent, uploaded).await?;
        self.commit(&intent, uploaded).await?;

        timing.next("acknowledge_journal");
        if intent.manifest["onDemand"] == true {
            self.acknowledge(&intent.value, &intent.manifest).await?;
            self.complete().await?;
            // Completed runs leave the active-run monitor. Read back the final
            // journal counters so their UI does not retain an old dirty count.
            let _ = super::storage::refresh(s, self.run).await;
        }

        timing.next("collect_after");
        collect_unused(s, self.run_id).await?;
        Ok(public(intent.value))
    }

    /// Durable upload intent: the complete block inventory exists before any PUT.
    /// A restart can collect an interrupted first publication without S3 bucket scans.
    async fn write_intent(&self, directory: &Path, mut manifest: Value) -> Result<Intent> {
        let s = self.s;
        let admission = s.node_backup_lock.lock().await;
        let occupied = used(s).await?;
        let backup_id = id();
        let mut value = json!({
            "id": backup_id,
            "snapshotId": self.snapshot_id,
            "runId": self.run_id,
            "nodeId": self.node,
            "createdAt": now(),
            "capturedAt": manifest["capturedAt"],
            "diskGeneration": manifest["generation"],
            "sessionId": self.run["sessionId"],
            "destination": S3,
            "bucket": self.storage.bucket,
            "endpoint": self.storage.endpoint,
            "uploadedBytes": 0,
            "pauseMs": manifest["pauseMs"],
            "indexMs": manifest["indexMs"],
            "localBytesRead": manifest["localBytesRead"],
            "incremental": manifest["incremental"],
            "blockFormat": SHARED_V1
        });

        let point = value.clone();
        let (manifest, objects) = s
            .store
            .transaction(move |db| {
                let objects = shared_blocks::reserve(db, &point, &mut manifest)?;
                Ok((manifest, objects))
            })
            .await?;
        value["manifest"] = s.vault.encrypt(&format!("backup:{backup_id}"), &manifest)?;

        let path = directory.join(format!("{backup_id}.json"));
        let intent = serde_json::to_vec(&value)?;
        if occupied.saturating_add(intent.len() as u64) > self.budget {
            return Err(Error::new(
                507,
                "Publication cache budget exhausted; the current disk remains available.",
            ));
        }
        crate::skills::atomic_write(&path, &intent).await?;
        // Persist the rename and newly created run/root directories before remote writes.
        for parent in directory.ancestors().take(3) {
            tokio::fs::File::open(parent).await?.sync_all().await?;
        }
        drop(admission);

        Ok(Intent {
            backup_id,
            path,
            value,
            manifest,
            objects,
        })
    }

    /// Uploads every reserved object not yet in S3 and returns the bytes uploaded.
    async fn transfer_blocks(
        &self,
        manifest: &Value,
        objects: &[shared_blocks::Object],
    ) -> Result<u64> {
        // The common all-reused path never loads other manifests. Incremental
        // snapshots may omit old blocks, which migrate directly from S3.
        let needs_upload = objects.iter().any(|object| !object.ready);
        let sources = if manifest["incremental"] == true && needs_upload {
            source_blocks(self.s, self.run_id).await?
        } else {
            HashMap::new()
        };
        let missing = objects
            .iter()
            .filter(|object| !object.ready && !sources.contains_key(&object.hash))
            .map(|object| (object.hash.clone(), object.size))
            .collect();
        let mut reads = snapshots::Fetch::new(
            self.s.disk_http.clone(),
            format!("{}/snapshots/{}", self.base, self.snapshot_id),
            self.credential.to_owned(),
            missing,
        );

        let mut uploads = JoinSet::<Result<()>>::new();
        let transfer = self
            .start_uploads(objects, &sources, &mut reads, &mut uploads)
            .await;
        // On failure, finish every already-started PUT before the operation lock
        // can be released and its pending references retired by another capture.
        let mut result = transfer;
        while let Some(upload) = uploads.join_next().await {
            if let Err(error) = upload.map_err(Error::internal).and_then(|upload| upload) {
                result = Err(error);
            }
        }
        let uploaded = result?;

        tracing::info!(
            target: "leo_performance",
            operation = "disk_publication",
            id = self.run_id,
            uploaded_bytes = uploaded,
            referenced_blocks = objects.len(),
            reused_blocks = objects.iter().filter(|o| o.ready).count()
        );
        Ok(uploaded)
    }

    /// Encrypts and queues each missing object, keeping a bounded number of PUTs in flight.
    async fn start_uploads(
        &self,
        objects: &[shared_blocks::Object],
        sources: &Sources,
        reads: &mut snapshots::Fetch,
        uploads: &mut JoinSet<Result<()>>,
    ) -> Result<u64> {
        let mut uploaded = 0u64;
        for object in objects.iter().filter(|object| !object.ready) {
            let bytes = source_block(self.s, object, sources, reads).await?;
            let scope = shared_blocks::key(&object.hash, &object.id);
            let encoded = encrypt_block(self.s, scope.clone(), bytes).await?;
            let destination = self.storage.clone();
            uploads.spawn(async move { destination.upload_bytes(encoded, &scope).await });
            uploaded += object.size;
            if uploads.len() >= crate::object_storage::HOT_WRITE_CONCURRENCY {
                uploads
                    .join_next()
                    .await
                    .unwrap()
                    .map_err(Error::internal)??;
            }
        }
        Ok(uploaded)
    }

    /// Settles duplicate uploads, then writes the final manifest locally and to S3.
    async fn publish_manifest(&self, intent: &mut Intent, uploaded: u64) -> Result<()> {
        let s = self.s;
        let publication = intent.backup_id.clone();
        let mut manifest = std::mem::take(&mut intent.manifest);
        intent.manifest = s
            .store
            .transaction(move |db| {
                shared_blocks::complete_uploads(db, &publication, &mut manifest)?;
                Ok(manifest)
            })
            .await?;

        let scope = format!("backup:{}", intent.backup_id);
        intent.value["manifest"] = s.vault.encrypt(&scope, &intent.manifest)?;
        intent.value["uploadedBytes"] = uploaded.into();
        crate::skills::atomic_write(&intent.path, &serde_json::to_vec(&intent.value)?).await?;
        self.storage
            .upload_file_verified(&intent.path, &manifest_key(self.run_id, &intent.backup_id))
            .await
    }

    /// Records the point and moves the run's pointer, unless the disk changed owner meanwhile.
    async fn commit(&self, intent: &Intent, uploaded: u64) -> Result<()> {
        let pointer = PublishedPointer {
            id: &intent.value["id"],
            snapshot_id: &intent.value["snapshotId"],
            captured_at: &intent.manifest["capturedAt"],
            uploaded_bytes: Some(uploaded),
            status: if self.snapshot["manifest"]["onDemand"] == true {
                BackupStatus::Saving
            } else {
                BackupStatus::Ready
            },
            error: None,
        };
        let patch = json!({ "backup": pointer });
        let run = self.run_id.to_owned();
        let attempt = self.attempt.to_owned();
        let node = self.node.clone();
        let on_demand = self.snapshot["manifest"]["onDemand"] == true;
        let grant_id = self.snapshot["grantId"].as_str().map(str::to_owned);
        let point = intent.value.clone();
        self.s
            .store
            .transaction(move |db| {
                let current = super::db_checkpoint(db, &run)?;
                let same_disk = if on_demand {
                    match grant_id {
                        Some(grant_id) => db
                            .get("node-disk-grants", &grant_id)?
                            .is_some_and(|grant| grant["runId"] == run && grant["nodeId"] == node),
                        None => false,
                    }
                } else {
                    current["runnerId"] == attempt
                };
                if !same_disk || current["nodeId"] != node {
                    return Err(Error::conflict("Disk owner changed during publication."));
                }
                shared_blocks::verified(db, text(&point, "id"))?;
                db.put("node-backups", &point)?;
                let current_run = db
                    .run(&run)?
                    .ok_or_else(|| Error::not_found("Conversation missing."))?;
                let mut backup = current_run["backup"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                backup.extend(patch["backup"].as_object().unwrap().clone());
                db.patch_run(&run, &json!({ "backup": backup }))?;
                Ok(())
            })
            .await
    }

    async fn complete(&self) -> Result<()> {
        let revision = self.run["backup"]["requestedRevision"]
            .as_u64()
            .unwrap_or(0);
        complete_request(self.s, self.run_id, revision).await
    }

    /// An on-demand disk keeps reading its old base until the node confirms the new one.
    async fn acknowledge(&self, point: &Value, manifest: &Value) -> Result<()> {
        let grant = text(self.snapshot, "grantId");
        super::disk_grants::extend(self.s, grant, point).await?;
        let body = json!({
            "generation": manifest["generation"],
            "backupId": point["id"],
            "grantId": self.snapshot["grantId"]
        });
        let response = self
            .s
            .http
            .post(format!("{}/disks/{}/published", self.base, self.run_id))
            .bearer_auth(self.credential)
            .json(&body)
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(|_| Error::unavailable("Disk publication acknowledgement interrupted."))?;
        if !response.status().is_success() {
            return Err(Error::unavailable(
                "Node could not acknowledge the published disk.",
            ));
        }
        super::disk_grants::acknowledged(self.s, grant, point).await
    }
}

async fn complete_request(s: &Service, run: &str, revision: u64) -> Result<()> {
    let run = run.to_owned();
    s.store
        .transaction(move |db| {
            let current = db
                .run(&run)?
                .ok_or_else(|| Error::not_found("Conversation missing."))?;
            let mut backup = current["backup"].as_object().cloned().unwrap_or_default();
            let requested = backup
                .get("requestedRevision")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            backup.insert("acknowledgedRevision".into(), revision.into());
            backup.insert(
                "status".into(),
                json!(if requested > revision {
                    BackupStatus::Pending
                } else {
                    BackupStatus::Ready
                }),
            );
            backup.insert("error".into(), Value::Null);
            db.patch_run(&run, &json!({ "backup": backup }))?;
            Ok(())
        })
        .await?;
    s.node_publication_notify.notify_one();
    Ok(())
}

async fn encrypt_block(s: &Service, scope: String, bytes: Vec<u8>) -> Result<Vec<u8>> {
    let vault = s.vault.clone();
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut encoded = BINARY_BLOCK_HEADER.to_vec();
        encoded.extend(vault.encrypt_bytes(&scope, &bytes)?);
        Ok(encoded)
    })
    .await
    .map_err(Error::internal)?
}

/// Blocks of this run's earlier publications, by hash, with the point that holds them.
type Sources = HashMap<String, (Arc<Value>, Value)>;

async fn source_blocks(s: &Service, run: &str) -> Result<Sources> {
    let mut index = HashMap::new();
    for point in s.store.node_backups_for_run(run).await? {
        let Ok(manifest) = manifest(s, &point).await else {
            continue;
        };
        let point = Arc::new(point);
        for block in manifest["blocks"].as_array().into_iter().flatten() {
            if let Some(hash) = block["hash"].as_str() {
                index.insert(hash.to_owned(), (point.clone(), block.clone()));
            }
        }
    }
    Ok(index)
}

async fn source_block(
    s: &Service,
    object: &shared_blocks::Object,
    sources: &Sources,
    reads: &mut snapshots::Fetch,
) -> Result<Vec<u8>> {
    let bytes = if let Some((point, block)) = sources.get(&object.hash) {
        read_manifest_block(s, point, block).await?
    } else {
        reads.block(&object.hash).await?
    };
    if bytes.len() as u64 != object.size || !crate::storage::digest::matches(&object.hash, &bytes) {
        return Err(Error::bad("Backup block failed integrity verification."));
    }
    Ok(bytes)
}

fn receipt(file: &Path, location: &Value) -> Result<PathBuf> {
    let location = s3_location(&location["bucket"], &location["endpoint"]);
    Ok(file.with_extension(format!(
        "s3-{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&location)?))
    )))
}

/// The S3 location recorded by a receipt, or the bucket named in its file name.
async fn receipt_location(path: &Path, bucket: &str) -> Result<Value> {
    let bytes = tokio::fs::read(path).await?;
    Ok(serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        json!({
            "destination": S3,
            "bucket": bucket
        })
    }))
}

pub fn public(mut value: Value) -> Value {
    value.as_object_mut().unwrap().remove("manifest");
    value
}

pub async fn manifest(s: &Service, backup: &Value) -> Result<Value> {
    let value = s.vault.decrypt(
        &format!("backup:{}", text(backup, "id")),
        &backup["manifest"],
    )?;
    snapshots::validate(&value)?;
    if shared_format(backup) {
        for block in value["blocks"].as_array().into_iter().flatten() {
            if block["hash"].is_string() {
                crate::validation::uuid(text(block, "object"))?;
            }
        }
    } else if !backup["blockFormat"].is_null() {
        return Err(Error::bad("Unsupported disk block format."));
    }
    Ok(value)
}

pub async fn read_block(s: &Service, backup: &Value, hash: &str) -> Result<Vec<u8>> {
    if !snapshots::valid_hash(hash) {
        return Err(Error::bad("Invalid backup block."));
    }
    if !shared_format(backup) {
        return read_legacy_block(s, backup, hash).await;
    }
    let manifest = manifest(s, backup).await?;
    let block = snapshots::find_block(&manifest, hash)
        .ok_or_else(|| Error::forbidden("Block outside disk scope."))?;
    read_manifest_block(s, backup, block).await
}

/// The authorized read handler already has this authenticated manifest block.
/// Avoid decrypting the manifest twice and never consult the ownership inventory.
pub(crate) async fn read_manifest_block(
    s: &Service,
    backup: &Value,
    block: &Value,
) -> Result<Vec<u8>> {
    let hash = text(block, "hash");
    if !shared_format(backup) {
        return read_legacy_block(s, backup, hash).await;
    }
    let object = text(block, "object");
    crate::validation::uuid(object)?;
    if !snapshots::valid_hash(hash) {
        return Err(Error::bad("Invalid backup block."));
    }

    let scope = shared_blocks::key(hash, object);
    let storage = storage_for(s, backup)?;
    let result = async {
        let encoded = storage
            .download_bytes(&scope, snapshots::BLOCK + 36)
            .await?;
        let bytes = decode_scoped_block(s, &scope, hash, encoded).await?;
        if Some(bytes.len() as u64) != block["size"].as_u64() {
            return Err(Error::conflict("Recovery block size mismatch."));
        }
        Ok(bytes)
    }
    .await;
    if result
        .as_ref()
        .is_err_and(|error: &Error| matches!(error.status, 400 | 409))
    {
        shared_blocks::invalidate(s, object).await?;
        forget_baseline(s, text(backup, "runId")).await?;
    }
    result
}

async fn read_legacy_block(s: &Service, backup: &Value, hash: &str) -> Result<Vec<u8>> {
    let run = text(backup, "runId");
    let path = root(s, run).join("blocks").join(hash);
    if let Ok(encoded) = tokio::fs::read(&path).await {
        match decode_block(s, run, hash, encoded).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                // A failed restore must not leave known-bad bytes in the cache.
                tokio::fs::remove_file(&path).await?;
                if !in_s3(backup) {
                    forget_baseline(s, run).await?;
                    return Err(error);
                }
            }
        }
    }
    if !in_s3(backup) {
        forget_baseline(s, run).await?;
        return Err(Error::unavailable("Local recovery block is missing."));
    }

    let storage = storage_for(s, backup)?;
    crate::skills::private_dir(path.parent().unwrap()).await?;
    let recovered = async {
        let encoded = storage
            .download_bytes(&key(run, hash), snapshots::BLOCK * 3 + 4096)
            .await?;
        decode_block(s, run, hash, encoded).await
    }
    .await;
    if let Err(error) = &recovered
        && matches!(error.status, 400 | 409)
    {
        // An outage or access refusal says nothing about the integrity of a verified copy.
        let mark = receipt(&path, backup)?;
        if mark.exists() {
            tokio::fs::remove_file(mark).await?;
        }
        forget_baseline(s, run).await?;
    }
    recovered
}

async fn decode_block(s: &Service, run: &str, hash: &str, encoded: Vec<u8>) -> Result<Vec<u8>> {
    decode_scoped_block(s, &key(run, hash), hash, encoded).await
}

async fn decode_scoped_block(
    s: &Service,
    scope: &str,
    hash: &str,
    encoded: Vec<u8>,
) -> Result<Vec<u8>> {
    let vault = s.vault.clone();
    let scope = scope.to_owned();
    let hash = hash.to_owned();
    tokio::task::spawn_blocking(move || {
        let bytes = if let Some(ciphertext) = encoded.strip_prefix(BINARY_BLOCK_HEADER) {
            vault.decrypt_bytes(&scope, ciphertext)?
        } else {
            // Published S3 blocks remain readable independently of archive removal.
            let value: Value = serde_json::from_slice(&encoded)?;
            let plaintext = vault.decrypt(&scope, &value)?;
            STANDARD
                .decode(plaintext.as_str().unwrap_or(""))
                .map_err(|_| Error::bad("Invalid backup ciphertext."))?
        };
        if !crate::storage::digest::matches(&hash, &bytes) {
            return Err(Error::bad("Backup integrity check failed."));
        }
        Ok(bytes)
    })
    .await
    .map_err(Error::internal)?
    // Decoding has no network or filesystem effects. Invalid envelopes,
    // authentication failures and digest mismatches all require repair.
    .map_err(|_| Error::conflict("Recovery block integrity check failed."))
}

/// The local controller owns the configured node cache. Retire duplicate S3
/// payloads on the master.
pub async fn maintain_local_cache(s: &Service) -> Result<()> {
    let Some(_operation) = s.node_backup_operation.try_cache_eviction() else {
        return Ok(());
    };
    let _guard = s.node_backup_lock.lock().await;
    let budget = budget_bytes(&settings(s).await?);
    make_room(s, used(s).await?, 0, budget).await?;
    Ok(())
}

async fn make_room(s: &Service, mut occupied: u64, incoming: u64, budget: u64) -> Result<u64> {
    // Keep this admission policy separate from storage::cache: publication may
    // use the emergency reserve so a dirty journal can still make progress.
    let node = s
        .store
        .get("nodes", super::LOCAL_NODE_ID)
        .await?
        .unwrap_or_default();
    let policy = crate::storage::policy::Policy::for_node(&node["storage"])?;
    let (total, _) = crate::storage::policy::space(&s.config.data_dir)?;
    let reserve = policy.reserve(total);
    let required = occupied
        .saturating_add(incoming)
        .saturating_sub(budget)
        .max(
            reserve
                .saturating_add(incoming.saturating_mul(3))
                .saturating_sub(crate::storage::policy::space(&s.config.data_dir)?.1),
        );

    let mut candidates = evictable_blocks(s).await?;
    candidates.sort_by_key(|v| v.2);
    let mut removed = 0;
    for (file, size, _) in candidates {
        if tokio::fs::try_exists(&file).await? {
            tokio::fs::remove_file(file).await?;
            removed += size;
            occupied = occupied.saturating_sub(size);
        }
    }

    if removed < required
        && (occupied.saturating_add(incoming) > budget
            || crate::storage::policy::space(&s.config.data_dir)?.1
                < incoming.saturating_mul(3).saturating_add(32 * MIB))
    {
        return Err(Error::new(
            507,
            "Backup cache and free-space reserve are exhausted; unsaved work is retained.",
        ));
    }
    Ok(occupied)
}

/// Cached blocks with an S3 receipt, with their size and modification time.
async fn evictable_blocks(s: &Service) -> Result<Vec<(PathBuf, u64, std::time::SystemTime)>> {
    let mut candidates = Vec::new();
    let root = s.config.data_dir.join("node-backups");
    if !root.exists() {
        return Ok(candidates);
    }
    let mut runs = tokio::fs::read_dir(root).await?;
    while let Some(run) = runs.next_entry().await? {
        if !run.file_type().await?.is_dir() {
            continue;
        }
        let blocks = run.path().join("blocks");
        if !blocks.exists() {
            continue;
        }
        let mut entries = tokio::fs::read_dir(&blocks).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((hash, _)) = name.split_once(".s3-") else {
                continue;
            };
            if !snapshots::valid_hash(hash) {
                continue;
            }
            let file = blocks.join(hash);
            if let Ok(meta) = tokio::fs::metadata(&file).await {
                let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                candidates.push((file, meta.len(), modified));
            }
        }
    }
    Ok(candidates)
}

async fn used(s: &Service) -> Result<u64> {
    let mut total = 0u64;
    let mut pending = vec![s.config.data_dir.join("node-backups")];
    while let Some(path) = pending.pop() {
        if !path.exists() {
            continue;
        }
        let mut entries = match tokio::fs::read_dir(path).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let meta = match entry.metadata().await {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

/// Remove superseded publications only after every disk has released its read grant.
/// This also serves idle disks: cleanup must not depend on another guest write.
pub async fn collect(s: &Service, run: &str) -> Result<()> {
    let _operation = s.node_backup_operation.lock(run).await;
    collect_unused(s, run).await
}

/// Publications a run still depends on, and the legacy blocks they reference.
struct Retained {
    points: Vec<Value>,
    keep: HashSet<String>,
    needed: HashSet<String>,
}

/// Decided while holding the cache lock. `None` defers cleanup.
async fn retained(s: &Service, run: &str) -> Result<Option<Retained>> {
    let mut keep = super::disk_grants::pinned(s, run).await?;
    let _guard = s.node_backup_lock.lock().await;
    let points = s.store.node_backups_for_run(run).await?;
    let current = s.store.run(run).await?;
    // The publication pointer is authoritative even if timestamps are equal or clocks regress.
    if let Some(head) = current["backup"]["id"].as_str() {
        keep.insert(head.to_owned());
    } else if !points.is_empty() {
        return Err(Error::conflict(
            "Published disk pointer missing; cleanup deferred.",
        ));
    }
    if let Some(id) = current["moveRequest"]["backupId"].as_str() {
        keep.insert(id.to_owned());
    }
    if keep
        .iter()
        .any(|id| !points.iter().any(|point| point["id"] == *id))
    {
        return Err(Error::conflict(
            "A referenced disk manifest is missing; cleanup deferred.",
        ));
    }

    let mut needed = HashSet::new();
    let retained_legacy = points
        .iter()
        .filter(|point| keep.contains(text(point, "id")) && !shared_format(point));
    for point in retained_legacy {
        // A damaged retained manifest must not prevent creating a fresh good point,
        // nor cause dependencies we cannot identify to be deleted.
        let Ok(manifest) = manifest(s, point).await else {
            return Ok(None);
        };
        needed.extend(snapshots::hashes(&manifest).map(str::to_owned));
    }
    Ok(Some(Retained {
        points,
        keep,
        needed,
    }))
}

async fn collect_unused(s: &Service, run: &str) -> Result<()> {
    let mut timing = Operation::new("disk_collection", run, "reader_lock");
    let readers = s.node_backup_operation.drain(run).await;

    timing.next("inventory");
    let Some(retained) = retained(s, run).await? else {
        return Ok(());
    };

    // Drain readers only through metadata retirement. Filesystem maintenance and
    // remote deletion must not hold the disk-reader lock.
    timing.next("retire_objects");
    retire_points(s, run, &retained).await?;
    drop(readers);

    queue_abandoned_intents(s, run, &retained).await?;
    queue_unneeded_blocks(s, run, &retained.needed).await?;
    let run = run.to_owned();
    let keep = retained.keep;
    s.store
        .transaction(move |db| shared_blocks::release_run(db, &run, &keep))
        .await?;
    timing.finish();
    Ok(())
}

/// Removes superseded publications and releases their shared objects.
async fn retire_points(s: &Service, run: &str, retained: &Retained) -> Result<()> {
    let superseded = retained
        .points
        .iter()
        .filter(|point| !retained.keep.contains(text(point, "id")));
    for point in superseded {
        let point_id = text(point, "id").to_owned();
        if in_s3(point) && !root(s, run).join(format!("{point_id}.json")).exists() {
            queue_deletion(s, point, &manifest_key(run, &point_id), false).await?;
        }
        s.store
            .transaction(move |db| {
                shared_blocks::release(db, &point_id)?;
                db.remove("node-backups", &point_id)
            })
            .await?;
    }
    Ok(())
}

/// The publication id of a local `{id}.json` manifest.
fn intent_id(entry: &tokio::fs::DirEntry) -> Option<String> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let id = name.strip_suffix(".json")?;
    crate::validation::uuid(id).ok()?;
    Some(id.to_owned())
}

async fn read_intent(path: &Path, id: &str, run: &str, deferred: &str) -> Result<Value> {
    let point: Value = serde_json::from_slice(&tokio::fs::read(path).await?)?;
    if point["id"] != id || point["runId"] != run {
        return Err(Error::bad(format!(
            "Invalid publication upload inventory; {deferred} deferred."
        )));
    }
    Ok(point)
}

/// Local manifests also serve as upload intents. They precede every remote write,
/// including first publication, and remain discoverable if the DB commit never happened.
async fn queue_abandoned_intents(s: &Service, run: &str, retained: &Retained) -> Result<()> {
    let directory = root(s, run);
    if !directory.exists() {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(&directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        let Some(point_id) = intent_id(&entry) else {
            continue;
        };
        if retained.keep.contains(&point_id) {
            continue;
        }
        let point = read_intent(&entry.path(), &point_id, run, "cleanup").await?;
        if in_s3(&point) {
            let keys = abandoned_keys(s, run, &point, &retained.needed).await?;
            let location = deletion_location(&point);
            s.store
                .transaction(move |db| {
                    for key in keys {
                        shared_blocks::queue(db, &location, &key, false)?;
                    }
                    Ok(())
                })
                .await?;
        }
        tokio::fs::remove_file(entry.path()).await?;
    }
    Ok(())
}

/// Remote objects an abandoned intent may have written: its manifest and, for a
/// legacy point, run-scoped blocks that nothing retained needs.
async fn abandoned_keys(
    s: &Service,
    run: &str,
    point: &Value,
    needed: &HashSet<String>,
) -> Result<Vec<String>> {
    let mut keys = vec![manifest_key(run, text(point, "id"))];
    if !point["blockFormat"].is_null() {
        return Ok(keys);
    }
    let manifest = manifest(s, point).await?;
    let blocks = root(s, run).join("blocks");
    for hash in snapshots::hashes(&manifest).collect::<HashSet<_>>() {
        if !needed.contains(hash) && !receipt(&blocks.join(hash), point)?.exists() {
            keys.push(key(run, hash));
        }
    }
    Ok(keys)
}

async fn queue_unneeded_blocks(s: &Service, run: &str, needed: &HashSet<String>) -> Result<()> {
    let blocks = root(s, run).join("blocks");
    if !blocks.exists() {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(&blocks).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let hash = name.split('.').next().unwrap_or("");
        if !snapshots::valid_hash(hash) || needed.contains(hash) {
            continue;
        }
        if let Some((_, bucket)) = name.split_once(".s3-") {
            let location = receipt_location(&entry.path(), bucket).await?;
            queue_deletion(s, &location, &key(run, hash), false).await?;
        }
        tokio::fs::remove_file(entry.path()).await?;
    }
    Ok(())
}

async fn queue_deletion(s: &Service, point: &Value, key: &str, prefix: bool) -> Result<()> {
    let location = deletion_location(point);
    let key = key.to_owned();
    s.store
        .transaction(move |db| shared_blocks::queue(db, &location, &key, prefix))
        .await
}

/// Every isolated conversation uses the same continuous S3 publication path.
pub fn protected(run: &Value) -> bool {
    run["isolated"] == true
}

pub async fn attempt(s: &Service, run: &Value) {
    if !protected(run) {
        return;
    }
    let run_id = text(run, "id");
    if let Err(error) = update_status(s, run_id, json!({ "status": BackupStatus::Saving })).await {
        tracing::warn!(error = %error, "failed to record synchronization start");
    }
    let Err(error) = capture(s, run).await else {
        return;
    };

    if error.status == 425 {
        let pending = json!({
            "status": BackupStatus::Pending,
            "error": null
        });
        if let Err(error) = update_status(s, run_id, pending).await {
            tracing::warn!(error = %error, "failed to record pending synchronization");
        }
        return;
    }

    // Nothing to save yet, or an older guest image whose limitation is shown in the conversation.
    if error.status != 409 && error.status != 412 {
        let body = format!(
            "The current disk could not be synchronized: {}",
            error.message
        );
        let raised = super::alerts::raise(
            s,
            run_id,
            "backup-failed",
            "S3 synchronization failed",
            &body,
        )
        .await;
        if let Err(error) = raised {
            tracing::warn!(error = %error, "failed to raise synchronization alert");
        }
    }

    // Publication may have committed before its controller acknowledgement failed.
    // Patch the current state transactionally, never overwrite it with the caller's old pointer.
    let failure = json!({
        "status": BackupStatus::Error,
        "error": error.message,
        "snapshotId": null
    });
    if let Err(error) = update_status(s, run_id, failure).await {
        tracing::warn!(error = %error, "failed to record synchronization failure");
    }
}

/// Durable, coalesced demand. The scheduler owns uploads, independently of the
/// completed turn's account and execution leases; a restart rediscovers demand.
pub async fn request(s: &Service, run: &str) -> Result<()> {
    let run = run.to_owned();
    s.store
        .transaction(move |db| {
            let current = db
                .run(&run)?
                .ok_or_else(|| Error::not_found("Conversation missing."))?;
            let mut backup = current["backup"].as_object().cloned().unwrap_or_default();
            let revision = backup
                .get("requestedRevision")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| Error::conflict("Publication revision exhausted."))?;
            backup.insert("requestedRevision".into(), revision.into());
            backup.insert("status".into(), json!(BackupStatus::Pending));
            db.patch_run(&run, &json!({ "backup": backup }))?;
            Ok(())
        })
        .await?;
    s.node_publication_notify.notify_one();
    Ok(())
}

pub(crate) fn requested(run: &Value) -> bool {
    run["backup"]["requestedRevision"].as_u64().unwrap_or(0)
        > run["backup"]["acknowledgedRevision"].as_u64().unwrap_or(0)
}

async fn update_status(s: &Service, run: &str, patch: Value) -> Result<()> {
    let run = run.to_owned();
    s.store
        .transaction(move |db| {
            let current = db
                .run(&run)?
                .ok_or_else(|| Error::not_found("Conversation missing."))?;
            let mut status = current["backup"].as_object().cloned().unwrap_or_default();
            status.extend(patch.as_object().unwrap().clone());
            db.patch_run(&run, &json!({ "backup": status }))?;
            Ok(())
        })
        .await
}

async fn collection_runs(s: &Service) -> Result<HashSet<String>> {
    let mut runs = s
        .store
        .list("node-backups")
        .await?
        .iter()
        .filter_map(|point| point["runId"].as_str().map(str::to_owned))
        .collect::<HashSet<_>>();
    let pending = s
        .store
        .read(|db| {
            let mut stmt =
                db.0.prepare_cached("SELECT DISTINCT run_id FROM shared_publications")?;
            Ok(stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await?;
    runs.extend(pending);
    let root = s.config.data_dir.join("node-backups");
    if root.exists() {
        let mut entries = tokio::fs::read_dir(root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().await?.is_dir() && crate::validation::uuid(&name).is_ok() {
                runs.insert(name);
            }
        }
    }
    Ok(runs)
}

pub async fn maintain(s: Arc<Service>) {
    let mut scheduler = Scheduler {
        s,
        last: HashMap::new(),
        last_cleanup: 0,
        tasks: JoinSet::new(),
        active: HashMap::new(),
        cleanup: HashSet::new(),
    };
    loop {
        tokio::select! {
            () = scheduler.s.shutdown.cancelled() => break,
            () = scheduler.s.node_publication_notify.notified() => {},
            () = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        scheduler.reap();
        scheduler.queue_cleanups().await;
        scheduler.start_publications().await;
        scheduler.start_cleanups();
    }
}

/// Background publications and cleanups, bounded by the synchronization slots and
/// never more than one task per conversation.
struct Scheduler {
    s: Arc<Service>,
    last: HashMap<String, i64>,
    last_cleanup: i64,
    tasks: JoinSet<()>,
    active: HashMap<String, AbortHandle>,
    cleanup: HashSet<String>,
}

impl Scheduler {
    fn reap(&mut self) {
        while self.tasks.try_join_next().is_some() {}
        self.active.retain(|_, task| !task.is_finished());
    }

    async fn queue_cleanups(&mut self) {
        if now() - self.last_cleanup < CLEANUP_INTERVAL_MS {
            return;
        }
        self.last_cleanup = now();
        if let Ok(runs) = collection_runs(&self.s).await {
            self.cleanup.extend(runs);
        }
    }

    async fn start_publications(&mut self) {
        let settings = settings(&self.s).await.unwrap_or_default();
        let interval = settings["intervalSeconds"].as_i64().unwrap_or(60) * 1000;
        let candidates = self
            .s
            .store
            .read(|db| db.json_rows(SYNC_CANDIDATES, []))
            .await;
        let Ok(mut runs) = candidates else {
            return;
        };

        // Overdue work first, oldest scheduling time within each class. Normal
        // work also becomes overdue, so busy early rows cannot starve it.
        runs.sort_by_key(|run| {
            (
                run["storage"]["backupUrgent"] != true,
                self.last.get(text(run, "id")).copied().unwrap_or(0),
            )
        });
        for run in runs {
            if self.tasks.len() >= super::coordination::SYNC_CONCURRENCY {
                break;
            }
            let id = text(&run, "id");
            let since = now() - self.last.get(id).copied().unwrap_or(0);
            if self.active.contains_key(id) || !publication_due(&run, since, interval) {
                continue;
            }
            let run_id = id.to_owned();
            let still_active = self
                .s
                .store
                .read(move |db| crate::conversation_lifecycle::require_active_run(db, &run_id))
                .await
                .is_ok();
            // Trashing a conversation stops execution, not its already durable
            // final publication request. Purge and publication share the disk
            // operation lock; a missing run is rejected before any upload.
            if !still_active && !requested(&run) {
                continue;
            }

            let run_id = id.to_owned();
            self.last.insert(run_id.clone(), now());
            self.cleanup.remove(&run_id);
            let s = self.s.clone();
            let task = self.tasks.spawn(async move {
                attempt(&s, &run).await;
            });
            self.active.insert(run_id, task);
        }
    }

    fn start_cleanups(&mut self) {
        let remaining = super::coordination::SYNC_CONCURRENCY.saturating_sub(self.tasks.len());
        let ready: Vec<_> = self
            .cleanup
            .iter()
            .filter(|run| !self.active.contains_key(*run))
            .take(remaining)
            .cloned()
            .collect();
        for run in ready {
            self.cleanup.remove(&run);
            let run_id = run.clone();
            let s = self.s.clone();
            let task = self.tasks.spawn(async move {
                let Err(error) = collect(&s, &run).await else {
                    return;
                };
                let detail = json!({
                    "runId": run,
                    "message": error.message
                });
                if let Err(error) = s.store.audit("disk.cleanup_failed", detail).await {
                    tracing::warn!(error = %error, "failed to audit disk cleanup failure");
                }
            });
            self.active.insert(run_id, task);
        }
    }
}

/// An isolated conversation publishes periodically while it has something new.
fn publication_due(run: &Value, since_last_ms: i64, default_interval_ms: i64) -> bool {
    let on_demand = run["storage"]["mode"] == super::ON_DEMAND;
    let synchronized = on_demand
        && run["storage"]["dirtyBytes"] == 0
        && run["backup"]["status"] == BackupStatus::Ready.as_str();
    let interval = run["storage"]["backupSeconds"]
        .as_i64()
        .map_or(default_interval_ms, |seconds| seconds * 1000);
    let requested = run["backup"]["requestedRevision"].as_u64().unwrap_or(0);
    let acknowledged = run["backup"]["acknowledgedRevision"].as_u64().unwrap_or(0);
    protected(run)
        && (run["sessionId"].is_string() || on_demand)
        && (!run["moveRequest"].is_object() || requested > acknowledged)
        && !synchronized
        && since_last_ms
            >= if requested > acknowledged || run["storage"]["backupUrgent"] == true {
                5000
            } else {
                interval
            }
}

pub(crate) fn storage_for(s: &Service, backup: &Value) -> Result<Storage> {
    if !in_s3(backup) {
        return Err(Error::unavailable("Local recovery block is missing."));
    }
    let mut storage = Storage::configured(s)?;
    if storage.endpoint.as_deref() != backup["endpoint"].as_str() {
        return Err(Error::conflict(
            "This recovery point belongs to a different S3 endpoint. Restore its \
                storage configuration before accessing it.",
        ));
    }
    storage.bucket = backup["bucket"]
        .as_str()
        .filter(|b| {
            !b.is_empty()
                && b.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
        })
        .ok_or_else(|| Error::bad("Invalid backup bucket."))?
        .into();
    Ok(storage)
}

/// Retire a conversation's recovery references and durably queue remote reclamation.
pub async fn purge(s: &Service, run: &str) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    crate::validation::uuid(run)?;
    let _operation = s.node_backup_operation.lock(run).await;
    let _readers = s.node_backup_operation.drain(run).await;
    let points = s.store.node_backups_for_run(run).await?;
    let mut locations = points
        .iter()
        .filter(|p| in_s3(p))
        .cloned()
        .collect::<Vec<_>>();
    locations.extend(local_locations(s, run).await?);

    let mut deleted = HashSet::new();
    for location in locations {
        let storage = storage_for(s, &location)?;
        if deleted.insert((storage.endpoint.clone(), storage.bucket.clone())) {
            queue_deletion(s, &location, &format!("node-backups/{run}/"), true).await?;
        }
    }
    let directory = root(s, run);
    if directory.exists() {
        tokio::fs::remove_dir_all(directory).await?;
    }
    // Old installations may still hold metadata from the removed periodic audit.
    s.store.delete(&format!("node-backup-audit:{run}")).await?;

    let run = run.to_owned();
    s.store
        .transaction(move |db| {
            shared_blocks::release_run(db, &run, &HashSet::new())?;
            for point in points {
                db.remove("node-backups", text(&point, "id"))?;
            }
            for grant in db.list("node-disk-grants")? {
                if grant["runId"] == run {
                    db.remove("node-disk-grants", text(&grant, "id"))?;
                }
            }
            Ok(())
        })
        .await
}

/// S3 locations known only from local upload intents and block receipts.
async fn local_locations(s: &Service, run: &str) -> Result<Vec<Value>> {
    let mut locations = Vec::new();
    let directory = root(s, run);
    if directory.exists() {
        let mut entries = tokio::fs::read_dir(&directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let Some(id) = intent_id(&entry) else {
                continue;
            };
            let point = read_intent(&entry.path(), &id, run, "purge").await?;
            if in_s3(&point) {
                locations.push(point);
            }
        }
    }
    let blocks = directory.join("blocks");
    if blocks.exists() {
        let mut entries = tokio::fs::read_dir(&blocks).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some((_, bucket)) = name.split_once(".s3-") {
                locations.push(receipt_location(&entry.path(), bucket).await?);
            }
        }
    }
    Ok(locations)
}

#[cfg(test)]
mod capture_policy_tests {
    use super::*;

    #[test]
    fn final_requested_and_moving_captures_keep_filesystem_consistency() {
        let mut run = json!({ "backup": { "snapshotId": "previous" } });
        assert_eq!(snapshot_request(&run, false)["consistency"], "crash");
        assert_eq!(snapshot_request(&run, true)["consistency"], "filesystem");

        run["backup"]["requestedRevision"] = 2.into();
        run["backup"]["acknowledgedRevision"] = 1.into();
        assert_eq!(snapshot_request(&run, false)["consistency"], "filesystem");

        run["backup"]["acknowledgedRevision"] = 2.into();
        assert_eq!(snapshot_request(&run, false)["consistency"], "crash");
        run["moveRequest"] = json!({ "idle": false });
        assert_eq!(snapshot_request(&run, false)["consistency"], "filesystem");
        assert_eq!(snapshot_request(&run, false)["baseline"], "previous");
    }
}
