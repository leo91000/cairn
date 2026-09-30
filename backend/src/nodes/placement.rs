//! Atomic admission and durable attempt ownership. Failed admission changes nothing.
use super::{LOCAL_NODE_ID, Resources, is_active_attempt};
use crate::{
    config::now,
    error::{Error, Result},
    run_status::RunStatus,
    service::{Service, allowed, policy},
    storage::policy::Policy,
    store::Db,
    validation::text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How long a new reservation stays valid before its execution must renew it.
const RESERVATION_LEASE_MS: i64 = 60_000;
/// Remote nodes must have sent a heartbeat this recently to receive work.
const FRESH_HEARTBEAT_MS: i64 = 30_000;
/// Disk kept aside for a new on-demand journal or a pending attempt.
const JOURNAL_MIB: u64 = 128;

/// What a `node-attempts` record reserves capacity for.
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AttemptRole {
    /// The conversation executes on the node.
    Execution,
    /// Capacity held on the node a conversation is moving to.
    Destination,
}

impl AttemptRole {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Execution => "execution",
            Self::Destination => "destination",
        }
    }
}

pub fn defaults() -> Resources {
    Resources {
        cpu: 2,
        memory_mi_b: 4096,
        disk_mi_b: 32768,
    }
}

fn disk_total(volume: &Value, requested: u64, replacing: bool) -> u64 {
    if volume.is_null() || volume["storageMode"] == super::ON_DEMAND {
        let local = volume["diskMiB"]
            .as_u64()
            .unwrap_or(JOURNAL_MIB)
            .max(JOURNAL_MIB);
        return if replacing {
            local.saturating_add(JOURNAL_MIB)
        } else {
            local
        };
    }
    let total = volume["diskMiB"].as_u64().unwrap_or(0);
    let current = volume["activeDiskMiB"].as_u64().unwrap_or(total);
    let added = if replacing {
        requested
    } else {
        requested.saturating_sub(current)
    };
    total.saturating_add(added)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NewAttempt<'a> {
    id: &'a str,
    role: AttemptRole,
    run_id: &'a Value,
    node_id: &'a str,
    resources: &'a Resources,
    runtime_id: &'a str,
    created_at: i64,
    lease_expires_at: i64,
    released: bool,
    additional_disk_mi_b: u64,
    disk_materialized: bool,
}

/// The node and capacity chosen for an attempt.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Placement<'a> {
    node_id: &'a Value,
    resources: &'a Value,
    runtime_id: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    lease_expires_at: Option<i64>,
}

/// A run's placement constraints, resolved against its agent and checkpoint.
struct Request<'a> {
    run: &'a Value,
    access: &'a Value,
    checkpoint: &'a Value,
    moving: bool,
    required_runtime: Option<&'a str>,
}

impl<'a> Request<'a> {
    fn new(run: &'a Value, access: &'a Value, checkpoint: &'a Value) -> Result<Self> {
        Ok(Self {
            run,
            access,
            checkpoint,
            moving: run["placementTransition"] == true,
            required_runtime: run["requiredRuntime"]
                .as_str()
                .or(checkpoint["runtimeId"].as_str()),
        })
    }

    fn run_id(&self) -> &'a str {
        text(self.run, "id")
    }

    /// A move to another node copies the disk beside any stale copy already there.
    fn replacing(&self, node: &str) -> bool {
        self.moving && self.checkpoint["nodeId"] != node
    }

    /// Checks that do not depend on the node's current reservations.
    fn eligible(&self, node: &Value, configured: bool) -> bool {
        let id = text(node, "id");
        let run = self.run;
        let has_tags = run["requiredTags"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|tag| super::node_tags(node).any(|present| present == tag));
        let has_runtime = self.required_runtime.is_none_or(|runtime| {
            node["runtimes"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r == runtime)
        });
        let only = |field: &str| run[field].as_str().is_none_or(|only| only == id);
        // An existing environment stays where it is unless it is moving.
        let existing = self.moving || self.checkpoint["nodeId"].as_str().is_none_or(|p| p == id);
        let local_unconfigured = node["local"] == true && !configured;
        let ready = local_unconfigured
            || (node["capabilities"]["kvm"] == true
                && node["executionReady"] == true
                && super::seen_within(node, FRESH_HEARTBEAT_MS));
        // A moved disk is mounted from its published S3 state. A node that
        // has not passed the storage probe cannot receive that disk.
        node["capabilities"]["fuse"] == true
            && !node["maintenance"].is_string()
            && !(local_unconfigured && run["isolated"] == true)
            && has_tags
            && has_runtime
            && only("targetNodeId")
            && allowed(&self.access["nodes"], id)
            && only("pinnedNodeId")
            && existing
            && node["revoked"] != true
            && node["accepting"] == true
            && ready
    }

    /// VM address-space ceilings follow the node; they never reserve host capacity.
    fn resources_for(&self, node: &Value) -> Option<Resources> {
        let limits: Resources = serde_json::from_value(node["limits"].clone()).ok()?;
        let disk = self.run["resources"]["diskMiB"]
            .as_u64()
            .or(self.checkpoint["resources"]["diskMiB"].as_u64())
            .unwrap_or(limits.disk_mi_b);
        Some(Resources {
            cpu: limits.cpu.min(32),
            memory_mi_b: limits.memory_mi_b,
            disk_mi_b: disk,
        })
    }
}

/// Current reservations across all nodes.
struct Fleet<'a> {
    attempts: &'a [Value],
    volumes: &'a [Value],
}

impl Fleet<'_> {
    fn active_on<'b>(&'b self, node: &'b str) -> impl Iterator<Item = &'b Value> {
        self.attempts
            .iter()
            .filter(move |a| a["nodeId"] == node && is_active_attempt(a))
    }

    /// Each live execution or pending destination owns one slot, atomically.
    fn free_slots(&self, request: &Request, node: &Value) -> u64 {
        let used = self
            .active_on(text(node, "id"))
            .filter(|attempt| !(request.moving && attempt["runId"] == request.run["id"]))
            .count() as u64;
        let capacity = super::slots(node).min(node["runtimeSlots"].as_u64().unwrap_or(u64::MAX));
        capacity.saturating_sub(used)
    }

    fn fit(&self, request: &Request, node: &Value, resources: &Resources) -> Result<Option<f64>> {
        let free = self.free_slots(request, node);
        let storage_policy = Policy::for_node(&node["storage"])?;
        if free == 0
            || node["pressure"].is_string()
            || !self.disk_fits(request, node, resources, &storage_policy)
        {
            return Ok(None);
        }
        Ok(Some(free as f64 / super::slots(node) as f64))
    }

    fn describe(&self, request: &Request, node: &Value, _resources: &Resources) -> Result<String> {
        let name = node["name"].as_str().unwrap_or_else(|| text(node, "id"));
        let free = self.free_slots(request, node);
        let pressure = node["pressure"].as_str().unwrap_or("shared disk space");
        Ok(format!("{name} has {free} free slots; check {pressure}"))
    }

    fn disk_fits(
        &self,
        request: &Request,
        node: &Value,
        resources: &Resources,
        storage_policy: &Policy,
    ) -> bool {
        let id = text(node, "id");
        // Nodes reporting free space keep their reserve plus room for each pending journal.
        if let Some(free) = node["capabilities"]["diskMiB"].as_u64() {
            let total = node["capabilities"]["diskTotalMiB"]
                .as_u64()
                .unwrap_or(free);
            let pending = self.active_on(id).count() as u64;
            let reserve = storage_policy.reserve(total.saturating_mul(1_048_576)) / 1_048_576;
            let budget_free = node["limits"]["diskMiB"]
                .as_u64()
                .unwrap_or(free)
                .saturating_sub(node["usage"]["diskMiB"].as_u64().unwrap_or(0));
            let journals = (pending + 1) * JOURNAL_MIB;
            return free > reserve + journals && budget_free > journals;
        }
        let run_id = &request.run["id"];
        let used = self
            .volumes
            .iter()
            .filter(|v| v["nodeId"] == id && v["runId"] != *run_id)
            .map(|v| v["diskMiB"].as_u64().unwrap_or(0))
            .sum::<u64>();
        let volume = self
            .volumes
            .iter()
            .find(|v| v["nodeId"] == id && v["runId"] == *run_id)
            .unwrap_or(&Value::Null);
        let needed = disk_total(volume, resources.disk_mi_b, request.replacing(id));
        let limit = node["limits"]["diskMiB"].as_u64().unwrap_or(0);
        used.checked_add(needed).is_some_and(|total| total <= limit)
    }
}

struct Candidate {
    preferred: bool,
    headroom: f64,
    node: Value,
    resources: Resources,
}

impl Candidate {
    /// Automatic placement spreads work by free-slot fraction; a preferred node wins.
    fn beats(&self, other: &Self) -> bool {
        (self.preferred && !other.preferred)
            || (self.preferred == other.preferred && self.headroom > other.headroom)
    }
}

/// Select within current grants and preserve the location of an existing environment.
pub async fn reserve(s: &Service, run: &Value, attempt: &str) -> Result<Value> {
    // Placement falls back to the last recorded local runner state.
    let _ = super::refresh_local(s).await;
    let run = run.clone();
    let attempt = attempt.to_owned();
    let configured = !s.config.runner_url.is_empty();
    s.store
        .transaction(move |db| reserve_in(db, &run, &attempt, configured))
        .await
}

/// Without a configured controller, development execution uses the process itself.
fn development_node(configured: bool) -> Value {
    json!({
        "id": LOCAL_NODE_ID,
        "local": true,
        "accepting": true,
        "limits": {
            "cpu": 4096,
            "memoryMiB": 1_073_741_824u64,
            "diskMiB": 1_099_511_627_776u64,
        },
        "slots": 4096,
        "capabilities": { "kvm": configured, "fuse": true },
        "runtimeId": "local",
    })
}

/// What a placement decision reads, from one consistent database view.
struct Snapshot {
    access: Value,
    checkpoint: Value,
    nodes: Vec<Value>,
    attempts: Vec<Value>,
    volumes: Vec<Value>,
}

impl Snapshot {
    fn load(db: &Db<'_>, run: &Value, configured: bool) -> Result<Self> {
        let agent = db
            .get("agents", super::run_agent(run))?
            .ok_or_else(|| Error::forbidden("Agent removed."))?;
        let mut nodes = db.list("nodes")?;
        // Existing development execution stays available without a controller.
        if !configured && nodes.iter().all(|n| n["id"] != LOCAL_NODE_ID) {
            nodes.push(development_node(configured));
        }
        // Ties keep the preferred node, then the current runner.
        nodes.sort_by_key(|node| {
            (
                node["id"] != run["preferredNodeId"],
                node["id"] != LOCAL_NODE_ID,
            )
        });
        Ok(Self {
            access: policy(&agent),
            checkpoint: super::db_checkpoint(db, text(run, "id"))?,
            nodes,
            attempts: db.list("node-attempts")?,
            volumes: db.list("node-volumes")?,
        })
    }

    fn fleet(&self) -> Fleet<'_> {
        Fleet {
            attempts: &self.attempts,
            volumes: &self.volumes,
        }
    }

    /// The best node for `request`, or why none can take it now.
    fn select(&self, request: &Request, configured: bool) -> Result<Candidate> {
        let fleet = self.fleet();
        let mut best: Option<Candidate> = None;
        let mut full = Vec::new();
        for node in &self.nodes {
            if !request.eligible(node, configured) {
                continue;
            }
            let Some(resources) = request.resources_for(node) else {
                continue;
            };
            let Some(headroom) = fleet.fit(request, node, &resources)? else {
                full.push(fleet.describe(request, node, &resources)?);
                continue;
            };
            let candidate = Candidate {
                preferred: node["id"] == request.run["preferredNodeId"],
                headroom,
                node: node.clone(),
                resources,
            };
            if best.as_ref().is_none_or(|best| candidate.beats(best)) {
                best = Some(candidate);
            }
        }
        best.ok_or_else(|| Error::unavailable(shortage(&full)))
    }
}

/// Starts every message for a conversation no authorized node can take now.
pub const NO_CAPACITY: &str = "No authorized node";

/// Whether `error` means the conversation only has to wait for a node to have room.
pub fn is_no_capacity(error: &Error) -> bool {
    error.is_unavailable() && error.message.starts_with(NO_CAPACITY)
}

fn shortage(full: &[String]) -> String {
    if full.is_empty() {
        return format!(
            "{NO_CAPACITY} is online and accepting this conversation. The existing environment is preserved."
        );
    }
    format!(
        "{NO_CAPACITY} has a free execution slot and enough shared headroom: {}. The existing environment is preserved.",
        full.join("; ")
    )
}

fn reserve_in(db: &Db<'_>, run: &Value, attempt: &str, configured: bool) -> Result<Value> {
    let snapshot = Snapshot::load(db, run, configured)?;
    let request = Request::new(run, &snapshot.access, &snapshot.checkpoint)?;
    if !request.moving
        && let Some(reservation) = run["moveReservation"].as_str()
    {
        return claim_reservation(db, &request, &snapshot.nodes, reservation, attempt);
    }
    let best = snapshot.select(&request, configured)?;
    admit(db, &request, &best, &snapshot.attempts, attempt)
}

/// Fails like [`reserve`] when no node can take `run` now, without reserving anything.
pub async fn check(s: &Service, run: &Value) -> Result<()> {
    let _ = super::refresh_local(s).await;
    let run = run.clone();
    let configured = !s.config.runner_url.is_empty();
    s.store
        .read(move |db| {
            let snapshot = Snapshot::load(db, &run, configured)?;
            let request = Request::new(&run, &snapshot.access, &snapshot.checkpoint)?;
            // A move already holds its destination.
            if !request.moving && run["moveReservation"].is_string() {
                return Ok(());
            }
            snapshot.select(&request, configured).map(drop)
        })
        .await
}

/// A move reserved destination capacity in advance; the new execution takes it over.
fn claim_reservation(
    db: &Db<'_>,
    request: &Request,
    nodes: &[Value],
    reservation: &str,
    attempt: &str,
) -> Result<Value> {
    let mut held = db
        .get("node-attempts", reservation)?
        .ok_or_else(|| Error::conflict("Movement reservation is missing."))?;
    let node = text(&held, "nodeId").to_owned();
    let record = nodes
        .iter()
        .find(|n| n["id"] == node.as_str())
        .ok_or_else(|| Error::conflict("Destination removed."))?;
    if record["capabilities"]["fuse"] != true {
        return Err(Error::conflict(
            "Destination no longer supports on-demand disks.",
        ));
    }
    if record["maintenance"].is_string() {
        return Err(Error::unavailable(
            "Destination is preparing for maintenance.",
        ));
    }
    let revoked = held["released"] == true
        || held["runId"] != request.run["id"]
        || !allowed(&request.access["nodes"], &node)
        || record["revoked"] == true;
    if revoked {
        return Err(Error::conflict("Movement reservation was revoked."));
    }
    let mut consumed = held.clone();
    consumed["released"] = true.into();
    db.put("node-attempts", &consumed)?;
    held["id"] = attempt.into();
    held["role"] = AttemptRole::Execution.as_str().into();
    held["leaseExpiresAt"] = (now() + RESERVATION_LEASE_MS).into();
    db.put("node-attempts", &held)?;
    db.patch_run(request.run_id(), &json!({ "moveReservation": null }))?;
    let placement = Placement {
        node_id: &held["nodeId"],
        resources: &held["resources"],
        runtime_id: &held["runtimeId"],
        lease_expires_at: None,
    };
    Ok(serde_json::to_value(placement)?)
}

/// Records the attempt and grows the run's volume on the chosen node.
fn admit(
    db: &Db<'_>,
    request: &Request,
    best: &Candidate,
    attempts: &[Value],
    attempt: &str,
) -> Result<Value> {
    let run = request.run;
    let id = text(&best.node, "id");
    let still_owned = attempts.iter().any(|a| {
        a["runId"] == run["id"]
            && is_active_attempt(a)
            && (!request.moving || a["role"] == AttemptRole::Destination.as_str())
    });
    if still_owned {
        return Err(Error::conflict(
            "Previous execution still owns this conversation.",
        ));
    }
    let runtime_id = request
        .required_runtime
        .unwrap_or_else(|| text(&best.node, "runtimeId"));
    let volume_id = format!("{}:{id}", request.run_id());
    let mut volume = match db.get("node-volumes", &volume_id)? {
        Some(volume) => volume,
        None => json!({
            "id": volume_id,
            "nodeId": id,
            "runId": run["id"],
            "materialized": false,
        }),
    };
    if volume["materialized"] != true {
        volume["storageMode"] = super::ON_DEMAND.into();
    }
    let previous = volume["diskMiB"].as_u64().unwrap_or(0);
    let disk = disk_total(&volume, best.resources.disk_mi_b, request.replacing(id));
    volume["diskMiB"] = disk.into();
    let lease_expires_at = now() + RESERVATION_LEASE_MS;
    let record = NewAttempt {
        id: attempt,
        role: if request.moving {
            AttemptRole::Destination
        } else {
            AttemptRole::Execution
        },
        run_id: &run["id"],
        node_id: id,
        resources: &best.resources,
        runtime_id,
        created_at: now(),
        lease_expires_at,
        released: false,
        additional_disk_mi_b: disk.saturating_sub(previous),
        disk_materialized: false,
    };
    db.put("node-attempts", &serde_json::to_value(record)?)?;
    db.put("node-volumes", &volume)?;
    let placement = Placement {
        node_id: &id.into(),
        resources: &serde_json::to_value(&best.resources)?,
        runtime_id: &runtime_id.into(),
        lease_expires_at: Some(lease_expires_at),
    };
    Ok(serde_json::to_value(placement)?)
}

pub async fn release(s: &Service, attempt: &str) -> Result<()> {
    let attempt = attempt.to_owned();
    s.store
        .transaction(move |db| release_in(db, &attempt))
        .await
}

fn release_in(db: &Db<'_>, attempt: &str) -> Result<()> {
    let Some(mut record) = db.get("node-attempts", attempt)? else {
        return Ok(());
    };
    if !is_active_attempt(&record) {
        return Ok(());
    }
    record["released"] = true.into();
    record["releasedAt"] = now().into();
    db.put("node-attempts", &record)?;
    let volume_id = format!("{}:{}", text(&record, "runId"), text(&record, "nodeId"));
    let Some(mut volume) = db.get("node-volumes", &volume_id)? else {
        return Ok(());
    };
    if record["diskMaterialized"] != true {
        volume["diskMiB"] = volume["diskMiB"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(record["additionalDiskMiB"].as_u64().unwrap_or(0))
            .into();
    }
    let unused = volume["materialized"] != true
        && !db.list("node-attempts")?.iter().any(|a| {
            a["runId"] == record["runId"] && a["nodeId"] == record["nodeId"] && is_active_attempt(a)
        });
    if unused {
        db.remove("node-volumes", &volume_id)?;
    } else {
        db.put("node-volumes", &volume)?;
    }
    Ok(())
}

/// Once a controller may have created a disk, keep its allocation until explicit disk deletion.
pub async fn materialize(s: &Service, attempt: &str) -> Result<()> {
    let attempt = attempt.to_owned();
    s.store
        .transaction(move |db| {
            let mut record = db
                .get("node-attempts", &attempt)?
                .ok_or_else(|| Error::conflict("Missing disk allocation."))?;
            if record["released"] == true {
                return Err(Error::conflict("Disk reservation was released."));
            }
            record["diskMaterialized"] = true.into();
            db.put("node-attempts", &record)?;
            let id = format!("{}:{}", text(&record, "runId"), text(&record, "nodeId"));
            let mut volume = db
                .get("node-volumes", &id)?
                .ok_or_else(|| Error::conflict("Missing disk allocation."))?;
            volume["materialized"] = true.into();
            volume["activeDiskMiB"] = record["resources"]["diskMiB"].clone();
            db.put("node-volumes", &volume)?;
            Ok(())
        })
        .await
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selection {
    pinned_node_id: Option<String>,
    preferred_node_id: Option<String>,
}

/// Owner controls still obey the selected agent's current node grants.
pub async fn configure(s: &Service, run: &str, input: Option<Value>) -> Result<Value> {
    crate::validation::uuid(run)?;
    let run = run.to_owned();
    s.store
        .transaction(move |db| configure_in(db, &run, input))
        .await
}

fn configure_in(db: &Db<'_>, run: &str, input: Option<Value>) -> Result<Value> {
    let mut record = db
        .run(run)?
        .ok_or_else(|| Error::not_found("Conversation not found."))?;
    let agent = db
        .get("agents", super::run_agent(&record))?
        .ok_or_else(|| Error::forbidden("Agent removed."))?;
    let access = policy(&agent);
    if let Some(input) = input {
        let selection: Selection = serde_json::from_value(input)
            .map_err(|_| Error::bad("Invalid placement selection."))?;
        for node in [&selection.pinned_node_id, &selection.preferred_node_id]
            .into_iter()
            .flatten()
        {
            crate::validation::uuid(node)?;
            let unauthorized = !allowed(&access["nodes"], node)
                || (node != LOCAL_NODE_ID
                    && db.get("nodes", node)?.is_none_or(|n| n["revoked"] == true));
            if unauthorized {
                return Err(Error::forbidden(
                    "This node is not authorized for the conversation's agent.",
                ));
            }
        }
        if record["moveRequest"].is_object() || record["moveReservation"].is_string() {
            return Err(Error::conflict("Wait for the current movement to finish."));
        }
        record = db.patch_run(run, &serde_json::to_value(&selection)?)?;
        db.audit(
            "node.placement.configured",
            &json!({
                "runId": run,
                "pinnedNodeId": record["pinnedNodeId"],
                "preferredNodeId": record["preferredNodeId"],
            }),
        )?;
    }
    let nodes = db
        .list("nodes")?
        .into_iter()
        .filter(|n| n["revoked"] != true && allowed(&access["nodes"], text(n, "id")))
        .map(super::public)
        .collect::<Vec<_>>();
    Ok(json!({
        "nodes": nodes,
        "pinnedNodeId": record["pinnedNodeId"],
        "preferredNodeId": record["preferredNodeId"],
    }))
}

/// The local controller follows the same expiring ownership rule as remote nodes.
pub async fn renew_local(s: &Service, run_id: &str) -> Result<()> {
    let checkpoint = super::checkpoint(s, run_id).await?;
    if checkpoint["nodeId"] != LOCAL_NODE_ID {
        return Ok(());
    }
    let Some(attempt) = checkpoint["runnerId"].as_str() else {
        return Ok(());
    };
    let lease_ms = super::publication::lease_ms(s).await?;
    let owned = attempt.to_owned();
    let run_id = run_id.to_owned();
    s.store
        .transaction(move |db| {
            let run = db
                .run(&run_id)?
                .ok_or_else(|| Error::not_found("Conversation removed."))?;
            let agent = db
                .get("agents", super::run_agent(&run))?
                .unwrap_or_default();
            let mut record = db
                .get("node-attempts", &owned)?
                .ok_or_else(|| Error::conflict("Attempt removed."))?;
            if run["status"] != RunStatus::Running
                || !run["cancelRequestedAt"].is_null()
                || record["released"] == true
                || !super::agent_allows(&agent, LOCAL_NODE_ID)
            {
                return Err(Error::conflict("Execution no longer authorized."));
            }
            record["leaseRequired"] = true.into();
            record["leaseDurationMs"] = record["leaseDurationMs"]
                .as_u64()
                .unwrap_or(0)
                .max(lease_ms)
                .into();
            db.put("node-attempts", &record)?;
            Ok(())
        })
        .await?;
    // Record an upper bound even if the acknowledgement is lost.
    super::record_lease(s, attempt, lease_ms + 3000).await;
    let response = s
        .http
        .post(format!("{}/runs/{attempt}/lease", s.config.runner_url))
        .bearer_auth(super::runner_secret(s).await?)
        .json(&json!({ "remainingMs": lease_ms }))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .map_err(|_| Error::unavailable("Local execution lease unavailable."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Local controller rejected execution lease.",
        ));
    }
    Ok(())
}
