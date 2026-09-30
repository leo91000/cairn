//! Trusted node identities. Enrollment and revocation are serialized with heartbeats.
pub mod alerts;
pub mod checkpoint;
pub mod connector;
pub mod coordination;
pub mod disk_grants;
pub mod executor;
pub mod files;
pub mod maintenance;
pub mod moves;
pub mod placement;
pub mod publication;
pub mod relay;
pub mod restore;
pub mod shared_blocks;
pub mod snapshots;
pub mod storage;
pub mod transport;
pub mod workspace;

use crate::{
    auth::{digest, token},
    config::{id, now},
    error::{Error, Result},
    http::{App, Input},
    run_status::RunStatus,
    service::Service,
    storage::policy::Policy,
    store::Db,
    validation::text,
};
use axum::{
    Json,
    extract::{Request, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

pub const LOCAL_NODE_ID: &str = "00000000-0000-4000-8000-000000000002";
const HEARTBEAT_TIMEOUT_MS: i64 = 60_000;
const HEARTBEAT_INTERVAL_MS: i64 = 10_000;
const ENROLLMENT_TTL_MS: i64 = 600_000;
/// Storage mode of journal-backed disks mounted from their published S3 state.
pub(crate) const ON_DEMAND: &str = "on-demand";

/// Execution progress of a conversation relative to its node, persisted in `run.nodeState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum NodeState {
    Pausing,
    Saving,
    Restoring,
    Resuming,
    Updating,
    WaitingForNode,
}

/// Connection state reported to owners; derived, never persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeStatus {
    Revoked,
    Local,
    Online,
    Offline,
}

impl NodeStatus {
    fn of(node: &Value) -> Self {
        if node["revoked"] == true {
            Self::Revoked
        } else if node["local"] == true {
            Self::Local
        } else if seen_within(node, HEARTBEAT_TIMEOUT_MS) {
            Self::Online
        } else {
            Self::Offline
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Local => "local",
            Self::Online => "online",
            Self::Offline => "offline",
        }
    }
}

/// Whether the node sent a heartbeat less than `max_age_ms` ago.
pub(crate) fn seen_within(node: &Value, max_age_ms: i64) -> bool {
    node["lastSeen"]
        .as_i64()
        .is_some_and(|seen| now() - seen < max_age_ms)
}

/// Fail before queueing only when no execution location is authorized.
pub fn require_node(agent: &Value) -> Result<()> {
    if crate::service::policy(agent)["nodes"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        return Err(Error::forbidden(
            "This agent has no authorized execution node.",
        ));
    }
    Ok(())
}

/// Whether the agent's current grants allow execution on `node`.
pub(crate) fn agent_allows(agent: &Value, node: &str) -> bool {
    crate::service::allowed(&crate::service::policy(agent)["nodes"], node)
}

/// The node of a run, publication or grant; records without one belong to the local runner.
pub(crate) fn owner_node(record: &Value) -> &str {
    record["nodeId"].as_str().unwrap_or(LOCAL_NODE_ID)
}

/// The id of the agent a run was started with.
pub(crate) fn run_agent(run: &Value) -> &str {
    text(&run["snapshot"]["agent"], "id")
}

pub(crate) fn checkpoint_key(run: &str) -> String {
    format!("run-checkpoint:{run}")
}

/// The run's resume checkpoint, or `null` when none was recorded.
pub(crate) async fn checkpoint(s: &Service, run: &str) -> Result<Value> {
    Ok(s.store.kv(&checkpoint_key(run)).await?.unwrap_or_default())
}

pub(crate) fn db_checkpoint(db: &Db<'_>, run: &str) -> Result<Value> {
    Ok(db.kv(&checkpoint_key(run))?.unwrap_or_default())
}

pub(crate) async fn runner_secret(s: &Service) -> Result<String> {
    crate::execution::secret(&s.config.data_dir, "runner-secret").await
}

/// The controller of `node`: the local runner, or the master's relay to a remote node.
pub(crate) fn controller_url(s: &Service, node: &str) -> String {
    if node == LOCAL_NODE_ID {
        s.config.runner_url.clone()
    } else {
        format!("{}/internal/execution/{node}", s.config.public_url)
    }
}

/// The data root shared by the master and node processes (`DATA_DIR`).
pub(crate) fn node_data_dir() -> PathBuf {
    PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".into()))
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// Tags accepted on nodes and in capacity requests.
pub(crate) fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 40
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:./".contains(&b))
}

/// Operator tags followed by detected system tags.
pub(crate) fn node_tags(node: &Value) -> impl Iterator<Item = &Value> {
    node["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(node["systemTags"].as_array().into_iter().flatten())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourceKind {
    Cpu,
    Memory,
    Disk,
}

impl ResourceKind {
    pub(crate) const ALL: [Self; 3] = [Self::Cpu, Self::Memory, Self::Disk];

    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memoryMiB",
            Self::Disk => "diskMiB",
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resources {
    pub cpu: u32,
    pub memory_mi_b: u64,
    pub disk_mi_b: u64,
}

impl Resources {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=4096).contains(&self.cpu)
            || !(128..=1_073_741_824).contains(&self.memory_mi_b)
            || !(128..=1_099_511_627_776).contains(&self.disk_mi_b)
        {
            return Err(Error::bad("Invalid node resource limits."));
        }
        Ok(())
    }

    pub(crate) fn amount(&self, kind: ResourceKind) -> u64 {
        match kind {
            ResourceKind::Cpu => u64::from(self.cpu),
            ResourceKind::Memory => self.memory_mi_b,
            ResourceKind::Disk => self.disk_mi_b,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Capabilities {
    os: String,
    arch: String,
    kvm: bool,
    #[serde(default)]
    fuse: bool,
    cpu: u32,
    memory_mi_b: u64,
    disk_mi_b: u64,
    #[serde(default)]
    disk_total_mi_b: u64,
}

impl Capabilities {
    fn validate(&self) -> Result<()> {
        if self.os != "linux" || self.arch != "x86_64" {
            return Err(Error::bad("Nodes require Linux x86-64."));
        }
        self.resources().validate()
    }

    const fn resources(&self) -> Resources {
        Resources {
            cpu: self.cpu,
            memory_mi_b: self.memory_mi_b,
            disk_mi_b: self.disk_mi_b,
        }
    }

    pub(crate) fn limits(&self) -> Resources {
        Resources {
            cpu: self.cpu.saturating_sub(1).max(1),
            memory_mi_b: self.memory_mi_b.saturating_sub(512).max(128),
            disk_mi_b: (self.disk_mi_b * 4 / 5).max(128),
        }
    }

    fn tags(&self) -> Vec<&'static str> {
        if self.kvm {
            vec!["linux", "x86_64", "kvm"]
        } else {
            vec!["linux", "x86_64"]
        }
    }
}

pub(crate) fn valid_runtime(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| Error::bad("Invalid node request."))
}

fn name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 100 || value.chars().any(char::is_control) {
        return Err(Error::bad(
            "Node name must contain 1 to 100 printable characters.",
        ));
    }
    Ok(value.into())
}

fn public(mut node: Value) -> Value {
    node["status"] = NodeStatus::of(&node).as_str().into();
    node
}

pub(crate) fn is_active_attempt(attempt: &Value) -> bool {
    attempt["released"] != true
}

/// A disk kept on a node that no conversation needs there any more.
struct StaleDisk {
    volume: Value,
    /// The whole disk is stale, not only older copies set aside beside it.
    whole: bool,
    mi_b: u64,
}

/// Disk kept on nodes that no conversation needs there any more: the whole disk of a
/// conversation now running elsewhere (it may hold changes newer than the recovery
/// point used to resume it), or older copies set aside beside a current disk.
async fn stale_disks(s: &Service, volumes: &[Value], attempts: &[Value]) -> Result<Vec<StaleDisk>> {
    let mut stale = Vec::new();
    for volume in volumes.iter().filter(|v| v["materialized"] == true) {
        let run = volume["runId"].as_str().unwrap_or_default();
        let node = volume["nodeId"].as_str().unwrap_or_default();
        let record = s.store.run(run).await.ok();
        let executing = attempts
            .iter()
            .any(|a| a["runId"] == run && a["nodeId"] == node && is_active_attempt(a));
        let moving = record
            .as_ref()
            .is_some_and(|r| r["moveRequest"].is_object() || r["moveReservation"].is_string());
        if executing || moving {
            continue;
        }
        let checkpoint = checkpoint(s, run).await?;
        let total = volume["diskMiB"].as_u64().unwrap_or(0);
        let elsewhere = checkpoint["nodeId"]
            .as_str()
            .is_some_and(|current| current != node);
        if record.is_none() || elsewhere {
            stale.push(StaleDisk {
                volume: volume.clone(),
                whole: true,
                mi_b: total,
            });
        } else if let Some(active) = volume["activeDiskMiB"].as_u64()
            && total > active
        {
            stale.push(StaleDisk {
                volume: volume.clone(),
                whole: false,
                mi_b: total - active,
            });
        }
    }
    Ok(stale)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StaleSummary {
    count: usize,
    disk_mi_b: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentGrant<'a> {
    id: &'a Value,
    name: &'a Value,
    all_nodes: bool,
}

struct Usage<'a> {
    attempts: &'a [Value],
    agents: &'a [Value],
    stale: &'a [StaleDisk],
}

impl Usage<'_> {
    fn stale_summary(&self, node: &Value) -> StaleSummary {
        let stale = self
            .stale
            .iter()
            .filter(|disk| disk.volume["nodeId"] == node["id"]);
        StaleSummary {
            count: stale.clone().count(),
            disk_mi_b: stale.map(|disk| disk.mi_b).sum(),
        }
    }

    fn granted_agents(&self, node: &Value) -> Vec<AgentGrant<'_>> {
        if node["revoked"] == true {
            return Vec::new();
        }
        let id = text(node, "id");
        self.agents
            .iter()
            .filter(|agent| agent_allows(agent, id))
            .map(|agent| AgentGrant {
                id: &agent["id"],
                name: &agent["name"],
                all_nodes: crate::service::policy(agent)["nodes"].is_null(),
            })
            .collect()
    }

    fn describe(&self, mut node: Value) -> Result<Value> {
        let used = self
            .attempts
            .iter()
            .filter(|attempt| attempt["nodeId"] == node["id"] && is_active_attempt(attempt))
            .count() as u64;
        let capacity = slots(&node);
        node["slots"] = capacity.into();
        node["occupiedSlots"] = used.into();
        node["availableSlots"] = capacity.saturating_sub(used).into();
        if let Some(object) = node.as_object_mut() {
            object.remove("reserved");
            object.remove("available");
        }
        node["staleDisks"] = serde_json::to_value(self.stale_summary(&node))?;
        node["agents"] = serde_json::to_value(self.granted_agents(&node))?;
        Ok(public(node))
    }
}

/// Legacy nodes inherit the runner's slot count, or the historical four slots.
pub(crate) fn slots(node: &Value) -> u64 {
    node["slots"]
        .as_u64()
        .or(node["runtimeSlots"].as_u64())
        .unwrap_or(4)
        .max(1)
}

/// Nodes with their shared budgets, free slots and authorized agents.
pub async fn inventory(s: &Service) -> Result<Vec<Value>> {
    let attempts = s.store.list("node-attempts").await?;
    let volumes = s.store.list("node-volumes").await?;
    let agents = s.store.list("agents").await?;
    let stale = stale_disks(s, &volumes, &attempts).await?;
    let usage = Usage {
        attempts: &attempts,
        agents: &agents,
        stale: &stale,
    };
    s.store
        .list("nodes")
        .await?
        .into_iter()
        .map(|node| usage.describe(node))
        .collect()
}

pub async fn admin(s: &Service, input: &Input) -> Result<Value> {
    let segments = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    if input.method == "GET" {
        // Nodes stay listed from their last record while the local runner is unreachable.
        let _ = refresh_local(s).await;
    }
    match (input.method.as_str(), segments.as_slice()) {
        ("GET" | "PUT", ["nodes", "placement", run]) => {
            let selection = (input.method == "PUT").then(|| input.body.clone());
            placement::configure(s, run, selection).await
        }
        ("POST", ["nodes", "placement", run, "move"]) => {
            crate::validation::uuid(run)?;
            moves::request(s, &s.store.run(run).await?, &input.body).await
        }
        ("GET", ["nodes", "alerts"]) => alerts::recent(s).await,
        ("GET", ["nodes", "settings"]) => {
            let mut value = publication::settings(s).await?;
            value["s3Configured"] = crate::object_storage::Storage::configured(s).is_ok().into();
            Ok(value)
        }
        ("PUT", ["nodes", "settings"]) => publication::configure(s, input.body.clone()).await,
        ("GET", ["nodes", "backups", run]) => backups(s, run).await,
        ("PUT", ["nodes", node, "storage"]) => configure_storage(s, node, &input.body).await,
        ("GET", ["nodes"]) => Ok(inventory(s).await?.into()),
        ("PUT", ["nodes", node]) => configure_node(s, node, input.body.clone()).await,
        ("POST", ["nodes", "enrollments"]) => invite(s, input.body.clone()).await,
        ("POST", ["nodes", node, "stale-disks", "delete"]) => delete_stale_disks(s, node).await,
        ("PUT", ["nodes", node, "agents"]) => grant_agents(s, node, input.body.clone()).await,
        ("POST", ["nodes", node, "revoke"]) => revoke(s, node).await,
        _ => Err(Error::not_found("Not found")),
    }
}

async fn backups(s: &Service, run: &str) -> Result<Value> {
    crate::validation::uuid(run)?;
    Ok(s.store
        .list("node-backups")
        .await?
        .into_iter()
        .filter(|point| point["runId"] == run)
        .map(publication::public)
        .collect::<Vec<_>>()
        .into())
}

async fn configure_storage(s: &Service, node: &str, body: &Value) -> Result<Value> {
    crate::validation::uuid(node)?;
    if body["enabled"] == false {
        return Err(Error::bad(
            "S3-backed disk storage is required on every node.",
        ));
    }
    let policy: Policy = decode(body.clone())?;
    policy.validate()?;
    if s.get("nodes", node).await?["revoked"] == true {
        return Err(Error::conflict("Node revoked."));
    }
    crate::object_storage::Storage::configured(s)?
        .validate()
        .await?;
    let response = s
        .http
        .post(format!("{}/storage-policy", controller_url(s, node)))
        .bearer_auth(runner_secret(s).await?)
        .json(&policy)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| Error::unavailable("Node storage probe unavailable."))?;
    if !response.status().is_success() {
        return Err(Error::conflict("Node did not pass its FUSE storage probe."));
    }
    let node = node.to_owned();
    let value = s
        .store
        .transaction(move |db| {
            let mut record = db
                .get("nodes", &node)?
                .ok_or_else(|| Error::not_found("Node missing."))?;
            record["storage"] = serde_json::to_value(&policy)?;
            db.put("nodes", &record)
        })
        .await?;
    Ok(public(value))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    name: String,
    tags: Vec<String>,
    limits: Resources,
    #[serde(default)]
    slots: Option<u64>,
    accepting: bool,
}

async fn configure_node(s: &Service, node: &str, body: Value) -> Result<Value> {
    let request: Configuration = decode(body)?;
    let label = name(&request.name)?;
    request.limits.validate()?;
    if request.limits.memory_mi_b <= 128 {
        return Err(Error::bad(
            "Shared RAM must exceed the 128 MiB controller reserve.",
        ));
    }
    if request
        .slots
        .is_some_and(|slots| !(1..=4096).contains(&slots))
    {
        return Err(Error::bad("Use between 1 and 4096 execution slots."));
    }
    if request.tags.len() > 32 || !request.tags.iter().all(|tag| valid_tag(tag)) {
        return Err(Error::bad(
            "Use at most 32 tags of 1 to 40 letters, digits or -_:./.",
        ));
    }
    let node = node.to_owned();
    s.store
        .transaction(move |db| {
            let mut record = db
                .get("nodes", &node)?
                .ok_or_else(|| Error::not_found("Node not found."))?;
            if record["revoked"] == true {
                return Err(Error::conflict("This node is revoked."));
            }
            // A capacity the node never reported counts as exceeded.
            let exceeds_capacity = ResourceKind::ALL.iter().any(|kind| {
                Some(request.limits.amount(*kind)) > record["capabilities"][kind.key()].as_u64()
            });
            if exceeds_capacity {
                return Err(Error::bad("Limits exceed the node's detected capacity."));
            }
            let changed_budget = record["limits"] != serde_json::to_value(&request.limits)?
                || request.slots.is_some_and(|count| count != slots(&record));
            if changed_budget {
                record["executionReady"] = false.into();
            }
            record["name"] = label.into();
            record["tags"] = request.tags.into();
            record["limits"] = serde_json::to_value(&request.limits)?;
            if let Some(slots) = request.slots {
                record["slots"] = slots.into();
            }
            record["accepting"] = request.accepting.into();
            db.put("nodes", &record)?;
            db.audit("node.configured", &json!({ "nodeId": node }))?;
            Ok(public(record))
        })
        .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invitation {
    name: String,
}

async fn invite(s: &Service, body: Value) -> Result<Value> {
    let request: Invitation = decode(body)?;
    let name = name(&request.name)?;
    let code = token();
    let expires = now() + ENROLLMENT_TTL_MS;
    s.store
        .set(
            &format!("node-enrollment:{}", digest(&code)),
            json!({ "name": name }),
            Some(expires),
        )
        .await?;
    let install = maintenance::release()
        .ok()
        .and_then(|_| connector::master(&s.config.public_url).ok())
        .map(|origin| {
            let script = format!("{origin}internal/nodes/install.sh").replace('\'', "'\\''");
            format!("curl --fail --silent --show-error '{script}' | sudo bash")
        });
    Ok(json!({ "code": code, "expiresAt": expires, "installCommand": install }))
}

async fn delete_stale_disks(s: &Service, node: &str) -> Result<Value> {
    crate::validation::uuid(node)?;
    let attempts = s.store.list("node-attempts").await?;
    let volumes = s
        .store
        .list("node-volumes")
        .await?
        .into_iter()
        .filter(|v| v["nodeId"] == node)
        .collect::<Vec<_>>();
    let (mut freed, mut failed) = (0u64, 0usize);
    for disk in stale_disks(s, &volumes, &attempts).await? {
        let run = text(&disk.volume, "runId");
        match crate::conversation_deletion::discard_stale_disk(s, run, node, disk.whole).await {
            Ok(()) => freed += disk.mi_b,
            Err(_) => failed += 1,
        }
    }
    s.store
        .audit(
            "node.stale-disks.deleted",
            json!({ "nodeId": node, "freedMiB": freed, "failed": failed }),
        )
        .await?;
    Ok(json!({ "freedMiB": freed, "failed": failed }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Grants {
    agent_ids: Vec<String>,
}

async fn grant_agents(s: &Service, node: &str, body: Value) -> Result<Value> {
    let request: Grants = decode(body)?;
    for agent in &request.agent_ids {
        crate::validation::uuid(agent)?;
    }
    let node = node.to_owned();
    s.store
        .transaction(move |db| {
            let record = db
                .get("nodes", &node)?
                .ok_or_else(|| Error::not_found("Node not found."))?;
            if record["revoked"] == true {
                return Err(Error::conflict("This node is revoked."));
            }
            for agent in db.list("agents")? {
                let granted = request.agent_ids.iter().any(|id| agent["id"] == *id);
                set_node_grant(db, agent, &node, granted)?;
            }
            db.audit(
                "node.agents.configured",
                &json!({ "nodeId": node, "agentIds": request.agent_ids }),
            )?;
            Ok(json!({ "nodeId": node }))
        })
        .await
}

fn set_node_grant(db: &Db<'_>, mut agent: Value, node: &str, granted: bool) -> Result<()> {
    // Agents allowed on every node keep that broader grant.
    let Some(mut nodes) = crate::service::policy(&agent)["nodes"].as_array().cloned() else {
        return Ok(());
    };
    let present = nodes.iter().any(|id| id == node);
    if granted == present {
        return Ok(());
    }
    if granted {
        nodes.push(node.into());
    } else {
        nodes.retain(|id| id != node);
    }
    if !agent["access"].is_object() {
        agent["access"] = json!({});
    }
    agent["access"]["nodes"] = nodes.into();
    db.put("agents", &agent)?;
    Ok(())
}

async fn revoke(s: &Service, node: &str) -> Result<Value> {
    let node = node.to_owned();
    s.store
        .transaction(move |db| {
            let mut record = db
                .get("nodes", &node)?
                .ok_or_else(|| Error::not_found("Node not found."))?;
            if record["local"] == true {
                return Err(Error::bad("The local runner cannot be revoked."));
            }
            record["revoked"] = true.into();
            record["accepting"] = false.into();
            for (key, value) in db.keys("node-token:")? {
                if value == node {
                    db.delete(&key)?;
                }
            }
            for mut agent in db.list("agents")? {
                let Some(nodes) = agent["access"]["nodes"].as_array_mut() else {
                    continue;
                };
                let before = nodes.len();
                nodes.retain(|id| id != &node);
                if nodes.len() != before {
                    db.put("agents", &agent)?;
                }
            }
            db.put("nodes", &record)?;
            db.audit("node.revoked", &json!({ "nodeId": node }))?;
            Ok(public(record))
        })
        .await
}

pub async fn internal(State(app): State<App>, request: Request) -> Result<Json<Value>> {
    let input = Input::read(request).await?;
    let s = &app.service;
    if input.method != "POST" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let value = match input.path.as_str() {
        "/internal/nodes/maintenance" => {
            let node = transport::authenticate(s, &input.headers).await?;
            maintenance::request(s, &node, &input.body).await?
        }
        "/internal/nodes/poll" => {
            let node = transport::authenticate(s, &input.headers).await?;
            s.node_transport.poll(&node).await?
        }
        "/internal/nodes/reply" => {
            let node = transport::authenticate(s, &input.headers).await?;
            s.node_transport.reply(&node, input.body).await?
        }
        "/internal/nodes/enroll" => enroll(s, input.body).await?,
        "/internal/nodes/heartbeat" => heartbeat(s, &input).await?,
        _ => return Err(Error::not_found("Not found")),
    };
    Ok(Json(value))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Enrollment {
    code: String,
    name: String,
    protocol: u32,
    capabilities: Capabilities,
    runtime_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnrolledNode {
    id: String,
    name: Value,
    local: bool,
    accepting: bool,
    revoked: bool,
    tags: Vec<String>,
    system_tags: Vec<&'static str>,
    capabilities: Capabilities,
    limits: Resources,
    storage: Policy,
    runtime_id: String,
    last_seen: i64,
    created_at: i64,
}

async fn enroll(s: &Service, body: Value) -> Result<Value> {
    let request: Enrollment = decode(body)?;
    if request.protocol != 1 {
        return Err(Error::conflict("Unsupported node protocol."));
    }
    request.capabilities.validate()?;
    name(&request.name)?;
    let runtime = name(&request.runtime_id)?;
    if request.code.len() != 43 {
        return Err(Error::unauthorized("Invalid or expired enrollment code."));
    }
    let key = format!("node-enrollment:{}", digest(&request.code));
    let node_id = id();
    let credential = token();
    let token_key = format!("node-token:{}", digest(&credential));
    s.store
        .transaction(move |db| {
            let invitation = db
                .kv(&key)?
                .ok_or_else(|| Error::unauthorized("Invalid or expired enrollment code."))?;
            db.delete(&key)?;
            let capabilities = request.capabilities;
            let node = EnrolledNode {
                id: node_id.clone(),
                name: invitation["name"].clone(),
                local: false,
                accepting: false,
                revoked: false,
                tags: Vec::new(),
                system_tags: capabilities.tags(),
                limits: capabilities.limits(),
                capabilities,
                storage: Policy::default(),
                runtime_id: runtime,
                last_seen: now(),
                created_at: now(),
            };
            db.put("nodes", &serde_json::to_value(node)?)?;
            db.set(&token_key, &Value::from(node_id.clone()), None)?;
            db.audit("node.enrolled", &json!({ "nodeId": node_id }))?;
            Ok(json!({
                "nodeId": node_id,
                "token": credential,
                "heartbeatIntervalMs": HEARTBEAT_INTERVAL_MS,
                "disconnectTimeoutMs": HEARTBEAT_TIMEOUT_MS,
            }))
        })
        .await
}

/// What the master expects from a node before it may execute conversations.
struct Expectations {
    data_root: String,
    image: Option<String>,
    lease_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Lease {
    id: String,
    remaining_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeartbeatReply {
    node_id: String,
    accepting: Value,
    slots: u64,
    limits: Value,
    leases: Vec<Lease>,
    heartbeat_interval_ms: i64,
    disconnect_timeout_ms: i64,
}

async fn heartbeat(s: &Service, input: &Input) -> Result<Value> {
    let credential = bearer(&input.headers)
        .filter(|token| token.len() == 43)
        .ok_or_else(|| Error::unauthorized("Invalid node identity."))?;
    let key = format!("node-token:{}", digest(credential));
    let capabilities = if input.body["capabilities"].is_object() {
        let value: Capabilities = decode(input.body["capabilities"].clone())?;
        value.validate()?;
        Some(serde_json::to_value(value)?)
    } else {
        None
    };
    let expected = Expectations {
        data_root: s.config.data_dir.to_string_lossy().into_owned(),
        image: maintenance::release().ok().map(|release| release.image),
        lease_ms: publication::lease_ms(s).await?,
    };
    let body = input.body.clone();
    let reply = s
        .store
        .transaction(move |db| record_heartbeat(db, &key, &body, capabilities, &expected))
        .await?;
    for lease in &reply.leases {
        record_lease(s, &lease.id, lease.remaining_ms).await;
    }
    Ok(serde_json::to_value(reply)?)
}

fn record_heartbeat(
    db: &Db<'_>,
    key: &str,
    body: &Value,
    capabilities: Option<Value>,
    expected: &Expectations,
) -> Result<HeartbeatReply> {
    let node_id = db
        .kv(key)?
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| Error::unauthorized("Invalid node identity."))?;
    let mut node = db
        .get("nodes", &node_id)?
        .filter(|v| v["revoked"] != true)
        .ok_or_else(|| Error::unauthorized("Invalid node identity."))?;
    if let Some(runtime) = body.get("runtimeId") {
        node["runtimeId"] = name(runtime.as_str().unwrap_or(""))?.into();
    }
    node["runtimes"] = body["runtimes"].clone();
    node["lastSeen"] = now().into();
    node["runtimeSlots"] = body["pool"]["capacity"].clone();
    node["usage"] = body["usage"].clone();
    node["pressure"] = body["pressure"].clone();
    node["budgetError"] = body["budgetError"].clone();
    if let Some(capabilities) = capabilities {
        node["capabilities"] = capabilities;
    }
    node["storage"] = serde_json::to_value(Policy::for_node(&node["storage"])?)?;
    node["imageDigest"] = body["imageDigest"].clone();
    node["executionReady"] = execution_ready(&node, body, expected).into();
    db.put("nodes", &node)?;
    let leases = renew_leases(db, &node_id, expected.lease_ms)?;
    Ok(HeartbeatReply {
        node_id,
        accepting: node["accepting"].clone(),
        slots: slots(&node),
        limits: node["limits"].clone(),
        leases,
        heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
        disconnect_timeout_ms: HEARTBEAT_TIMEOUT_MS,
    })
}

fn execution_ready(node: &Value, body: &Value, expected: &Expectations) -> bool {
    // A node whose update failed keeps running its previous image.
    let current_image = expected
        .image
        .as_ref()
        .is_none_or(|image| node["imageDigest"] == *image || node["updateError"].is_string());
    body["executionReady"] == true
        && body["sharedResources"] == true
        && body["budget"] == json!({ "slots": slots(node), "limits": node["limits"] })
        && node["capabilities"]["fuse"] == true
        && body["dataRoot"] == expected.data_root
        && current_image
}

/// Extends the leases of attempts this node still owns and may keep running.
fn renew_leases(db: &Db<'_>, node_id: &str, lease_ms: u64) -> Result<Vec<Lease>> {
    let mut leases = Vec::new();
    for mut attempt in db.list("node-attempts")? {
        if attempt["nodeId"] != node_id || !is_active_attempt(&attempt) {
            continue;
        }
        let run_id = text(&attempt, "runId");
        let run = db.run(run_id)?.unwrap_or_default();
        let checkpoint = db_checkpoint(db, run_id)?;
        let agent = db.get("agents", run_agent(&run))?.unwrap_or_default();
        let owns_execution = run["status"] == RunStatus::Running
            && run["cancelRequestedAt"].is_null()
            && checkpoint["runnerId"] == attempt["id"]
            && agent_allows(&agent, node_id);
        if !owns_execution {
            continue;
        }
        attempt["leaseExpiresAt"] = (now() + lease_ms as i64).into();
        attempt["leaseDurationMs"] = attempt["leaseDurationMs"]
            .as_u64()
            .unwrap_or(0)
            .max(lease_ms)
            .into();
        db.put("node-attempts", &attempt)?;
        leases.push(Lease {
            id: text(&attempt, "id").to_owned(),
            remaining_ms: lease_ms,
        });
    }
    Ok(leases)
}

pub async fn refresh_local(s: &Service) -> Result<()> {
    if s.config.runner_url.is_empty() {
        return Ok(());
    }
    let health = s
        .http
        .get(format!("{}/health", s.config.runner_url))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map_err(|_| Error::unavailable("Local runner unavailable."))?
        .json::<Value>()
        .await
        .map_err(Error::internal)?;
    let capabilities = health["capabilities"].clone();
    if !capabilities.is_object() {
        return Ok(());
    }
    let detected: Capabilities = decode(capabilities.clone())?;
    let mut budget_error = Value::Null;
    if let Some(record) = s.store.get("nodes", LOCAL_NODE_ID).await? {
        let budget = json!({ "slots": slots(&record), "limits": record["limits"] });
        if health["budget"].is_object() && health["budget"] != budget {
            let result = s
                .http
                .post(format!("{}/node-budget", s.config.runner_url))
                .bearer_auth(runner_secret(s).await?)
                .json(&budget)
                .send()
                .await;
            budget_error = connector::budget_error(result).await;
        }
    }
    s.store
        .transaction(move |db| {
            let mut record = match db.get("nodes", LOCAL_NODE_ID)? {
                Some(record) => record,
                None => json!({
                    "id": LOCAL_NODE_ID,
                    "name": "Current runner",
                    "local": true,
                    "revoked": false,
                    "accepting": true,
                    "tags": [],
                    "createdAt": now(),
                    "limits": detected.limits(),
                }),
            };
            record["systemTags"] = detected.tags().into();
            record["runtimes"] = health["runtimes"].clone();
            record["capabilities"] = capabilities;
            record["runtimeId"] = health["runtimeId"].clone();
            record["storage"] = serde_json::to_value(Policy::for_node(&record["storage"])?)?;
            record["executionReady"] = (health["status"] == "ok"
                && detected.fuse
                && health["sharedResources"] == true
                && health["budget"]
                    == json!({ "slots": slots(&record), "limits": record["limits"] }))
            .into();
            record["budgetError"] = budget_error;
            record["lastSeen"] = now().into();
            record["runtimeSlots"] = health["pool"]["capacity"].clone();
            record["usage"] = health["usage"].clone();
            record["pressure"] = health["pressure"].clone();
            db.put("nodes", &record)?;
            Ok(())
        })
        .await
}

/// Linux suspend time counts toward a remote execution lease.
pub(crate) fn boot_ms() -> u64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut value) } != 0 {
        return u64::MAX;
    }
    value.tv_sec as u64 * 1000 + value.tv_nsec as u64 / 1_000_000
}

pub async fn daemon(
    directory: &std::path::Path,
    stop: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let mut runner = tokio::spawn(crate::runner::serve(stop.child_token()));
    let directory = directory.to_owned();
    let connector_stop = stop.child_token();
    let mut connector =
        tokio::spawn(async move { connector::connect(&directory, connector_stop).await });
    let (runner_first, result) = tokio::select! {
        result = &mut runner => (true, result),
        result = &mut connector => (false, result),
    };
    stop.cancel();
    // Only the first task's result is reported; the other just has to finish.
    if runner_first {
        let _ = connector.await;
    } else {
        let _ = runner.await;
    }
    result.map_err(Error::internal)?
}

pub(crate) async fn record_lease(s: &Service, attempt: &str, remaining_ms: u64) {
    let until = tokio::time::Instant::now() + std::time::Duration::from_millis(remaining_ms);
    s.node_lease_deadlines
        .lock()
        .await
        .entry(attempt.into())
        .and_modify(|deadline| *deadline = (*deadline).max(until))
        .or_insert(until);
}
