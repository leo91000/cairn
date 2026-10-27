//! Trusted node identities. Enrollment and revocation are serialized with heartbeats.
pub mod alerts;
pub mod checkpoint;
pub mod connector;
pub mod coordination;
pub(crate) mod disk_capacity;
pub mod disk_grants;
pub(crate) mod eviction;
pub mod exclusive;
pub mod executor;
pub mod files;
pub mod inventory;
pub mod maintenance;
pub mod moves;
pub mod native_mode;
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
use cairn_contracts::{
    CreateNodeEnrollmentInput, ExecutionNode, NodeAgent, NodeBackend, NodeConfiguration,
    NodeEnrollment, NodeId, NodeStatus, StaleDisks,
};
pub use cairn_contracts::{NodeCapabilities as Capabilities, NodeResources as Resources};
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

fn status(node: &ExecutionNode) -> NodeStatus {
    if node.revoked {
        NodeStatus::Revoked
    } else if node.local {
        NodeStatus::Local
    } else if node
        .last_seen
        .is_some_and(|seen| now() - seen < HEARTBEAT_TIMEOUT_MS)
    {
        NodeStatus::Online
    } else {
        NodeStatus::Offline
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

/// The run's resume checkpoint, or `null` when none was recorded.
pub(crate) async fn checkpoint(s: &Service, run: &str) -> Result<Value> {
    Ok(s.store.checkpoint_boundary(run).await?.unwrap_or_default())
}

pub(crate) fn db_checkpoint(db: &Db<'_>, run: &str) -> Result<Value> {
    Ok(db.checkpoint_boundary(run)?.unwrap_or_default())
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

/// Native workloads share the host network. Their privileged controller must
/// use an authenticated, root-private filesystem endpoint, never a claimable TCP port.
pub(crate) fn local_client() -> Result<reqwest::Client> {
    if crate::native::enabled() {
        return crate::native::controller_client(
            &crate::native::state_dir().join("controller.sock"),
        );
    }
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(Error::internal)
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

fn public(mut node: ExecutionNode) -> Result<Value> {
    node.status = status(&node);
    Ok(serde_json::to_value(node)?)
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

struct Usage<'a> {
    attempts: &'a [Value],
    agents: &'a [Value],
    stale: &'a [StaleDisk],
}

impl Usage<'_> {
    fn stale_summary(&self, node: &ExecutionNode) -> StaleDisks {
        let stale = self
            .stale
            .iter()
            .filter(|disk| disk.volume["nodeId"] == node.id.to_string());
        StaleDisks {
            count: stale.clone().count() as u32,
            disk_mi_b: stale.map(|disk| disk.mi_b).sum(),
        }
    }

    fn granted_agents(&self, node: &ExecutionNode) -> Result<Vec<NodeAgent>> {
        if node.revoked {
            return Ok(Vec::new());
        }
        // Agent policy remains a named boundary until A18; no node field is read dynamically.
        let id = node.id.to_string();
        Ok(self
            .agents
            .iter()
            .filter(|agent| {
                agent_allows(agent, &id)
                    && (!node
                        .native_mode_request
                        .as_ref()
                        .is_some_and(|r| r.mode == cairn_contracts::NativeMode::Administrator)
                        || node
                            .native_reviewed_agents
                            .iter()
                            .any(|id| id == text(agent, "id")))
                    && crate::native::policy::supports(
                        agent,
                        if node.capabilities.backend == NodeBackend::Native {
                            "native"
                        } else {
                            "firecracker"
                        },
                    )
            })
            .map(|agent| NodeAgent {
                id: text(agent, "id").into(),
                name: text(agent, "name").into(),
                all_nodes: crate::service::policy(agent)["nodes"].is_null(),
            })
            .collect())
    }

    fn describe(&self, mut node: ExecutionNode) -> Result<ExecutionNode> {
        let used = self
            .attempts
            .iter()
            .filter(|a| a["nodeId"] == node.id.to_string() && is_active_attempt(a))
            .count() as u32;
        let capacity = node.slots();
        node.slots = Some(capacity);
        node.occupied_slots = used;
        node.available_slots = capacity.saturating_sub(used);
        node.stale_disks = self.stale_summary(&node);
        node.agents = self.granted_agents(&node)?;
        node.status = status(&node);
        Ok(node)
    }
}

/// Fresh nodes inherit controller capacity until the owner configures a slot budget.
pub(crate) fn slots(node: &Value) -> u64 {
    node["slots"]
        .as_u64()
        .or(node["runtimeSlots"].as_u64())
        .unwrap_or(4)
        .max(1)
}

/// Nodes with their shared budgets, free slots and authorized agents.
pub async fn typed_inventory(s: &Service) -> Result<Vec<ExecutionNode>> {
    let attempts = s.store.list("node-attempts").await?;
    let volumes = s.store.list("node-volumes").await?;
    let agents = s.store.agent_snapshots().await?;
    let stale = stale_disks(s, &volumes, &attempts).await?;
    let usage = Usage {
        attempts: &attempts,
        agents: &agents,
        stale: &stale,
    };
    let mut nodes = s
        .store
        .nodes()
        .await?
        .into_iter()
        .map(|node| usage.describe(node))
        .collect::<Result<Vec<_>>>()?;
    s.store
        .read(move |db| {
            for node in &mut nodes {
                if node.capabilities.backend == NodeBackend::Native
                    && node.disk_growth_protocol == Some(1)
                {
                    node.disk_accounting = serde_json::from_value(disk_capacity::accounting(
                        db,
                        &serde_json::to_value(&*node)?,
                        &attempts,
                    )?)?;
                }
            }
            Ok(nodes)
        })
        .await
}

/// A10/A11 view adapter; inventory/configuration themselves use the typed reader.
pub async fn inventory(s: &Service) -> Result<Vec<Value>> {
    typed_inventory(s)
        .await?
        .into_iter()
        .map(|node| Ok(serde_json::to_value(node)?))
        .collect()
}

fn public_boundary(node: Value) -> Result<Value> {
    public(serde_json::from_value(node)?)
}

pub async fn admin(s: &Service, input: &Input) -> Result<Value> {
    let segments = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    let command = match (input.method.as_str(), segments.as_slice()) {
        ("GET", ["nodes"]) => Some(("list_nodes", json!({}))),
        ("GET", ["nodes", "native-callers"]) => Some(("list_native_callers", json!({}))),
        ("POST", ["nodes", "enrollments"]) => Some(("create_node_enrollment", input.body.clone())),
        ("PUT", ["nodes", node]) if *node != "settings" => {
            Some(("update_node", json!({"nodeId": node, "node": input.body})))
        }
        ("PUT", ["nodes", node, "storage"]) => {
            if input.body["nodeId"] != *node {
                return Err(Error::bad("Node identity differs from path."));
            }
            Some(("update_node_storage", input.body.clone()))
        }
        ("POST", ["nodes", node, "native-mode"]) => Some((
            "change_native_node_mode",
            json!({"nodeId": node, "change": input.body}),
        )),
        ("POST", ["nodes", node, "revoke"]) => {
            let mut body = if input.body.is_null() {
                json!({})
            } else {
                input.body.clone()
            };
            if !body.is_object() {
                return Err(Error::bad("Invalid node revocation."));
            }
            if body.get("nodeId").is_some_and(|id| id != *node) {
                return Err(Error::bad("Node identity differs from path."));
            }
            body["nodeId"] = (*node).into();
            Some(("revoke_node", body))
        }
        _ => None,
    };
    if let Some((name, body)) = command {
        return crate::capabilities::node_operation(name)
            .expect("node command")
            .invoke(s, body, crate::capabilities::ResumeContext::http(input)?)
            .await;
    }
    if input.method == "GET" {
        // Nodes stay listed from their last record while the local runner is unreachable.
        let _ = refresh_local(s).await;
    }
    match (input.method.as_str(), segments.as_slice()) {
        ("GET" | "PUT", ["nodes", "placement", run]) => {
            if input.method == "GET" {
                placement::configure(s, run, None).await
            } else {
                placement::configure_authorized(
                    s,
                    run,
                    Some(input.body.clone()),
                    crate::native::policy::run_authorization(run),
                )
                .await
            }
        }
        ("POST", ["nodes", "placement", run, "move"]) => {
            crate::validation::uuid(run)?;
            moves::request(s, &s.store.run(run).await?, &input.body).await
        }
        ("GET", ["nodes", "placement", run, "backend-migration"]) => {
            let chat = crate::backend_migration::chat_id(s, run).await?;
            crate::backend_migration::preview(s, &chat, |_| Ok(())).await
        }
        ("POST", ["nodes", "placement", run, "backend-migration"]) => {
            let chat = crate::backend_migration::chat_id(s, run).await?;
            let mut args = input.body.clone();
            if !args.is_object() {
                return Err(Error::bad("Confirm the destination backend and node."));
            }
            args["chatId"] = chat.into();
            s.migrate_chat_backend(args, |_| Ok(())).await
        }
        ("GET", ["nodes", "alerts"]) => alerts::recent(s).await,
        ("GET", ["nodes", "settings"]) => {
            let mut value = publication::settings(s).await?;
            value["s3Configured"] = crate::object_storage::Storage::configured(s).is_ok().into();
            Ok(value)
        }
        ("PUT", ["nodes", "settings"]) => publication::configure(s, input.body.clone()).await,
        ("GET", ["nodes", "backups", run]) => backups(s, run).await,
        ("POST", ["nodes", _, "exclusive", reservation, "release"]) => {
            exclusive::release_owned(s, reservation, None).await
        }
        ("POST", ["nodes", node, "stale-disks", "delete"]) => delete_stale_disks(s, node).await,
        ("PUT", ["nodes", node, "agents"]) => grant_agents(s, node, input.body.clone()).await,
        _ => Err(Error::not_found("Not found")),
    }
}

pub(crate) async fn native_callers(
    s: &Service,
    authorize: impl FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
) -> Result<Value> {
    crate::native::policy::refresh(s).await?;
    s.store
        .read(move |db| {
            authorize(db)?;
            let authority = db.kv("native-authority")?.unwrap_or_default();
            Ok(authority["accounts"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(id, account)| json!({ "id": id, "email": account["email"] }))
                .collect::<Vec<_>>()
                .into())
        })
        .await
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

pub(crate) async fn configure_storage(
    s: &Service,
    node: &str,
    body: &Value,
    authorize: impl FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
) -> Result<Value> {
    crate::validation::uuid(node)?;
    let request: cairn_contracts::UpdateNodeStorageInput = decode(body.clone())?;
    if request.node_id.to_string() != node {
        return Err(Error::bad("Node identity differs from request."));
    }
    let policy = request.storage;
    policy.validate().map_err(Error::bad)?;
    let current = s.get_node(node).await?;
    if current.revoked {
        return Err(Error::conflict("Node revoked."));
    }
    if current.revision != request.revision {
        return Err(Error::conflict(
            "This node was changed elsewhere. Reload it before saving.",
        ));
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
        return Err(Error::conflict("Node did not pass its storage probe."));
    }
    let node = node.to_owned();
    let value = s
        .store
        .transaction(move |db| {
            authorize(db)?;
            let mut record = db
                .node(&node)?
                .ok_or_else(|| Error::not_found("Node missing."))?;
            if record.revoked {
                return Err(Error::conflict("Node revoked."));
            }
            if record.revision != request.revision {
                return Err(Error::conflict(
                    "This node was changed elsewhere. Reload it before saving.",
                ));
            }
            record.storage = policy;
            record.revision = id();
            db.save_node(&record)
        })
        .await?;
    public(value)
}

pub(crate) async fn configure_node(
    s: &Service,
    node: &str,
    body: Value,
    authorize: impl FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
) -> Result<Value> {
    let request: NodeConfiguration = decode(body)?;
    let owner_review = native_mode::owner().is_some();
    if let Some(accounts) = &request.native_accounts {
        let existing = s.get_node(node).await?;
        if accounts
            .iter()
            .any(|account| !existing.native_accounts.contains(account))
        {
            crate::native::policy::refresh(s).await?;
        }
    }
    let label = name(&request.name)?;
    request.limits.validate().map_err(Error::bad)?;
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
            authorize(db)?;
            let mut record = db.node(&node)?.ok_or_else(|| Error::not_found("Node not found."))?;
            if record.revoked { return Err(Error::conflict("This node is revoked.")); }
            if record.revision != request.revision { return Err(Error::conflict("This node was changed elsewhere. Reload it before saving.")); }
            let capacity = record.capabilities.resources();
            let detected_total = record.capabilities.disk_total_mi_b;
            let disk_capacity = if detected_total > 0 { detected_total } else { capacity.disk_mi_b };
            if request.limits.cpu > capacity.cpu || request.limits.memory_mi_b > capacity.memory_mi_b || request.limits.disk_mi_b > disk_capacity {
                return Err(Error::bad("Limits exceed the node's detected capacity."));
            }
            let changed_budget = record.limits != request.limits || request.slots.is_some_and(|count| count != record.slots());
            if changed_budget { record.execution_ready = false; }
            record.name = label;
            record.tags = request.tags;
            record.limits = request.limits;
            if let Some(slots) = request.slots { record.slots = Some(slots); }
            if let Some(enabled) = request.native_enabled { record.native_enabled = enabled; }
            if let Some(accounts) = request.native_accounts {
                if accounts.len() > 100 { return Err(Error::bad("Use at most 100 trusted native accounts.")); }
                let authority = db.kv("native-authority")?.unwrap_or_default();
                for account in &accounts {
                    crate::validation::uuid(account)?;
                    let retained = record.native_accounts.contains(account);
                    let administrator = record.native_mode_request.as_ref().is_some_and(|r| r.mode == cairn_contracts::NativeMode::Administrator);
                    if !retained && administrator && (!owner_review || !request.confirm_native_administration) {
                        return Err(Error::forbidden("The owner must confirm that new trusted people may administer this host."));
                    }
                    if !retained && authority["accounts"].get(account).is_none() {
                        return Err(Error::bad("Select a current installation owner or member for native access."));
                    }
                }
                record.native_accounts = accounts;
            }
            record.accepting = request.accepting;
            record.revision = id();
            db.save_node(&record)?;
            db.audit("node.configured", &json!({ "nodeId": node }))?;
            public(record)
        })
        .await
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnrollmentChallenge {
    name: String,
    backend: NodeBackend,
    node_id: NodeId,
}

pub(crate) async fn invite(
    s: &Service,
    body: Value,
    authorize: impl FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
) -> Result<Value> {
    let request: CreateNodeEnrollmentInput = decode(body)?;
    let name = name(&request.name)?;
    let backend = if request.backend == NodeBackend::Native {
        "native"
    } else {
        "firecracker"
    };
    let direct = match request.manager_url {
        Some(value) => Some(connector::master(value.trim())?),
        None => None,
    };
    // Without an address, Beacon introduces the node to a claimed
    // installation: no address, certificate or incoming port on the manager.
    // The node pins the manager's node certificate from its command.
    let beacon = match direct {
        Some(_) => None,
        None => match crate::relay::node_channel_route(&s.config.data_dir).await? {
            Some((beacon, installation)) => {
                let certificate = crate::relay::nodes::NodeCertificate::load(
                    &s.config.data_dir.join("installation-relay"),
                )
                .await?;
                Some((beacon, installation, certificate.pin.clone()))
            }
            None => None,
        },
    };
    let origin = match (direct, &beacon) {
        (Some(direct), _) => Some(direct),
        (None, None) => connector::master(&s.config.public_url).ok(),
        (None, Some(_)) => None,
    };
    let manager_url = origin
        .as_ref()
        .map(|url| url.as_str().trim_end_matches('/').to_owned());
    let code = token();
    let node_id = id();
    let expires = now() + ENROLLMENT_TTL_MS;
    let invitation = EnrollmentChallenge {
        name,
        backend: request.backend,
        node_id: NodeId(uuid::Uuid::parse_str(&node_id).map_err(Error::internal)?),
    };
    let key = format!("node-enrollment:{}", digest(&code));
    s.store
        .transaction(move |db| {
            authorize(db)?;
            db.set(&key, &serde_json::to_value(&invitation)?, Some(expires))
        })
        .await?;
    let installable = backend == "native" || maintenance::release().is_ok();
    let route = if beacon.is_some() { "beacon" } else { "direct" };
    let install = match (installable, beacon, origin) {
        (false, _, _) => None,
        (true, Some((beacon, installation, pin)), _) => {
            let script = format!("{beacon}/install/node.sh").replace('\'', "'\\''");
            Some(format!(
                "curl --fail --silent --show-error '{script}' | sudo bash -s -- '{installation}' {backend} {pin}"
            ))
        }
        (true, None, Some(origin)) => {
            let mut script = origin.clone();
            script.set_path(if backend == "native" {
                "/internal/nodes/native-install.sh"
            } else {
                "/internal/nodes/install.sh"
            });
            let quoted = script.as_str().replace('\'', "'\\''");
            let manager = origin.as_str().trim_end_matches('/').replace('\'', "'\\''");
            Some(format!(
                "curl --fail --silent --show-error '{quoted}' | sudo bash -s -- '{manager}'"
            ))
        }
        (true, None, None) => None,
    };
    Ok(serde_json::to_value(NodeEnrollment {
        code,
        expires_at: expires,
        route: route.into(),
        manager_url,
        install_command: install,
        backend: request.backend,
        node_id: NodeId(uuid::Uuid::parse_str(&node_id).map_err(Error::internal)?),
    })?)
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
    #[serde(default)]
    confirm_native_administration: bool,
    /// Only grants the listed agents and never revokes, for example after adding a
    /// machine while an agent page grants it at the same time.
    #[serde(default)]
    add: bool,
}

async fn grant_agents(s: &Service, node: &str, body: Value) -> Result<Value> {
    let request: Grants = decode(body)?;
    for agent in &request.agent_ids {
        crate::validation::uuid(agent)?;
    }
    let node = node.to_owned();
    let owner_review = native_mode::owner().is_some();
    s.store
        .transaction(move |db| {
            let mut record = db
                .node(&node)?
                .ok_or_else(|| Error::not_found("Node not found."))?;
            if record.revoked {
                return Err(Error::conflict("This node is revoked."));
            }
            for profile in db.agents()? {
                let agent = serde_json::to_value(&profile)?;
                let granted = request
                    .agent_ids
                    .iter()
                    .any(|id| *id == profile.id.to_string());
                if request.add && !granted {
                    continue;
                }
                if granted
                    && !crate::native::policy::supports(
                        &agent,
                        if record.capabilities.backend == NodeBackend::Native {
                            "native"
                        } else {
                            "firecracker"
                        },
                    )
                {
                    return Err(Error::bad(
                        "Choose agents configured for this node's execution backend.",
                    ));
                }
                if record
                    .native_mode_request
                    .as_ref()
                    .is_some_and(|r| r.mode == cairn_contracts::NativeMode::Administrator)
                {
                    let mut reviewed = record.native_reviewed_agents.clone();
                    if !granted {
                        reviewed.retain(|id| id != text(&agent, "id"));
                    } else if owner_review
                        && request.confirm_native_administration
                        && !reviewed.iter().any(|id| id == text(&agent, "id"))
                    {
                        reviewed.push(text(&agent, "id").into());
                    } else if !record
                        .native_reviewed_agents
                        .iter()
                        .any(|id| id == text(&agent, "id"))
                    {
                        return Err(Error::forbidden(
                            "The owner must explicitly review access to this administrative node.",
                        ));
                    }
                    record.native_reviewed_agents = reviewed;
                }
                set_node_grant(db, profile, &node, granted)?;
            }
            // Revoking placement can also release exclusive reservations on
            // this record. Persist only our admission field onto that new copy.
            let mut current = db.node(&node)?.unwrap();
            if current.native_reviewed_agents != record.native_reviewed_agents {
                current.native_reviewed_agents = record.native_reviewed_agents;
                current.revision = id();
                db.save_node(&current)?;
            }
            db.audit(
                "node.agents.configured",
                &json!({ "nodeId": node, "agentIds": request.agent_ids }),
            )?;
            Ok(json!({ "nodeId": node }))
        })
        .await
}

fn set_node_grant(
    db: &Db<'_>,
    mut agent: cairn_contracts::Agent,
    node: &str,
    granted: bool,
) -> Result<()> {
    let Some(nodes) = &mut agent.settings.access.nodes else {
        return Ok(());
    };
    if granted == nodes.iter().any(|id| id == node) {
        return Ok(());
    }
    if granted {
        nodes.push(node.to_owned());
    } else {
        nodes.retain(|id| id != node);
        agent.settings.exclusive_nodes.retain(|id| id != node);
        agent.settings.node_priorities.retain(|id| id != node);
    }
    agent.revision = crate::config::id();
    db.save_agent(&agent)?;
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
struct Revocation {
    /// Return conversations fixed to or preferring this node to automatic placement,
    /// as when the machine re-enrolls under another execution backend.
    release_placements: bool,
}

pub(crate) async fn revoke(
    s: &Service,
    node: &str,
    body: Value,
    authorize: impl FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
) -> Result<Value> {
    let request: Revocation = if body.is_null() {
        Revocation::default()
    } else {
        decode(body)?
    };
    let node = node.to_owned();
    s.store
        .transaction(move |db| {
            authorize(db)?;
            let mut record = db
                .node(&node)?
                .ok_or_else(|| Error::not_found("Node not found."))?;
            if record.local {
                return Err(Error::bad("The local runner cannot be revoked."));
            }
            record.revoked = true;
            record.accepting = false;
            record.revision = id();
            for (key, value) in db.keys("node-token:")? {
                if value == node {
                    db.delete(&key)?;
                }
            }
            for mut agent in db.agents()? {
                let mut changed = false;
                if let Some(nodes) = &mut agent.settings.access.nodes {
                    let before = nodes.len();
                    nodes.retain(|id| id != &node);
                    changed |= before != nodes.len();
                }
                let before = agent.settings.exclusive_nodes.len();
                agent.settings.exclusive_nodes.retain(|id| id != &node);
                changed |= before != agent.settings.exclusive_nodes.len();
                let before = agent.settings.node_priorities.len();
                agent.settings.node_priorities.retain(|id| id != &node);
                changed |= before != agent.settings.node_priorities.len();
                if changed {
                    agent.revision = crate::config::id();
                    db.save_agent(&agent)?;
                }
            }
            db.save_node(&record)?;
            // Agent revocation updates the authoritative grant relations in this transaction.
            record = db
                .node(&node)?
                .ok_or_else(|| Error::not_found("Node not found."))?;

            let mut released = 0_u64;
            if request.release_placements {
                released = release_placements(db, &node)?;
            }

            db.audit(
                "node.revoked",
                &json!({ "nodeId": node, "releasedPlacements": released }),
            )?;
            Ok(serde_json::to_value(cairn_contracts::NodeRevocation {
                node: serde_json::from_value(public(record)?)?,
                released_placements: released as u32,
            })?)
        })
        .await
}

/// Clears pins and preferences naming `node`. A pinned conversation otherwise waits
/// for a machine that can never return under this identity.
fn release_placements(db: &Db<'_>, node: &str) -> Result<u64> {
    let runs = db.json_rows(
        "SELECT data FROM runs
         WHERE json_extract(data,'$.pinnedNodeId')=?1 OR json_extract(data,'$.preferredNodeId')=?1",
        [node],
    )?;
    let mut released = 0;
    for run in runs {
        let mut patch = json!({});
        for field in ["pinnedNodeId", "preferredNodeId"] {
            if run[field] == node {
                patch[field] = Value::Null;
            }
        }
        db.update_run_boundary(text(&run, "id"), &patch)?;
        released += 1;
    }
    Ok(released)
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

async fn enroll(s: &Service, body: Value) -> Result<Value> {
    let request: Enrollment = decode(body)?;
    if request.protocol != 1 {
        return Err(Error::conflict("Unsupported node protocol."));
    }
    if request.runtime_id != component_runtime() {
        return Err(Error::conflict(
            "Node version mismatch; deploy manager and nodes at the same version.",
        ));
    }
    request.capabilities.validate().map_err(Error::bad)?;
    name(&request.name)?;
    let runtime = name(&request.runtime_id)?;
    if request.code.len() != 43 {
        return Err(Error::unauthorized("Invalid or expired enrollment code."));
    }
    let key = format!("node-enrollment:{}", digest(&request.code));
    let credential = token();
    let token_key = format!("node-token:{}", digest(&credential));
    s.store
        .transaction(move |db| {
            let proof = db
                .kv(&key)?
                .ok_or_else(|| Error::unauthorized("Invalid or expired enrollment code."))?;
            let invitation: EnrollmentChallenge = decode(proof)?;
            if request.capabilities.backend != invitation.backend {
                return Err(Error::forbidden(
                    "Enrollment backend differs from the owner's invitation.",
                ));
            }
            let node_id = invitation.node_id.to_string();
            db.delete(&key)?;
            let capabilities = request.capabilities;
            let native = capabilities.backend == NodeBackend::Native;
            let node = ExecutionNode {
                id: NodeId(uuid::Uuid::parse_str(&node_id).map_err(Error::internal)?),
                name: invitation.name,
                revision: id(),
                accepting: false,
                system_tags: capabilities.tags(),
                limits: capabilities.limits(),
                capabilities,
                storage: Policy::default(),
                runtime_id: runtime,
                last_seen: Some(now()),
                created_at: now(),
                slots: native.then_some(1),
                ..ExecutionNode::default()
            };
            db.save_node(&node)?;
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

pub(crate) fn component_runtime() -> String {
    std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_| "development".into())
}

fn matches_component_version(observation: &Value) -> bool {
    observation["runtimeId"] == component_runtime() && observation["nodeProtocol"] == 2
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
    accepting: bool,
    slots: u64,
    limits: Resources,
    leases: Vec<Lease>,
    native_mode: cairn_contracts::NativeModeRequest,
    #[serde(skip_serializing_if = "Value::is_null")]
    disk_growth: Value,
    heartbeat_interval_ms: i64,
    disconnect_timeout_ms: i64,
}

async fn heartbeat(s: &Service, input: &Input) -> Result<Value> {
    let credential = bearer(&input.headers)
        .filter(|token| token.len() == 43)
        .ok_or_else(|| Error::unauthorized("Invalid node identity."))?;
    let key = format!("node-token:{}", digest(credential));
    // Rejected versions must reach the authenticated readiness fence even when
    // their capabilities are malformed or native authority is unavailable.
    let matching_version = matches_component_version(&input.body);
    let capabilities = if matching_version && input.body["capabilities"].is_object() {
        let value: Capabilities = decode(input.body["capabilities"].clone())?;
        value.validate().map_err(Error::bad)?;
        Some(serde_json::to_value(value)?)
    } else {
        None
    };
    if matching_version && input.body["capabilities"]["backend"] == "native" {
        crate::native::policy::refresh(s).await?;
    }
    let expected = Expectations {
        data_root: s.config.data_dir.to_string_lossy().into_owned(),
        image: maintenance::release().ok().map(|release| release.image),
        lease_ms: publication::lease_ms(s).await?,
    };
    let body = input.body.clone();
    let reply = s
        .store
        .transaction(move |db| record_heartbeat(db, &key, &body, capabilities, &expected))
        .await??;
    for lease in &reply.leases {
        record_lease(s, &lease.id, lease.remaining_ms).await;
    }
    let mut value = serde_json::to_value(&reply)?;
    // Catalogue failures must not interrupt current execution leases.
    match crate::project_archives::catalog(s, &reply.node_id).await {
        Ok(archives) => value["projectArchives"] = serde_json::to_value(archives)?,
        Err(error) => tracing::warn!(
            status = error.status,
            "Archive catalogue unavailable; node leases remain valid"
        ),
    }
    Ok(value)
}

fn record_heartbeat(
    db: &Db<'_>,
    key: &str,
    body: &Value,
    capabilities: Option<Value>,
    expected: &Expectations,
) -> Result<Result<HeartbeatReply>> {
    let node_id = db
        .kv(key)?
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| Error::unauthorized("Invalid node identity."))?;
    let mut node = db
        .node(&node_id)?
        .filter(|n| !n.revoked)
        .ok_or_else(|| Error::unauthorized("Invalid node identity."))?;
    if !matches_component_version(body) {
        node.execution_ready = false;
        db.save_node(&node)?;
        return Ok(Err(Error::conflict(
            "Node/controller version mismatch; deploy all components at the same version.",
        )));
    }
    node.runtime_id = name(text(body, "runtimeId"))?;
    node.last_seen = Some(now());
    node.runtime_slots = decode(body["pool"]["capacity"].clone())?;
    node.retained_runs = decode(body["pool"]["retained_runs"].clone())?;
    node.usage = decode(body["usage"].clone())?;
    node.pressure = decode(body["pressure"].clone())?;
    node.budget_error = decode(body["budgetError"].clone())?;
    node.exclusive_reservations = body["exclusiveReservations"] == true;
    if let Some(capabilities) = capabilities {
        let capabilities: Capabilities = decode(capabilities)?;
        if capabilities.backend != node.capabilities.backend {
            return Err(Error::conflict(
                "A node cannot change its execution backend. Enroll the machine again as a new node.",
            ));
        }
        node.capabilities = capabilities;
    }
    node.image_digest = decode(body["imageDigest"].clone())?;
    node.native_mode_observed = decode(body["nativeMode"].clone())?;
    if node.capabilities.backend == NodeBackend::Native {
        let boundary = serde_json::to_value(&node)?;
        node.native_mode_status = Some(
            if native_mode::applied(&boundary) {
                "applied"
            } else if body["nativeMode"]["status"] == "failed"
                && (body["nativeMode"]["generation"] == boundary["nativeModeRequest"]["generation"]
                    || body["nativeMode"]["target"]["generation"]
                        == boundary["nativeModeRequest"]["generation"])
            {
                "failed"
            } else {
                "pending"
            }
            .into(),
        );
    }
    node.execution_ready = execution_ready(&serde_json::to_value(&node)?, body, expected);
    let mut disk_boundary = serde_json::to_value(&node)?;
    let disk_growth = disk_capacity::report(db, &mut disk_boundary, &body["diskGrowth"])?;
    node.disk_growth_protocol = decode(disk_boundary["diskGrowthProtocol"].clone())?;
    node.disk_capacity = decode(disk_boundary["diskCapacity"].clone())?;
    db.save_node(&node)?;
    let leases = renew_leases(db, &node_id, expected.lease_ms)?;
    Ok(Ok(HeartbeatReply {
        native_mode: node.native_mode_request.clone().unwrap_or_default(),
        disk_growth,
        node_id,
        accepting: node.accepting,
        slots: u64::from(node.slots()),
        limits: node.limits,
        leases,
        heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
        disconnect_timeout_ms: HEARTBEAT_TIMEOUT_MS,
    }))
}

fn execution_ready(node: &Value, body: &Value, expected: &Expectations) -> bool {
    // Only the image approved by this manager may execute.
    let current_image = expected
        .image
        .as_ref()
        .is_none_or(|image| node["imageDigest"] == *image);
    if crate::native::policy::node_native(node) {
        return native_mode::applied(node)
            && body["executionReady"] == true
            && body["backend"] == "native"
            && body["nodeProtocol"] == 2
            && body["dataRoot"]
                .as_str()
                .is_some_and(|path| std::path::Path::new(path).is_absolute())
            && body["budget"] == json!({ "slots": slots(node), "limits": node["limits"] });
    }
    body["executionReady"] == true
        && body["pool"]["quarantined"] != true
        && body["sharedResources"] == true
        && body["budget"] == json!({ "slots": slots(node), "limits": node["limits"] })
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
        let agent = db.agent_snapshot(run_agent(&run))?.unwrap_or_default();
        let owns_execution = run["status"] == RunStatus::Running
            && run["cancelRequestedAt"].is_null()
            && checkpoint["runnerId"] == attempt["id"]
            && agent_allows(&agent, node_id);
        let native_allowed =
            !crate::native::policy::node_native(&db.node_boundary(node_id)?.unwrap_or_default())
                || crate::native::policy::allows(
                    db,
                    &run,
                    &db.node_boundary(node_id)?.unwrap_or_default(),
                )?;
        if !owns_execution || !native_allowed {
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
    if health["runtimeId"] != component_runtime() || health["nodeProtocol"] != 2 {
        if let Some(mut node) = s.store.node(LOCAL_NODE_ID).await? {
            node.execution_ready = false;
            s.store.transaction(move |db| db.save_node(&node)).await?;
        }
        return Err(Error::conflict(
            "Local controller version mismatch; deploy all components at the same version.",
        ));
    }
    let capabilities = health["capabilities"].clone();
    if !capabilities.is_object() {
        return Ok(());
    }
    let detected: Capabilities = decode(capabilities.clone())?;
    let mut budget_error = Value::Null;
    if let Some(record) = s.store.node(LOCAL_NODE_ID).await? {
        let budget = json!({ "slots": record.slots(), "limits": record.limits });
        if health["budget"].is_object() && health["budget"] != budget {
            let started = std::time::Instant::now();
            let result = s
                .http
                .post(format!("{}/node-budget", s.config.runner_url))
                .bearer_auth(runner_secret(s).await?)
                .json(&budget)
                .send()
                .await;
            let status = result
                .as_ref()
                .ok()
                .map(|response| response.status().as_u16());
            budget_error = connector::budget_error(result).await;
            tracing::info!(
                target: "cairn_performance",
                operation = "node_budget_reconcile",
                observed_slots = health["budget"]["slots"].as_u64(),
                requested_slots = budget["slots"].as_u64(),
                status,
                accepted = budget_error.is_null(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "Local controller budget reconciliation"
            );
        }
    }
    s.store
        .transaction(move |db| {
            let mut record = match db.node(LOCAL_NODE_ID)? {
                Some(record) => record,
                None => ExecutionNode {
                    id: NodeId(uuid::Uuid::parse_str(LOCAL_NODE_ID).map_err(Error::internal)?),
                    name: "Current runner".into(),
                    local: true,
                    accepting: true,
                    created_at: now(),
                    limits: detected.limits(),
                    revision: id(),
                    ..ExecutionNode::default()
                },
            };
            record.system_tags = detected.tags();
            record.capabilities = detected;
            record.runtime_id = decode(health["runtimeId"].clone())?;
            record.execution_ready = health["status"] == "ok"
                && health["sharedResources"] == true
                && health["budget"] == json!({ "slots": record.slots(), "limits": record.limits });
            record.budget_error = decode(budget_error)?;
            record.exclusive_reservations = health["exclusiveReservations"] == true;
            record.last_seen = Some(now());
            record.runtime_slots = decode(health["pool"]["capacity"].clone())?;
            record.retained_runs = decode(health["pool"]["retained_runs"].clone())?;
            record.usage = decode(health["usage"].clone())?;
            record.pressure = decode(health["pressure"].clone())?;
            db.save_node(&record)?;
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
    let runner_stop = stop.child_token();
    let mut runner = tokio::spawn(async move {
        if crate::native::enabled() {
            crate::native::serve(runner_stop).await
        } else {
            crate::runner::serve(runner_stop).await
        }
    });
    let directory = directory.to_owned();
    // Bound before the connector starts, so its first request finds the gateway.
    if let Some(socket) = std::env::var_os("CAIRN_NODE_GATEWAY_SOCKET")
        && let Some(listener) = connector::bind_gateway(&directory).await?
    {
        let gateway_stop = stop.child_token();
        let socket = std::path::PathBuf::from(socket);
        tokio::spawn(async move {
            if let Err(error) = connector::serve_gateway(listener, socket, gateway_stop).await {
                tracing::warn!(%error, "Node gateway bridge stopped");
            }
        });
    }
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
