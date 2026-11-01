//! Publish the current disk to S3, then collect unreachable generations.
//! Publications share immutable authenticated objects.
use super::{shared_blocks, snapshots};
use crate::performance::publication::{self as diagnostic, Stage};
use crate::{
    config::{id, now},
    error::{Error, Result},
    object_storage::Storage,
    performance::Operation,
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::task::{AbortHandle, JoinSet};
use tracing::Instrument;

const MIB: u64 = 1024 * 1024;

const DEFAULT_BUDGET_MIB: u64 = 102_400;

const CLEANUP_INTERVAL_MS: i64 = 60_000;

const S3: &str = "s3";

/// Current block format: shared immutable objects.
const SHARED_V1: &str = "shared-v1";
const DIRECT_PUBLICATIONS: usize = 16;
// Refill freed global PUT capacity without an unbounded control/producer queue.
// The terminal batch still waits for every earlier receipt to settle.
const DIRECT_BATCH_WINDOW: usize = 2;
const MAX_DIRECT_BATCH_WINDOW: usize = 2;

mod direct_reader;
mod direct_upload;

#[cfg(test)]
mod pipeline_tests;

fn legacy_publication(run: &Value) -> bool {
    run["backup"]["id"].is_string() && run["backup"]["blockEnvelope"] != 3
}

/// Running executions and stopped on-demand disks with unpublished writes or an
/// unfinished publication, including a mission parked before demand was saved.
// These fields live in the physical boundary. Filtering the full wire projection
// would rebuild its frozen snapshot for each predicate, including rejected rows.
pub(super) const SYNC_CANDIDATES: &str =
    "SELECT data FROM run_views AS runs WHERE status='running'
        OR json_extract(boundary,'$.backup.requestedRevision') > COALESCE(json_extract(boundary,'$.backup.acknowledgedRevision'),0)
        OR (status IN ('succeeded','failed','cancelled','interrupted','queued','waiting')
             AND json_extract(boundary,'$.storage.mode')='on-demand'
             AND (json_extract(boundary,'$.storage.dirtyBytes')>0
                  OR json_extract(boundary,'$.backup.status') IN ('pending','saving','error')))
        OR (json_extract(boundary,'$.storage.mode')='on-demand'
            AND json_extract(boundary,'$.backup.id') IS NOT NULL
            AND COALESCE(json_extract(boundary,'$.backup.blockEnvelope'),2)<>3)";

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

fn manifest_key(run: &str, point: &str) -> String {
    format!("node-backups/{run}/{point}.json")
}

/// Whether a publication (or receipt location) is stored in S3.
pub(crate) fn in_s3(point: &Value) -> bool {
    point["destination"] == S3
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
    capture_with(s, run, CaptureProtocol::Direct).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureProtocol {
    Direct,
    LegacyControllerDrain,
}

/// Select the old controller's publication protocol before capture or transfer.
/// This compatibility operation exists only while draining a node for upgrade.
pub(crate) async fn capture_for_update(s: &Service, run: &Value, node: &str) -> Result<Value> {
    let response = s
        .http
        .get(format!("{}/health", super::controller_url(s, node)))
        .bearer_auth(super::runner_secret(s).await?)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| {
            Error::unavailable(
                "Unable to identify the controller's storage protocol before update.",
            )
        })?;
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Controller health is required before update publication.",
        ));
    }
    let bytes = snapshots::response_block(response).await?;
    let health: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::unavailable("Invalid controller health response."))?;
    if health["status"] != "ok" || health["nodeProtocol"] != 2 {
        return Err(Error::unavailable(
            "Unsupported controller protocol during update.",
        ));
    }
    let protocol = match health["directStorageProtocol"].as_u64() {
        Some(1) => CaptureProtocol::Direct,
        None if health["directStorageProtocol"].is_null() => CaptureProtocol::LegacyControllerDrain,
        _ => {
            return Err(Error::unavailable(
                "Unsupported direct storage protocol during update.",
            ));
        }
    };
    capture_with(s, run, protocol).await
}

async fn capture_with(s: &Service, run: &Value, protocol: CaptureProtocol) -> Result<Value> {
    // Cancellation of an HTTP caller must not drop the operation lock while its
    // PUTs are still executing. The owned task finishes publication or recovery
    // bookkeeping; process death leaves durable reservations for the next boot.
    let (s, run) = (s.clone(), run.clone());
    tokio::spawn(async move { capture_owned(&s, &run, protocol).await })
        .await
        .map_err(Error::internal)?
}

async fn capture_owned(s: &Service, run: &Value, protocol: CaptureProtocol) -> Result<Value> {
    let publication_attempt = id();
    let span = tracing::info_span!(
        target: "cairn_performance",
        "disk_publication_attempt",
        publication_attempt,
        run_id = crate::performance::identity(text(run, "id"))
    );
    let result = Box::pin(publish(s, run, &publication_attempt, protocol))
        .instrument(span)
        .await;
    tracing::info!(
        target: "cairn_performance",
        operation = "s3_totals",
        publication_attempt,
        counter_scope = "process_cumulative",
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

pub(crate) async fn forget_baseline(s: &Service, run_id: &str) -> Result<()> {
    update_status(s, run_id, json!({ "snapshotId": null })).await
}

/// One publication of a node snapshot, from block upload to the run's new pointer.
struct Publication<'a> {
    s: &'a Service,
    run: &'a Value,
    run_id: &'a str,
    attempt: &'a str,
    publication_attempt: &'a str,
    node: &'a Value,
    storage: &'a Storage,
    base: &'a str,
    credential: &'a str,
    snapshot: &'a Value,
    snapshot_id: &'a str,
    budget: u64,
    block_envelope: u64,
}

/// Revalidated in the same transaction as capability issuance, object promotion
/// or pointer publication; async checks alone leave a revocation race.
struct PublicationOwner {
    run: String,
    attempt: String,
    node: Value,
    on_demand: bool,
    grant: Option<String>,
}

impl PublicationOwner {
    fn validate(&self, db: &crate::store::Db<'_>) -> Result<Value> {
        let current = super::db_checkpoint(db, &self.run)?;
        if current["nodeId"] != self.node
            || (!self.on_demand && current["runnerId"] != self.attempt)
        {
            return Err(Error::conflict("Disk owner changed during publication."));
        }
        let run = crate::error::required(db.run(&self.run)?, "Conversation missing.")?;
        if self.on_demand {
            let authorized = match &self.grant {
                Some(id) => super::disk_grants::authorize_id_db(db, id)?.is_some_and(|grant| {
                    grant["runId"] == self.run && grant["nodeId"] == self.node
                }),
                None => false,
            };
            if !authorized {
                return Err(Error::conflict("Disk owner changed during publication."));
            }
        } else {
            super::disk_grants::authorize_owner(
                db,
                &run,
                self.node
                    .as_str()
                    .ok_or_else(|| Error::bad("Missing publication node."))?,
            )?;
        }
        Ok(run)
    }
}

/// The durable upload intent of a publication and the shared objects it reserved.
struct Intent {
    backup_id: String,
    path: PathBuf,
    value: Value,
    manifest: Value,
    objects: Vec<shared_blocks::Object>,
}

async fn publish(
    s: &Service,
    run: &Value,
    publication_attempt: &str,
    protocol: CaptureProtocol,
) -> Result<Value> {
    let run_id = text(run, "id");
    let mut timing = Operation::new("disk_publication", run_id, "queue");
    crate::validation::uuid(run_id)?;
    let _operation = s.node_backup_operation.lock(run_id).await;
    super::journal_acknowledgements::recover(s, run_id).await?;
    // A queued demand may have outlived its originating turn. Capture the live
    // attempt, not a stale stopped-run endpoint or publication revision.
    let current_run = s.store.run(run_id).await?;
    let run = &current_run;
    if run["environmentState"].is_string() {
        return Err(Error::conflict("This working environment has expired."));
    }

    timing.next("collect_before");
    let checkpoint = super::checkpoint(s, run_id).await?;
    let attempt = text(&checkpoint, "runnerId");
    crate::validation::uuid(attempt)?;
    let settings = settings(s).await?;
    let previous_point = match run["backup"]["id"].as_str() {
        Some(id) => s.store.get("node-backups", id).await?,
        None => None,
    };
    let storage = match previous_point.as_ref() {
        Some(point) if in_s3(point) => storage_for(s, point)?,
        _ => Storage::configured(s)?,
    };
    collect_unused(s, run_id).await?;

    timing.next("snapshot");
    let base = super::transport::url(s, run_id).await?;
    let credential = super::runner_secret(s).await?;
    let snapshot = request_snapshot(s, run, &base, attempt, &credential, protocol).await?;

    if snapshot["alreadyPublished"] == true {
        let point = s
            .get("node-backups", text(&snapshot["published"], "backupId"))
            .await?;
        let publication = text(&point, "id").to_owned();
        s.store
            .read(move |db| shared_blocks::verified(db, &publication))
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
        publication_attempt,
        node: &checkpoint["nodeId"],
        storage: &storage,
        base: &base,
        credential: &credential,
        snapshot: &snapshot,
        snapshot_id,
        budget: budget_bytes(&settings),
        block_envelope: if protocol == CaptureProtocol::Direct {
            3
        } else {
            2
        },
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
    protocol: CaptureProtocol,
) -> Result<Value> {
    let run_id = text(run, "id");
    let changing_envelope = match protocol {
        CaptureProtocol::Direct => legacy_publication(run),
        CaptureProtocol::LegacyControllerDrain => run["backup"]["blockEnvelope"] == 3,
    };
    if requested(run) && !changing_envelope {
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
        if response.status().as_u16() != 412 {
            return Err(Error::unavailable(
                "Unable to capture the completed disk generation.",
            ));
        }
        // A current turn stopped before recording a completed generation still
        // needs a coherent capture of its retained disk.
    }
    let stopped = protocol == CaptureProtocol::LegacyControllerDrain
        || run["status"] != RunStatus::Running
        || run["moveRequest"]["idle"] == true;
    let mut capture = snapshot_request(run, stopped);
    if protocol == CaptureProtocol::LegacyControllerDrain {
        capture["baseline"] = if changing_envelope {
            Value::Null
        } else {
            run["backup"]["snapshotId"].clone()
        };
    }
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
        .json(&capture)
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
        "baseline": if legacy_publication(run) { Value::Null } else { run["backup"]["snapshotId"].clone() },
        "consistency": if periodic { "crash" } else { "filesystem" }
    })
}

impl Publication<'_> {
    fn owner(&self) -> PublicationOwner {
        PublicationOwner {
            run: self.run_id.to_owned(),
            attempt: self.attempt.to_owned(),
            node: self.node.clone(),
            on_demand: self.snapshot["manifest"]["onDemand"] == true,
            grant: self.snapshot["grantId"].as_str().map(str::to_owned),
        }
    }

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
        let integrity = s.publication_integrity.read().await;
        self.commit(&intent, uploaded).await?;

        timing.next("acknowledge_journal");
        if intent.manifest["onDemand"] == true {
            self.acknowledge(&intent.value, &intent.manifest).await?;
            self.complete().await?;
            // Completed runs leave the active-run monitor. Read back the final
            // journal counters so their UI does not retain an old dirty count.
            let _ = super::storage::refresh(s, self.run).await;
        }
        drop(integrity);

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
        // Conversion images and old-controller update drains use explicit v2.
        // Ordinary VM/native captures always publish directly from nodes.
        if self.block_envelope == 3 {
            value["blockEnvelope"] = 3.into();
            manifest["blockEnvelope"] = 3.into();
        }

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
        let metrics = diagnostic::Metrics::new(
            self.publication_attempt,
            self.run_id,
            self.snapshot_id,
            "manager",
        );
        self.transfer_observed(&metrics, manifest, objects).await
    }

    async fn transfer_observed(
        &self,
        metrics: &Arc<diagnostic::Metrics>,
        manifest: &Value,
        objects: &[shared_blocks::Object],
    ) -> Result<u64> {
        self.observe_transfer(
            metrics,
            objects,
            Box::pin(self.transfer_operations(manifest, objects)),
        )
        .await
    }

    async fn observe_transfer(
        &self,
        metrics: &Arc<diagnostic::Metrics>,
        objects: &[shared_blocks::Object],
        operation: impl std::future::Future<Output = Result<u64>>,
    ) -> Result<u64> {
        metrics.inventory(
            objects.len() as u64,
            objects.iter().filter(|object| object.ready).count() as u64,
            objects
                .iter()
                .filter(|object| !object.ready)
                .map(|object| object.size)
                .sum(),
        );
        let _registration = diagnostic::register(metrics);
        let mut summary = diagnostic::Summary {
            metrics: metrics.clone(),
            outcome: "cancelled",
        };
        let result = diagnostic::scope(Some(metrics.clone()), operation).await;
        summary.outcome = if result.is_ok() { "success" } else { "error" };
        result
    }

    async fn transfer_converted(
        &self,
        disk: &Path,
        manifest: &Value,
        objects: &[shared_blocks::Object],
    ) -> Result<u64> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut file = tokio::fs::File::open(disk).await?;
        let mut uploaded = 0;
        for object in objects.iter().filter(|object| !object.ready) {
            let read = diagnostic::start(Stage::ConvertedRead);
            let block = snapshots::find_block(manifest, &object.hash)
                .ok_or_else(|| Error::bad("Converted block missing."))?;
            file.seek(std::io::SeekFrom::Start(block["offset"].as_u64().unwrap()))
                .await?;
            let mut bytes = vec![0; object.size as usize];
            file.read_exact(&mut bytes).await?;
            read.finish(bytes.len());
            if !crate::storage::digest::matches(&object.hash, &bytes) {
                return Err(Error::bad("Converted disk changed during publication."));
            }
            let scope = shared_blocks::key(&object.hash, &object.id);
            self.storage
                .upload_bytes(encrypt_block(self.s, scope.clone(), bytes).await?, &scope)
                .await?;
            uploaded += object.size;
        }
        Ok(uploaded)
    }

    async fn transfer_operations(
        &self,
        manifest: &Value,
        objects: &[shared_blocks::Object],
    ) -> Result<u64> {
        if manifest["blockEnvelope"] == 3 {
            return self.transfer_direct(objects).await;
        }
        let _legacy_transfer = self.s.node_backup_operation.transfer().await;
        // The common all-reused path never loads other manifests. Incremental
        // snapshots may omit old blocks, which migrate directly from S3.
        let inventory = diagnostic::start(Stage::Inventory);
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
        )
        .diagnostics(self.snapshot["publicationDiagnostics"] == true);

        if self.node == super::LOCAL_NODE_ID {
            reads = reads.direct_publication();
        }

        inventory.finish(0);
        let mut uploads = JoinSet::<Result<()>>::new();
        let transfer = self
            .start_uploads(objects, &sources, &mut reads, &mut uploads)
            .await;
        // On failure, finish every already-started PUT before the operation lock
        // can be released and its pending references retired by another capture.
        let mut result = transfer;
        let drain = diagnostic::start(Stage::UploadDrain);
        while let Some(upload) = uploads.join_next().await {
            if let Err(error) = upload.map_err(Error::internal).and_then(|upload| upload) {
                result = Err(error);
            }
        }
        drain.finish(0);
        let uploaded = result?;

        tracing::info!(
            target: "cairn_performance",
            operation = "disk_publication",
            id = self.run_id,
            publication_attempt = self.publication_attempt,
            uploaded_bytes = uploaded,
            referenced_blocks = objects.len(),
            reused_blocks = objects.iter().filter(|o| o.ready).count()
        );
        Ok(uploaded)
    }

    async fn transfer_direct(&self, objects: &[shared_blocks::Object]) -> Result<u64> {
        self.transfer_direct_window(objects, DIRECT_BATCH_WINDOW)
            .await
    }

    async fn transfer_direct_window(
        &self,
        objects: &[shared_blocks::Object],
        window: usize,
    ) -> Result<u64> {
        use crate::storage::direct::BATCH;
        if !(1..=MAX_DIRECT_BATCH_WINDOW).contains(&window) {
            return Err(Error::bad("Invalid direct publication window."));
        }
        let missing = objects
            .iter()
            .filter(|object| !object.ready)
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(0);
        }
        self.storage.require_direct().await?;
        let mut reader = direct_reader::Lease::open(self).await?;
        let mut uploaded = 0;
        let mut uploads = JoinSet::<Result<u64>>::new();
        let mut failure = None;
        let batches = missing.chunks(BATCH);
        let batch_count = batches.len();
        for (index, batch) in batches.enumerate() {
            let terminal = index + 1 == batch_count;
            // The terminal request retains its existing quiescent-summary
            // contract. Ordinary requests may reuse freed global PUT permits.
            while !uploads.is_empty() && (uploads.len() >= window || terminal) {
                match direct_upload::complete_one(&mut uploads).await {
                    Ok(bytes) => uploaded += bytes,
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            if failure.is_some() {
                break;
            }
            let prepared = match reader.check() {
                Ok(()) => direct_upload::Prepared::new(self, batch, terminal).await,
                Err(error) => Err(error),
            };
            match prepared {
                Ok(prepared) => {
                    uploads.spawn(diagnostic::scope(diagnostic::current(), prepared.upload()));
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        // Stop admitting after an observed failure and join every control task.
        // HTTP failure can precede a detached node PUT's completion: unsettled
        // grants and the node reader retain that ambiguous work. Success below
        // still requires explicit reader/physical-owner close acknowledgement.
        let drain = diagnostic::start(Stage::UploadDrain);
        while !uploads.is_empty() {
            match direct_upload::complete_one(&mut uploads).await {
                Ok(bytes) => uploaded += bytes,
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        drain.finish(0);
        if let Some(error) = failure {
            return Err(error);
        }
        reader.close(self).await?;
        tracing::info!(target: "cairn_performance", operation = "disk_publication", id = self.run_id, transport = "direct", uploaded_bytes = uploaded, referenced_blocks = objects.len(), reused_blocks = objects.iter().filter(|object| object.ready).count());
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
            let metrics = diagnostic::current();
            uploads.spawn(diagnostic::scope(metrics, async move {
                destination.upload_bytes(encoded, &scope).await
            }));
            uploaded += object.size;
            if uploads.len() >= crate::object_storage::HOT_WRITE_CONCURRENCY {
                let wait = diagnostic::start(Stage::UploadBackpressure);
                uploads
                    .join_next()
                    .await
                    .unwrap()
                    .map_err(Error::internal)??;
                wait.finish(0);
            }
        }
        Ok(uploaded)
    }

    /// Settles duplicate uploads, then writes the final manifest locally and to S3.
    async fn publish_manifest(&self, intent: &mut Intent, uploaded: u64) -> Result<()> {
        let s = self.s;
        let publication = intent.backup_id.clone();
        // Offline backend conversions have their own migration ownership fence.
        let owner = (!self.base.is_empty()).then(|| self.owner());
        let mut manifest = std::mem::take(&mut intent.manifest);
        intent.manifest = s
            .store
            .transaction(move |db| {
                if let Some(owner) = owner {
                    owner.validate(db)?;
                }
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
        let owner = self.owner();
        let point = intent.value.clone();
        let acknowledge = self.snapshot["manifest"]["onDemand"] == true;
        self.s
            .store
            .transaction(move |db| {
                let current_run = owner.validate(db)?;
                shared_blocks::verified(db, text(&point, "id"))?;
                if acknowledge && point["blockEnvelope"] == 3 {
                    super::journal_acknowledgements::begin(db, &point)?;
                }
                db.put("node-backups", &point)?;
                let mut backup = current_run["backup"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                backup.extend(patch["backup"].as_object().unwrap().clone());
                backup.insert("blockEnvelope".into(), point["blockEnvelope"].clone());
                db.update_run_boundary(&owner.run, &json!({ "backup": backup }))?;
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
        super::journal_acknowledgements::settle(self.s, text(point, "id")).await?;
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
            db.update_run_boundary(&run, &json!({ "backup": backup }))?;
            Ok(())
        })
        .await?;
    s.node_publication_notify.notify_one();
    Ok(())
}

async fn encrypt_block(s: &Service, scope: String, bytes: Vec<u8>) -> Result<Vec<u8>> {
    s.hot_s3
        .block_codec
        .encode(s.vault.clone(), scope, bytes)
        .await
}

/// Publish a converted, offline environment without replacing the source pointer.
/// The migration commits its checkpoint and this point together after restoration.
pub(crate) async fn converted_environment(
    s: &Service,
    run: &Value,
    node: &Value,
    disk: &Path,
    manifest: Value,
) -> Result<Value> {
    let publication_attempt = id();
    let span = tracing::info_span!(
        target: "cairn_performance",
        "disk_publication_attempt",
        publication_attempt,
        run_id = crate::performance::identity(text(run, "id")),
        source = "converted_environment"
    );
    Box::pin(converted_environment_owned(
        s,
        run,
        node,
        disk,
        manifest,
        &publication_attempt,
    ))
    .instrument(span)
    .await
}

async fn converted_environment_owned(
    s: &Service,
    run: &Value,
    node: &Value,
    disk: &Path,
    manifest: Value,
    publication_attempt: &str,
) -> Result<Value> {
    let run_id = text(run, "id");
    let mut timing = Operation::new(
        "disk_publication",
        crate::performance::identity(run_id),
        "queue",
    );
    let _operation = s.node_backup_operation.lock(run_id).await;
    timing.next("prepare_upload");
    let storage = Storage::configured(s)?;
    let snapshot_id = id();
    let snapshot = json!({ "manifest": manifest });
    let publication = Publication {
        s,
        run,
        run_id,
        publication_attempt,
        attempt: "",
        node: &node["id"],
        storage: &storage,
        base: "",
        credential: "",
        snapshot: &snapshot,
        snapshot_id: &snapshot_id,
        budget: budget_bytes(&settings(s).await?),
        block_envelope: 2,
    };
    let directory = root(s, run_id);
    crate::skills::private_dir(&directory.join("blocks")).await?;
    let mut intent = publication
        .write_intent(&directory, snapshot["manifest"].clone())
        .await?;
    timing.next("transfer_blocks");
    let metrics = diagnostic::Metrics::new(publication_attempt, run_id, &snapshot_id, "manager");
    let uploaded = publication
        .observe_transfer(
            &metrics,
            &intent.objects,
            Box::pin(publication.transfer_converted(disk, &intent.manifest, &intent.objects)),
        )
        .await?;
    timing.next("publish_manifest");
    publication.publish_manifest(&mut intent, uploaded).await?;
    let (owner, migration, point) = (
        run_id.to_owned(),
        run["backendMigration"]["id"].clone(),
        intent.value.clone(),
    );
    s.store
        .transaction(move |db| {
            let current = db
                .run(&owner)?
                .ok_or_else(|| Error::not_found("Conversation removed."))?;
            if current["backendMigration"]["id"] != migration {
                return Err(Error::conflict(
                    "Backend migration changed during publication.",
                ));
            }
            shared_blocks::verified(db, text(&point, "id"))?;
            db.put("node-backups", &point)?;
            let mut migration = current["backendMigration"].clone();
            migration["convertedBackup"] = point["id"].clone();
            db.update_run_boundary(&owner, &json!({ "backendMigration": migration }))?;
            Ok(())
        })
        .await?;
    timing.finish();
    Ok(intent.value)
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
        let sample = diagnostic::start(Stage::BaseRead);
        let bytes = read_manifest_block(s, point, block).await?;
        sample.finish(bytes.len());
        bytes
    } else {
        reads.block(&object.hash).await?
    };
    if bytes.len() as u64 != object.size || !crate::storage::digest::matches(&object.hash, &bytes) {
        return Err(Error::bad("Backup block failed integrity verification."));
    }
    Ok(bytes)
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
    if backup["blockFormat"] != SHARED_V1 {
        return Err(Error::bad("Unsupported disk block format."));
    }
    {
        for block in value["blocks"].as_array().into_iter().flatten() {
            if block["hash"].is_string() {
                crate::validation::uuid(text(block, "object"))?;
            }
        }
    }
    Ok(value)
}

pub async fn read_block(s: &Service, backup: &Value, hash: &str) -> Result<Vec<u8>> {
    if !snapshots::valid_hash(hash) {
        return Err(Error::bad("Invalid backup block."));
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
    let object = text(block, "object");
    crate::validation::uuid(object)?;
    if !snapshots::valid_hash(hash) {
        return Err(Error::bad("Invalid backup block."));
    }

    let envelope = block["envelope"].as_u64().unwrap_or(2);
    let scope = shared_blocks::object_key(envelope, hash, object);
    let storage = storage_for(s, backup)?;
    let result = async {
        let permit = s.hot_s3.block_codec.read_permit().await?;
        let encoded = storage
            .download_bytes(&scope, crate::storage::block_codec::MAX_ENCODED_BYTES)
            .await?;
        let size = block["size"]
            .as_u64()
            .ok_or_else(|| Error::conflict("Recovery block size mismatch."))?;
        let bytes = if envelope == 3 {
            // Explicit compatibility/conversion callers only. Ordinary node reads
            // receive a direct ticket in restore::handle, never these payloads.
            let key = s.vault.storage_object_key(&scope);
            let (scope, hash) = (scope.clone(), hash.to_owned());
            diagnostic::blocking(move || {
                let _permit = permit;
                crate::storage::block_codec::decode_direct(&key, &scope, &hash, size, &encoded)
                    .map_err(|_| Error::conflict("Recovery block integrity check failed."))
            })
            .await
            .map_err(Error::internal)??
        } else {
            decode_scoped_block(s, &scope, hash, Some(size), encoded, permit).await?
        };
        Ok(bytes)
    }
    .await;
    if result
        .as_ref()
        .is_err_and(|error: &Error| matches!(error.status, 400 | 409))
    {
        if envelope == 3 {
            // Conversion readers observe the same shared v3 dependency as a
            // direct node. Fence every ambiguous ACK before invalidating it.
            super::journal_acknowledgements::quarantine(s, object).await?;
        } else {
            shared_blocks::invalidate(s, object).await?;
            forget_baseline(s, text(backup, "runId")).await?;
        }
    }
    result
}

async fn decode_scoped_block(
    s: &Service,
    scope: &str,
    hash: &str,
    expected_size: Option<u64>,
    encoded: Vec<u8>,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<Vec<u8>> {
    s.hot_s3
        .block_codec
        .decode_reserved(
            s.vault.clone(),
            scope.to_owned(),
            hash.to_owned(),
            expected_size,
            encoded,
            permit,
        )
        .await
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
        .node_boundary(super::LOCAL_NODE_ID)
        .await?
        .unwrap_or_default();
    let policy = crate::storage::policy::for_node(&node["storage"])?;
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

/// Publications a run still depends on.
struct Retained {
    points: Vec<Value>,
    keep: HashSet<String>,
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

    Ok(Some(Retained { points, keep }))
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
            let keys = vec![manifest_key(run, text(&point, "id"))];
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

    // Nothing to save yet.
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
            db.update_run_boundary(&run, &json!({ "backup": backup }))?;
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
            db.update_run_boundary(&run, &json!({ "backup": status }))?;
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
    let recovery = s.clone();
    let reconciliation = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = recovery.shutdown.cancelled() => return,
                result = super::journal_acknowledgements::resume(&recovery) => {
                    if let Err(error) = result {
                        tracing::warn!(message = %error.message, "Storage quarantine reconciliation will retry");
                    }
                }
            }
            tokio::select! {
                () = recovery.shutdown.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    });
    let mut scheduler = Scheduler {
        s,
        last: HashMap::new(),
        failures: HashMap::new(),
        last_cleanup: 0,
        tasks: JoinSet::new(),
        active: HashMap::new(),
        cleanup: HashSet::new(),
        migrations: HashSet::new(),
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
    reconciliation.abort();
}

/// Background publications and cleanups, bounded by the synchronization slots and
/// never more than one task per conversation.
struct Scheduler {
    s: Arc<Service>,
    last: HashMap<String, i64>,
    /// Consecutive failed synchronizations per conversation, for back-off.
    failures: HashMap<String, u32>,
    last_cleanup: i64,
    tasks: JoinSet<()>,
    active: HashMap<String, AbortHandle>,
    cleanup: HashSet<String>,
    migrations: HashSet<String>,
}

impl Scheduler {
    fn reap(&mut self) {
        while self.tasks.try_join_next().is_some() {}
        self.active.retain(|_, task| !task.is_finished());
        self.migrations.retain(|run| self.active.contains_key(run));
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
            if self.tasks.len() >= DIRECT_PUBLICATIONS {
                break;
            }
            let id = text(&run, "id");
            let migration =
                legacy_publication(&run) && run["storage"]["dirtyBytes"] == 0 && !requested(&run);
            // Background conversion is progressive and never fans out over all
            // inactive disks. Foreground publications keep their own capacity.
            if migration && !self.migrations.is_empty() {
                continue;
            }
            let since = now() - self.last.get(id).copied().unwrap_or(0);
            let failures = if run["backup"]["status"] == BackupStatus::Error.as_str() {
                self.failures
                    .get(id)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1)
            } else {
                0
            };
            if self.active.contains_key(id) || !publication_due(&run, since, interval, failures) {
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
            if failures == 0 {
                self.failures.remove(&run_id);
            } else {
                self.failures.insert(run_id.clone(), failures);
            }
            self.cleanup.remove(&run_id);
            if migration {
                self.migrations.insert(run_id.clone());
            }
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

/// Longest wait between synchronization attempts that keep failing.
const MAX_RETRY_DELAY_MS: i64 = 30 * 60 * 1000;

/// Minimum wait after `failures` consecutive failed synchronizations.
fn retry_delay_ms(failures: u32) -> i64 {
    if failures == 0 {
        return 0;
    }
    60_000_i64
        .saturating_mul(1 << (failures - 1).min(16))
        .min(MAX_RETRY_DELAY_MS)
}

/// An isolated conversation publishes periodically while it has something new.
fn publication_due(
    run: &Value,
    since_last_ms: i64,
    default_interval_ms: i64,
    failures: u32,
) -> bool {
    let on_demand = run["storage"]["mode"] == super::ON_DEMAND;
    let synchronized = on_demand
        && !legacy_publication(run)
        && run["storage"]["dirtyBytes"] == 0
        && run["backup"]["status"] == BackupStatus::Ready.as_str();
    let interval = run["storage"]["backupSeconds"]
        .as_i64()
        .map_or(default_interval_ms, |seconds| seconds * 1000);
    let requested = run["backup"]["requestedRevision"].as_u64().unwrap_or(0);
    let acknowledged = run["backup"]["acknowledgedRevision"].as_u64().unwrap_or(0);
    protected(run)
        && (run["sessionId"].is_string() || on_demand || requested > acknowledged)
        && (!run["moveRequest"].is_object() || requested > acknowledged)
        && !synchronized
        && since_last_ms
            >= if requested > acknowledged || run["storage"]["backupUrgent"] == true {
                5000
            } else {
                interval
            }
            .max(retry_delay_ms(failures))
}

pub(crate) fn storage_for(s: &Service, backup: &Value) -> Result<Storage> {
    if !in_s3(backup) {
        return Err(Error::unavailable("Local recovery block is missing."));
    }
    let mut storage = Storage::for_location(s, backup)?;
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
    retire_environment(s, run, true).await
}

pub(crate) async fn expire_environment(s: &Service, run: &str) -> Result<()> {
    retire_environment(s, run, false).await
}

async fn retire_environment(s: &Service, run: &str, permanently_deleted: bool) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    crate::validation::uuid(run)?;
    let _operation = s.node_backup_operation.lock(run).await;
    let _readers = s.node_backup_operation.drain(run).await;
    super::journal_acknowledgements::retire(s, run).await?;
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
        if !permanently_deleted {
            // The same run can publish a new environment later. A delayed prefix
            // deletion would erase its new manifests, so retire exact old keys only.
            queue_deletion(
                s,
                &location,
                &manifest_key(run, text(&location, "id")),
                false,
            )
            .await?;
        } else if deleted.insert((storage.endpoint.clone(), storage.bucket.clone())) {
            queue_deletion(s, &location, &format!("node-backups/{run}/"), true).await?;
        }
    }
    let directory = root(s, run);
    if directory.exists() {
        tokio::fs::remove_dir_all(directory).await?;
    }

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
    Ok(locations)
}

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod capture_policy_tests {
    use super::*;
    use crate::{agent_fixture::StoreAgentFixture, node_fixture::StoreNodeFixture};

    #[tokio::test]
    async fn publication_commit_rechecks_revocation_and_agent_grants_after_upload() {
        let root = tempfile::tempdir().unwrap();
        let service = fixture_service(root.path()).await;
        std::fs::write(
            root.path().join("storage-s3.json"),
            br#"{"bucket":"fixture-never-contacted"}"#,
        )
        .unwrap();
        let storage = Storage::configured(&service).unwrap();
        let node = id();
        service
            .store
            .put_node_fixture(json!({ "id": node, "revoked": false }))
            .await
            .unwrap();
        for on_demand in [false, true] {
            service
                .store
                .put_agent_fixture(
                    json!({ "id": crate::config::MAIN_AGENT_ID, "access": { "nodes": [node] } }),
                )
                .await
                .unwrap();
            service
                .store
                .put_node_fixture(json!({ "id": node, "revoked": false }))
                .await
                .unwrap();
            let run_id = id();
            let attempt = id();
            let grant = id();
            let (run, checkpoint_node, checkpoint_attempt, grant_id) =
                (run_id.clone(), node.clone(), attempt.clone(), grant.clone());
            let run = service
                .store
                .transaction(move |db| {
                    let record = db.add_run_fixture(
                        &json!({
                            "id": run,
                            "taskId": run,
                            "status": "succeeded",
                            "createdAt": 0,
                            "nodeId": checkpoint_node,
                            "snapshot": { "agent": { "id": crate::config::MAIN_AGENT_ID } }
                        }),
                        None,
                    )?;
                    db.save_checkpoint_boundary(
                        &run,
                        &json!({
                            "nodeId": checkpoint_node,
                            "runnerId": checkpoint_attempt
                        }),
                    )?;
                    db.put(
                        "node-disk-grants",
                        &json!({
                            "id": grant_id,
                            "runId": run,
                            "nodeId": checkpoint_node,
                            "backups": []
                        }),
                    )?;
                    db.run(record["id"].as_str().unwrap())?
                        .ok_or_else(|| Error::bad("Fixture run missing."))
                })
                .await
                .unwrap();
            let snapshot_id = id();
            let snapshot = json!({ "manifest": { "onDemand": on_demand }, "grantId": grant });
            let node_value = json!(node);
            let publication = Publication {
                publication_attempt: &snapshot_id,
                s: &service,
                run: &run,
                run_id: &run_id,
                attempt: &attempt,
                node: &node_value,
                storage: &storage,
                base: "http://unused",
                credential: "fixture",
                snapshot: &snapshot,
                snapshot_id: &snapshot_id,
                budget: 0,
                block_envelope: 3,
            };
            let point = id();
            let mut intent = Intent {
                backup_id: point.clone(),
                path: root.path().join("unused"),
                value: json!({
                    "id": point,
                    "runId": run_id,
                    "nodeId": node,
                    "snapshotId": snapshot_id,
                    "blockEnvelope": 3
                }),
                manifest: json!({ "capturedAt": 1, "blocks": [] }),
                objects: Vec::new(),
            };
            let (point, mut manifest) = (intent.value.clone(), intent.manifest.clone());
            service
                .store
                .transaction(move |db| shared_blocks::reserve(db, &point, &mut manifest))
                .await
                .unwrap();
            publication.commit(&intent, 0).await.unwrap();
            let previous = service.store.run(&run_id).await.unwrap()["backup"]["id"].clone();
            // Model a revocation after all data/manifest I/O has completed.
            service
                .store
                .put_node_fixture(json!({ "id": node, "revoked": true }))
                .await
                .unwrap();
            intent.value["id"] = id().into();
            assert_eq!(
                publication.commit(&intent, 0).await.unwrap_err().status,
                403
            );
            assert_eq!(
                service.store.run(&run_id).await.unwrap()["backup"]["id"],
                previous
            );
            service
                .store
                .put_node_fixture(json!({ "id": node, "revoked": false }))
                .await
                .unwrap();
            service
                .store
                .put_agent_fixture(
                    json!({ "id": crate::config::MAIN_AGENT_ID, "access": { "nodes": [] } }),
                )
                .await
                .unwrap();
            assert_eq!(
                publication.commit(&intent, 0).await.unwrap_err().status,
                403
            );
            assert_eq!(
                service.store.run(&run_id).await.unwrap()["backup"]["id"],
                previous
            );
        }
    }

    async fn fixture_service(root: &Path) -> Arc<Service> {
        Service::new(crate::config::Config {
            data_dir: root.to_owned(),
            home: root.join("home"),
            workspace_roots: vec![root.to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            opencode_bin: "opencode".into(),
            opencode_go_usage_url: crate::accounts::opencode::USAGE_URL.into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn expiration_never_queues_a_prefix_that_can_erase_the_next_environment() {
        let root = tempfile::tempdir().unwrap();
        let service = fixture_service(root.path()).await;
        tokio::fs::write(
            root.path().join("storage-s3.json"),
            br#"{"bucket":"retention-test","region":"us-east-1"}"#,
        )
        .await
        .unwrap();
        let run = id();
        let old = id();
        service
            .store
            .put(
                "node-backups",
                json!({
                    "id": old, "runId": run, "destination": "s3", "bucket": "retention-test",
                }),
            )
            .await
            .unwrap();
        expire_environment(&service, &run).await.unwrap();
        let next = id();
        service
            .store
            .put(
                "node-backups",
                json!({
                    "id": next, "runId": run, "destination": "s3", "bucket": "retention-test",
                }),
            )
            .await
            .unwrap();
        let old_key = manifest_key(&run, &old);
        service
            .store
            .read(move |db| {
                let mut statement = db.0.prepare("SELECT key,prefix FROM remote_deletions")?;
                let queued = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                assert_eq!(queued, vec![(old_key, false)]);
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            service
                .store
                .get("node-backups", &next)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn storage_pressure_recovers_dirty_finished_runs_without_pending_demand() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            for status in ["succeeded", "failed", "cancelled", "interrupted"] {
                db.add_run_fixture(&json!({
                    "id": status,
                    "taskId": status,
                    "status": status,
                    "createdAt": 0,
                    "isolated": true,
                    "storage": { "mode": "on-demand", "dirtyBytes": 4096 },
                    "backup": { "status": "error", "requestedRevision": 1, "acknowledgedRevision": 1 }
                }), None)?;
            }
            Ok(())
        }).await.unwrap();
        let candidates = store
            .read(|db| db.json_rows(SYNC_CANDIDATES, []))
            .await
            .unwrap();
        assert_eq!(
            candidates.len(),
            4,
            "All finished journals must remain eligible for backup and reclamation"
        );
    }

    #[tokio::test]
    async fn waiting_mission_dirty_publication_is_recovered_after_a_crash_before_demand() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store.transaction(|db| {
            for (id, dirty, backup) in [("dirty", 4096, "synced"), ("pending", 0, "pending"), ("error", 0, "error"), ("clean", 0, "synced")] {
                db.add_run_fixture(&json!({
                    "id": id, "taskId": id, "status": "waiting", "createdAt": 0,
                    "isolated": true, "storage": { "mode": "on-demand", "dirtyBytes": dirty },
                    "backup": { "status": backup, "requestedRevision": 1, "acknowledgedRevision": 1 },
                    "wakeup": { "reason": "Wait for CI", "wakeAt": 9007199254740991i64 },
                }), None)?;
            }
            Ok(())
        }).await.unwrap();
        drop(store);
        // Restart sees the committed waiting state without outstanding demand.
        let restarted = crate::store::Store::open(root.path()).unwrap();
        let mut candidates = restarted
            .read(|db| db.json_rows(SYNC_CANDIDATES, []))
            .await
            .unwrap()
            .into_iter()
            .map(|run| text(&run, "id").to_owned())
            .collect::<Vec<_>>();
        candidates.sort();
        assert_eq!(candidates, ["dirty", "error", "pending"]);
    }

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
        run["backup"]["id"] = "legacy-point".into();
        assert!(
            snapshot_request(&run, false)["baseline"].is_null(),
            "legacy migration needs a full capture, including unchanged v2 blocks"
        );
        run["backup"]["blockEnvelope"] = 3.into();
        assert_eq!(snapshot_request(&run, false)["baseline"], "previous");
    }

    #[test]
    fn requested_native_work_retries_without_an_engine_session() {
        for status in ["failed", "cancelled", "queued"] {
            let mut run = json!({
                "isolated": true,
                "status": status,
                "storage": { "mode": "native", "dirtyBytes": 4096 },
                "backup": {
                    "requestedRevision": 1,
                    "acknowledgedRevision": 0,
                    "status": "error",
                },
            });
            assert!(!publication_due(&run, 4999, 60_000, 0));
            assert!(publication_due(&run, 5000, 60_000, 0), "{status}");
            assert!(!publication_due(&run, 5000, 60_000, 1));
            assert!(publication_due(&run, retry_delay_ms(1), 60_000, 1));
            run["backup"]["acknowledgedRevision"] = 1.into();
            assert!(!publication_due(&run, 60_000, 60_000, 0));
        }
    }

    #[test]
    fn failing_synchronizations_back_off_instead_of_retrying_every_few_seconds() {
        let run = json!({
            "isolated": true,
            "sessionId": "session",
            "storage": { "mode": "on-demand", "dirtyBytes": 1, "backupUrgent": true },
            "backup": { "status": "error" },
        });
        assert!(publication_due(&run, 5_000, 60_000, 0));

        // Production 2026-10-05: a node whose uploads always failed was
        // retried every 90 s for 11 hours, re-reading gigabytes each time.
        assert!(!publication_due(&run, 5_000, 60_000, 1));
        assert!(publication_due(&run, retry_delay_ms(1), 60_000, 1));
        assert!(retry_delay_ms(2) > retry_delay_ms(1));
        assert_eq!(retry_delay_ms(30), MAX_RETRY_DELAY_MS);
        assert!(!publication_due(&run, MAX_RETRY_DELAY_MS - 1, 60_000, 30));
        assert!(publication_due(&run, MAX_RETRY_DELAY_MS, 60_000, 30));
    }
}

#[cfg(test)]
#[path = "publication/diagnostic_tests.rs"]
mod diagnostic_tests;

#[cfg(test)]
#[path = "publication/integrity_tests.rs"]
mod integrity_tests;

#[cfg(test)]
#[path = "publication/measurements.rs"]
mod measurements;

#[cfg(test)]
#[path = "publication/selection_tests.rs"]
mod selection_tests;
