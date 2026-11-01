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
use serde::Serialize;
use serde_json::{Value, json};

/// How long a new reservation stays valid before its execution must renew it.
const RESERVATION_LEASE_MS: i64 = 60_000;
/// Remote nodes must have sent a heartbeat this recently to receive work.
const FRESH_HEARTBEAT_MS: i64 = 30_000;
/// Disk kept aside for a new on-demand journal or a pending attempt.
const JOURNAL_MIB: u64 = 128;

pub(crate) use super::attempts::{Attempt, AttemptRole};

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

/// The node and capacity chosen for an attempt.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Placement<'a> {
    node_id: &'a Value,
    resources: &'a Value,
    runtime_id: &'a Value,
    native_mode: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    lease_expires_at: Option<i64>,
}

/// A run's placement constraints, resolved against its agent and checkpoint.
struct Request<'a> {
    run: &'a Value,
    access: &'a Value,
    checkpoint: &'a Value,
    moving: bool,
}

impl<'a> Request<'a> {
    fn new(run: &'a Value, access: &'a Value, checkpoint: &'a Value) -> Self {
        Self {
            run,
            access,
            checkpoint,
            moving: run["placementTransition"] == true,
        }
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
        let only = |field: &str| run[field].as_str().is_none_or(|only| only == id);
        // An existing environment stays where it is unless it is moving.
        let existing = self.moving || self.checkpoint["nodeId"].as_str().is_none_or(|p| p == id);
        let local_unconfigured = node["local"] == true && !configured;
        // Development's shared process is not an available VM controller.
        let automatic_needs_controller = policy(&run["snapshot"]["agent"])["backend"] == "auto"
            && (crate::service::isolated(&run["snapshot"]["agent"])
                || self.access["nodes"].is_null()
                || self.access["nodes"]
                    .as_array()
                    .is_some_and(|nodes| nodes.iter().any(|node| node != LOCAL_NODE_ID)));
        let native = crate::native::policy::node_native(node);
        let backend = if native { "native" } else { "firecracker" };
        if !crate::native::policy::supports(&self.run["snapshot"]["agent"], backend)
            || !matches!(self.access["backend"].as_str(), Some("auto") | None)
                && self.access["backend"] != backend
            || native && node["capabilities"]["ublk"] != true
        {
            return false;
        }
        let same_version = local_unconfigured || node["runtimeId"] == super::component_runtime();
        let ready = same_version
            && ((native
                && node["executionReady"] == true
                && super::seen_within(node, FRESH_HEARTBEAT_MS))
                || local_unconfigured
                || (node["capabilities"]["kvm"] == true
                    && node["executionReady"] == true
                    && super::seen_within(node, FRESH_HEARTBEAT_MS)));
        !node["maintenance"].is_string()
            && !node["exclusiveReservation"].is_object()
            && !(local_unconfigured && (run["isolated"] == true || automatic_needs_controller))
            && has_tags
            && only("targetNodeId")
            && allowed(&self.access["nodes"], id)
            && only("pinnedNodeId")
            && existing
            && node["revoked"] != true
            && node["accepting"] == true
            && ready
    }

    /// Native negotiated allowances reserve capacity; VM ceilings retain their policy.
    fn resources_for(&self, node: &Value) -> Option<Resources> {
        let limits: Resources = serde_json::from_value(node["limits"].clone()).ok()?;
        let disk = self.run["resources"]["diskMiB"]
            .as_u64()
            .or(self.checkpoint["resources"]["diskMiB"].as_u64())
            .unwrap_or_else(|| {
                let usable = if node["diskGrowthProtocol"] == 1 {
                    limits.disk_mi_b.saturating_sub(
                        super::disk_capacity::pool_reserve(
                            limits.disk_mi_b.saturating_mul(1_048_576),
                        ) / 1_048_576
                            + JOURNAL_MIB,
                    ) / 4
                        * 4
                } else {
                    limits.disk_mi_b
                };
                defaults().disk_mi_b.min(usable)
            });
        Some(Resources {
            cpu: limits.cpu.min(32),
            memory_mi_b: limits.memory_mi_b,
            disk_mi_b: disk,
        })
    }
}

/// Current reservations across all nodes.
struct Fleet<'a> {
    attempts: &'a [Attempt],
    volumes: &'a [Value],
}

impl Fleet<'_> {
    fn active_on<'b>(&'b self, node: &'b str) -> impl Iterator<Item = &'b Attempt> {
        self.attempts
            .iter()
            .filter(move |a| a.node_id == node && is_active_attempt(a))
    }

    /// Each live execution or pending destination owns one slot, atomically.
    fn free_slots(&self, request: &Request, node: &Value) -> u64 {
        let used = self
            .active_on(text(node, "id"))
            .filter(|attempt| !(request.moving && attempt.run_id == request.run["id"]))
            .count() as u64;
        let capacity = super::slots(node).min(node["runtimeSlots"].as_u64().unwrap_or(u64::MAX));
        capacity.saturating_sub(used)
    }

    fn fit(&self, request: &Request, node: &Value, resources: &Resources) -> Result<Option<f64>> {
        let free = self.free_slots(request, node);
        let storage_policy = crate::storage::policy::for_node(&node["storage"])?;
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
        if node["diskGrowthProtocol"] == 1 && crate::native::policy::node_native(node) {
            return super::disk_capacity::admission_fits(
                node,
                self.attempts,
                request.run_id(),
                resources.disk_mi_b.saturating_mul(1_048_576),
                !request.replacing(id),
            );
        }
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
    priority: usize,
    headroom: f64,
    node: Value,
    resources: Resources,
}

impl Candidate {
    /// Automatic placement spreads work by free-slot fraction; a preferred node wins.
    fn beats(&self, other: &Self) -> bool {
        (self.preferred, std::cmp::Reverse(self.priority))
            > (other.preferred, std::cmp::Reverse(other.priority))
            || self.preferred == other.preferred
                && self.priority == other.priority
                && self.headroom > other.headroom
    }
}

/// Select within current grants and preserve the location of an existing environment.
pub async fn reserve(s: &Service, run: &Value, attempt: &str) -> Result<Value> {
    reserve_authorized(s, run, attempt, super::moves::Authorization::default()).await
}

pub(crate) async fn reserve_authorized(
    s: &Service,
    run: &Value,
    attempt: &str,
    authorize: super::moves::Authorization,
) -> Result<Value> {
    // Placement falls back to the last recorded local runner state.
    let _ = super::refresh_local(s).await;
    authorize.recheck(s).await?;
    let run = run.clone();
    let attempt = attempt.to_owned();
    let configured = !s.config.runner_url.is_empty();
    s.store
        .transaction(move |db| {
            authorize.check(db)?;
            let result = match reserve_in(db, &run, &attempt, configured) {
                Ok(placement) => Ok(placement),
                Err(error) if is_no_capacity(&error) => Err(error),
                Err(error) => return Err(error),
            };
            if result.as_ref().is_err_and(is_no_capacity) {
                request_reclamation(db, &run, configured)?;
            }
            // A failed transaction rolls back its pressure signal as well.
            // Return the decision as data so the independent signal commits.
            Ok(result)
        })
        .await?
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
        "capabilities": { "kvm": configured },
        "runtimeId": "local",
    })
}

/// What a placement decision reads, from one consistent database view.
struct Snapshot {
    access: Value,
    priorities: Vec<Value>,
    checkpoint: Value,
    nodes: Vec<Value>,
    attempts: Vec<Attempt>,
    volumes: Vec<Value>,
}

impl Snapshot {
    fn load(db: &Db<'_>, run: &Value, configured: bool) -> Result<Self> {
        let agent = db
            .agent_snapshot(super::run_agent(run))?
            .ok_or_else(|| Error::forbidden("Agent removed."))?;
        let mut nodes = db.nodes_boundary()?;
        // Existing development execution stays available without a controller.
        if !configured && nodes.iter().all(|n| n["id"] != LOCAL_NODE_ID) {
            nodes.push(development_node(configured));
        }
        nodes.retain(|node| {
            !crate::native::policy::node_native(node)
                || crate::native::policy::allows(db, run, node).unwrap_or(false)
        });
        // Ties keep the preferred node, then the current runner.
        nodes.sort_by_key(|node| {
            (
                node["id"] != run["preferredNodeId"],
                node["id"] != LOCAL_NODE_ID,
            )
        });
        Ok(Self {
            access: policy(&agent),
            priorities: agent["nodePriorities"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            checkpoint: super::db_checkpoint(db, text(run, "id"))?,
            nodes,
            attempts: db.attempts()?,
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
                priority: self
                    .priorities
                    .iter()
                    .position(|id| id == &node["id"])
                    .unwrap_or(usize::MAX),
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
    // Moves deliberately supply a prospective environment. Automatic admission
    // reads the latest persisted run so stale callers cannot rebind its backend.
    let persisted;
    let run = if run["placementTransition"] == true
        || policy(&run["snapshot"]["agent"])["backend"] != "auto"
    {
        run
    } else {
        persisted = db
            .run(text(run, "id"))?
            .ok_or_else(|| Error::not_found("Conversation removed."))?;
        &persisted
    };
    let snapshot = Snapshot::load(db, run, configured)?;
    let request = Request::new(run, &snapshot.access, &snapshot.checkpoint);
    if !request.moving
        && let Some(reservation) = run["moveReservation"].as_str()
    {
        return claim_reservation(
            db,
            &request,
            &snapshot.nodes,
            reservation,
            attempt,
            configured,
        );
    }
    let best = snapshot.select(&request, configured)?;
    admit(db, &request, &best, &snapshot.attempts, attempt)
}

fn request_reclamation(db: &Db<'_>, run: &Value, configured: bool) -> Result<()> {
    let snapshot = Snapshot::load(db, run, configured)?;
    let request = Request::new(run, &snapshot.access, &snapshot.checkpoint);
    for node in &snapshot.nodes {
        if node["diskGrowthProtocol"] == 1
            && request.eligible(node, configured)
            && snapshot.fleet().free_slots(&request, node) > 0
            && request.resources_for(node).is_some_and(|resources| {
                !super::disk_capacity::admission_fits(
                    node,
                    &snapshot.attempts,
                    request.run_id(),
                    resources.disk_mi_b.saturating_mul(1_048_576),
                    !request.replacing(text(node, "id")),
                )
            })
        {
            super::eviction::request(db, node)?;
        }
    }
    Ok(())
}

/// Growth leaves enough capacity for the oldest eligible queued conversation.
/// This is a transaction-local ceiling, not an allocation or an authority grant.
pub(super) fn waiting_disk_bytes(db: &Db<'_>, node: &Value, attempts: &[Attempt]) -> Result<u64> {
    let mut runs = db.queued_run_records()?;
    runs.sort_by(|left, right| {
        left.state
            .created_at
            .cmp(&right.state.created_at)
            .then_with(|| left.state.id.cmp(&right.state.id))
    });
    for record in runs {
        let state = &record.state;
        if record.boundary["isolated"] != true
            || state.cancel_requested_at.is_some()
            || record.boundary["backendMigration"].is_object()
            || attempts
                .iter()
                .any(|attempt| attempt.run_id == state.id && super::is_active_attempt(attempt))
            || crate::run_retry::Retry::waiting(state.retry.as_ref(), crate::config::now())
            || crate::conversation_lifecycle::require_active_run(db, &state.id).is_err()
        {
            continue;
        }
        let run = record.wire()?;
        let agent = db
            .agent_snapshot(super::run_agent(&run))?
            .unwrap_or_default();
        if !crate::native::policy::allows(db, &run, node)? {
            continue;
        }
        let access = policy(&agent);
        let checkpoint = super::db_checkpoint(db, text(&run, "id"))?;
        if checkpoint["capacityRetryAt"]
            .as_i64()
            .is_some_and(|at| at > crate::config::now())
        {
            continue;
        }
        let request = Request::new(&run, &access, &checkpoint);
        let fleet = Fleet {
            attempts,
            volumes: &[],
        };
        if !request.eligible(node, true) || fleet.free_slots(&request, node) == 0 {
            continue;
        }
        let Some(resources) = request.resources_for(node) else {
            continue;
        };
        return Ok(super::disk_capacity::admission_bytes(
            node,
            attempts,
            request.run_id(),
            resources.disk_mi_b.saturating_mul(1_048_576),
            !request.replacing(text(node, "id")),
        ));
    }
    Ok(0)
}

/// Fails like [`reserve`] without holding capacity; disk pressure can schedule
/// verification and reclamation of synchronized inactive local caches.
pub async fn check(s: &Service, run: &Value) -> Result<()> {
    let _ = super::refresh_local(s).await;
    let run = run.clone();
    let configured = !s.config.runner_url.is_empty();
    s.store
        .transaction(move |db| {
            let snapshot = Snapshot::load(db, &run, configured)?;
            let request = Request::new(&run, &snapshot.access, &snapshot.checkpoint);
            if !request.moving && run["moveReservation"].is_string() {
                return Ok(Ok(()));
            }
            match snapshot.select(&request, configured) {
                Ok(_) => Ok(Ok(())),
                Err(error) if is_no_capacity(&error) => {
                    request_reclamation(db, &run, configured)?;
                    Ok(Err(error))
                }
                Err(error) => Err(error),
            }
        })
        .await?
}

/// A move reserved destination capacity in advance; the new execution takes it over.
fn claim_reservation(
    db: &Db<'_>,
    request: &Request,
    nodes: &[Value],
    reservation: &str,
    attempt: &str,
    configured: bool,
) -> Result<Value> {
    let mut held = db
        .attempt(reservation)?
        .ok_or_else(|| Error::conflict("Movement reservation is missing."))?;
    let node = held.node_id.clone();
    let record = nodes
        .iter()
        .find(|n| n["id"] == node.as_str())
        .ok_or_else(|| Error::conflict("Destination removed."))?;
    let local_unconfigured = record["local"] == true && !configured;
    if !local_unconfigured && record["runtimeId"] != super::component_runtime() {
        return Err(Error::conflict(
            "Destination version mismatch; deploy all components at the same version.",
        ));
    }
    if !local_unconfigured
        && (record["executionReady"] != true || !super::seen_within(record, FRESH_HEARTBEAT_MS))
    {
        return Err(Error::unavailable(
            "Destination execution is not ready; restore a healthy controller with matching component versions.",
        ));
    }
    let native = crate::native::policy::native(&request.run["snapshot"]["agent"]);
    if native != crate::native::policy::node_native(record)
        || native && record["capabilities"]["ublk"] != true
    {
        return Err(Error::conflict("Destination execution backend changed."));
    }
    if record["maintenance"].is_string() {
        return Err(Error::unavailable(
            "Destination is preparing for maintenance.",
        ));
    }
    let revoked = held.released
        || held.run_id != request.run["id"]
        || !allowed(&request.access["nodes"], &node)
        || record["revoked"] == true;
    if revoked {
        return Err(Error::conflict("Movement reservation was revoked."));
    }
    let mut consumed = held.clone();
    consumed.released = true;
    db.save_attempt(&consumed)?;
    held.id = attempt.into();
    held.role = AttemptRole::Execution;
    held.runtime_id = text(record, "runtimeId").into();
    held.lease_expires_at = Some(now() + RESERVATION_LEASE_MS);
    db.save_attempt(&held)?;
    db.update_run_placement(
        request.run_id(),
        crate::nodes::run_placement::PlacementChanges {
            move_reservation: Some(None),
            ..Default::default()
        },
    )?;
    let placement = Placement {
        native_mode: serde_json::to_value(&held.native_mode)?,
        node_id: &held.node_id.clone().into(),
        resources: &serde_json::to_value(&held.resources)?,
        runtime_id: &held.runtime_id.clone().into(),
        lease_expires_at: None,
    };
    Ok(serde_json::to_value(placement)?)
}

/// Records the attempt and grows the run's volume on the chosen node.
fn admit(
    db: &Db<'_>,
    request: &Request,
    best: &Candidate,
    attempts: &[Attempt],
    attempt: &str,
) -> Result<Value> {
    let run = request.run;
    let id = text(&best.node, "id");
    let still_owned = attempts.iter().any(|a| {
        a.run_id == run["id"]
            && is_active_attempt(a)
            && (!request.moving || a.role == AttemptRole::Destination)
    });
    if still_owned {
        return Err(Error::conflict(
            "Previous execution still owns this conversation.",
        ));
    }
    let runtime_id = text(&best.node, "runtimeId");
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
    let record = Attempt {
        id: attempt.into(),
        role: if request.moving {
            AttemptRole::Destination
        } else {
            AttemptRole::Execution
        },
        run_id: request.run_id().into(),
        node_id: id.into(),
        resources: best.resources.clone(),
        runtime_id: runtime_id.into(),
        created_at: now(),
        lease_expires_at: Some(lease_expires_at),
        released: false,
        native_mode: serde_json::from_value(super::native_mode::requested(&best.node))?,
        additional_disk_mi_b: disk.saturating_sub(previous),
        disk_materialized: false,
        released_at: None,
        lease_required: false,
        lease_duration_ms: None,
    };
    db.save_attempt(&record)?;
    db.put("node-volumes", &volume)?;
    if policy(&run["snapshot"]["agent"])["backend"] == "auto" {
        let mut snapshot = run["snapshot"].clone();
        let backend = if crate::native::policy::node_native(&best.node) {
            "native"
        } else {
            "firecracker"
        };
        snapshot["agent"]["access"]["backend"] = backend.into();
        snapshot["agent"]["access"]["nodes"] = request.access["nodes"].clone();
        db.change_run(
            request.run_id(),
            crate::runs::RunChanges {
                snapshot: Some(serde_json::from_value(snapshot)?),
                ..Default::default()
            },
        )?;
        db.update_run_placement(
            request.run_id(),
            crate::nodes::run_placement::PlacementChanges {
                execution_backend: Some(serde_json::from_value(serde_json::json!(backend))?),
                ..Default::default()
            },
        )?;
    }
    let placement = Placement {
        native_mode: super::native_mode::requested(&best.node),
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

/// Initial automatic admission precedes workspace preparation. Keep its capacity
/// until materialization hands the attempt over to the ordinary controller lease.
pub(crate) async fn renew_preparation(s: &Service, run: &str, attempt: &str) -> Result<()> {
    let (run, attempt) = (run.to_owned(), attempt.to_owned());
    s.store
        .transaction(move |db| {
            let Some(mut held) = db.attempt(&attempt)? else {
                return Ok(());
            };
            if held.run_id != run || held.released || held.disk_materialized {
                return Ok(());
            }
            let current = db.run(&run)?.unwrap_or_default();
            let agent = db
                .agent_snapshot(super::run_agent(&current))?
                .unwrap_or_default();
            let node = db.node_boundary(&held.node_id)?.unwrap_or_default();
            if current["status"] != RunStatus::Running
                || !current["cancelRequestedAt"].is_null()
                || !super::agent_allows(&agent, &held.node_id)
                || node["revoked"] == true
                || crate::native::policy::node_native(&node)
                    && !crate::native::policy::allows(db, &current, &node)?
            {
                return Ok(());
            }
            held.lease_expires_at = Some(now() + RESERVATION_LEASE_MS);
            db.save_attempt(&held)?;
            Ok(())
        })
        .await
}

pub(crate) fn release_in(db: &Db<'_>, attempt: &str) -> Result<()> {
    let Some(mut record) = db.attempt(attempt)? else {
        return Ok(());
    };
    if !is_active_attempt(&record) {
        return Ok(());
    }
    record.released = true;
    record.released_at = Some(now());
    db.save_attempt(&record)?;
    let volume_id = format!("{}:{}", record.run_id, record.node_id);
    let Some(mut volume) = db.get("node-volumes", &volume_id)? else {
        return Ok(());
    };
    if !record.disk_materialized {
        volume["diskMiB"] = volume["diskMiB"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(record.additional_disk_mi_b)
            .into();
    }
    let unused = volume["materialized"] != true
        && !db.attempts()?.iter().any(|a| {
            a.run_id == record.run_id && a.node_id == record.node_id && is_active_attempt(a)
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
                .attempt(&attempt)?
                .ok_or_else(|| Error::conflict("Missing disk allocation."))?;
            if record.released {
                return Err(Error::conflict("Disk reservation was released."));
            }
            record.disk_materialized = true;
            db.save_attempt(&record)?;
            let id = format!("{}:{}", record.run_id, record.node_id);
            let mut volume = db
                .get("node-volumes", &id)?
                .ok_or_else(|| Error::conflict("Missing disk allocation."))?;
            volume["materialized"] = true.into();
            volume["activeDiskMiB"] = record.resources.disk_mi_b.into();
            db.put("node-volumes", &volume)?;
            Ok(())
        })
        .await
}

/// Owner controls still obey the selected agent's current node grants.
pub async fn configure(s: &Service, run: &str, input: Option<Value>) -> Result<Value> {
    configure_authorized(s, run, input, |_| Ok(())).await
}

pub(crate) async fn configure_authorized<F>(
    s: &Service,
    run: &str,
    input: Option<Value>,
    authorize: F,
) -> Result<Value>
where
    F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
{
    crate::validation::uuid(run)?;
    let run = run.to_owned();
    s.store
        .transaction(move |db| {
            authorize(db)?;
            configure_in(db, &run, input)
        })
        .await
}

fn configure_in(db: &Db<'_>, run: &str, input: Option<Value>) -> Result<Value> {
    let mut record = db
        .run(run)?
        .ok_or_else(|| Error::not_found("Conversation not found."))?;
    let agent = db
        .agent_snapshot(super::run_agent(&record))?
        .ok_or_else(|| Error::forbidden("Agent removed."))?;
    let access = policy(&agent);
    if let Some(input) = input {
        let selection: cairn_contracts::PlacementSelection = serde_json::from_value(input)
            .map_err(|_| Error::bad("Invalid placement selection."))?;
        for node in [&selection.pinned_node_id, &selection.preferred_node_id]
            .into_iter()
            .flatten()
        {
            let node = node.to_string();
            let unauthorized = !allowed(&access["nodes"], &node)
                || (node != LOCAL_NODE_ID
                    && db
                        .node_boundary(&node)?
                        .is_none_or(|n| n["revoked"] == true));
            if unauthorized {
                return Err(Error::forbidden(
                    "This node is not authorized for the conversation's agent.",
                ));
            }
        }
        if record["moveRequest"].is_object() || record["moveReservation"].is_string() {
            return Err(Error::conflict("Wait for the current movement to finish."));
        }
        record = db.update_run_placement(
            run,
            super::run_placement::PlacementChanges {
                pinned_node_id: Some(selection.pinned_node_id.map(|id| id.to_string())),
                preferred_node_id: Some(selection.preferred_node_id.map(|id| id.to_string())),
                ..Default::default()
            },
        )?;
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
        .nodes_boundary()?
        .into_iter()
        .filter(|n| n["revoked"] != true && allowed(&access["nodes"], text(n, "id")))
        .map(super::public_boundary)
        .collect::<Result<Vec<_>>>()?;
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
                .agent_snapshot(super::run_agent(&run))?
                .unwrap_or_default();
            let mut record = db
                .attempt(&owned)?
                .ok_or_else(|| Error::conflict("Attempt removed."))?;
            if run["status"] != RunStatus::Running
                || !run["cancelRequestedAt"].is_null()
                || record.released
                || !super::agent_allows(&agent, LOCAL_NODE_ID)
            {
                return Err(Error::conflict("Execution no longer authorized."));
            }
            record.lease_required = true;
            record.lease_duration_ms = Some(record.lease_duration_ms.unwrap_or(0).max(lease_ms));
            db.save_attempt(&record)?;
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

#[cfg(test)]
use crate::attempt_fixture::*;

#[cfg(test)]
use crate::run_fixture::DbRunFixture;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_fixture::DbNodeFixture;

    #[tokio::test]
    async fn move_reservations_require_the_current_node_and_adopt_its_runtime() {
        for backend in ["native", "firecracker"] {
            let root = tempfile::tempdir().unwrap();
            let store = crate::store::Store::open(root.path()).unwrap();
            let run_id = crate::config::id();
            let held_id = crate::config::id();
            let attempt = crate::config::id();
            let node_id = crate::config::id();
            store.transaction(move |db| {
                db.put_node_fixture(&json!({ "id":node_id,"capabilities":{"backend":backend,"ublk":true} }))?;
                let run = json!({
                    "id": run_id, "taskId": crate::config::id(), "createdAt": now(),
                    "status": "queued", "moveReservation": held_id,
                    "snapshot": { "agent": { "provider": "codex", "access": { "backend": backend } } },
                });
                db.add_run_fixture(&run, None)?;
                db.put_attempt_fixture(&json!({
                    "id": held_id, "runId": run_id, "nodeId": node_id,
                    "role": "destination", "runtimeId": "previous-build", "released": false,
                    "resources": { "cpu": 1, "memoryMiB": 512, "diskMiB": 1024 },
                    "nativeMode": { "mode": "protected", "generation": null },
                }))?;
                let access = json!({ "nodes": [node_id] });
                let checkpoint = Value::Null;
                let request = Request::new(&run, &access, &checkpoint);
                let mut node = json!({
                    "id": node_id, "runtimeId": "previous-build", "executionReady": true,
                    "capabilities": { "backend": backend, "ublk": true },
                });
                let error = claim_reservation(db, &request, &[node.clone()], &held_id, &attempt, true).unwrap_err();
                assert!(error.message.contains("version mismatch"));
                assert_eq!(db.attempt_fixture(&held_id)?.unwrap()["released"], false);
                assert!(db.attempt_fixture(&attempt)?.is_none());
                assert_eq!(db.run(&run_id)?.unwrap()["moveReservation"], held_id);

                node["runtimeId"] = super::super::component_runtime().into();
                node["lastSeen"] = (now() - FRESH_HEARTBEAT_MS).into();
                assert!(claim_reservation(db, &request, &[node.clone()], &held_id, &attempt, true).is_err());
                assert_eq!(db.attempt_fixture(&held_id)?.unwrap()["released"], false);
                assert!(db.attempt_fixture(&attempt)?.is_none());
                node["lastSeen"] = now().into();
                let placement = claim_reservation(db, &request, &[node], &held_id, &attempt, true)?;
                assert_eq!(placement["runtimeId"], super::super::component_runtime());
                assert_eq!(db.attempt_fixture(&attempt)?.unwrap()["runtimeId"], placement["runtimeId"]);
                assert_eq!(db.attempt_fixture(&held_id)?.unwrap()["released"], true);
                assert!(db.run(&run_id)?.unwrap()["moveReservation"].is_null());
                Ok(())
            }).await.unwrap();
        }
    }
}
