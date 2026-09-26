//! Trusted node identities. Enrollment and revocation are serialized with heartbeats.
pub mod archive;
pub mod backups;
pub mod checkpoint;
pub mod connector;
pub mod executor;
pub mod files;
pub mod maintenance;
pub mod moves;
pub mod placement;
pub mod relay;
pub mod restore;
pub mod snapshots;
pub mod transport;
pub mod workspace;
use crate::{
    auth::{digest, token},
    config::{id, now},
    error::{Error, Result},
    http::{App, Input},
    service::Service,
};
use axum::{
    Json,
    extract::{Request, State},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub const LOCAL_NODE_ID: &str = "00000000-0000-4000-8000-000000000002";
const HEARTBEAT_TIMEOUT_MS: i64 = 60_000;

/// Fail before queueing only when no execution location is authorized.
pub fn require_node(agent: &Value) -> Result<()> {
    if crate::service::policy(agent)["nodes"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        return Err(Error::new(
            403,
            "This agent has no authorized execution node.",
        ));
    }
    Ok(())
}

#[derive(Deserialize, serde::Serialize, Clone)]
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
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Capabilities {
    os: String,
    arch: String,
    kvm: bool,
    cpu: u32,
    memory_mi_b: u64,
    disk_mi_b: u64,
}
impl Capabilities {
    fn validate(&self) -> Result<()> {
        if self.os != "linux" || self.arch != "x86_64" {
            return Err(Error::bad("Nodes require Linux x86-64."));
        }
        self.resources().validate()
    }
    fn resources(&self) -> Resources {
        Resources {
            cpu: self.cpu,
            memory_mi_b: self.memory_mi_b,
            disk_mi_b: self.disk_mi_b,
        }
    }
    fn limits(&self) -> Resources {
        Resources {
            cpu: self.cpu.saturating_sub(1).max(1),
            memory_mi_b: self.memory_mi_b.saturating_sub(512).max(128),
            disk_mi_b: (self.disk_mi_b * 4 / 5).max(128),
        }
    }
    fn tags(&self) -> Value {
        if self.kvm {
            json!(["linux", "x86_64", "kvm"])
        } else {
            json!(["linux", "x86_64"])
        }
    }
    fn value(&self) -> Value {
        let mut value = json!(self.resources());
        value["os"] = self.os.clone().into();
        value["arch"] = self.arch.clone().into();
        value["kvm"] = self.kvm.into();
        value
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
    node["status"] = if node["revoked"] == true {
        "revoked"
    } else if node["local"] == true {
        "local"
    } else if node["lastSeen"]
        .as_i64()
        .is_some_and(|seen| now() - seen < HEARTBEAT_TIMEOUT_MS)
    {
        "online"
    } else {
        "offline"
    }
    .into();
    node
}
pub async fn admin(s: &Service, input: &Input) -> Result<Value> {
    let segments = input
        .path
        .trim_start_matches("/api/")
        .split('/')
        .collect::<Vec<_>>();
    if input.method == "GET" {
        let _ = refresh_local(s).await;
    }
    match (input.method.as_str(), segments.as_slice()) {
        ("GET" | "PUT", ["nodes", "placement", run]) => {
            placement::configure(
                s,
                run,
                if input.method == "PUT" {
                    Some(input.body.clone())
                } else {
                    None
                },
            )
            .await
        }
        ("POST", ["nodes", "placement", run, "move"]) => {
            crate::validation::uuid(run)?;
            moves::request(s, &s.store.run(run).await?, &input.body).await
        }

        ("GET", ["nodes", "settings"]) => backups::settings(s).await,
        ("PUT", ["nodes", "settings"]) => {
            #[derive(Deserialize, serde::Serialize)]
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct Settings {
                destination: String,
                interval_seconds: u64,
                retention: u64,
                budget_mi_b: u64,
                #[serde(default)]
                disconnect_timeout_seconds: Option<u64>,
                #[serde(default)]
                shutdown_timeout_seconds: Option<u64>,
                #[serde(default)]
                max_capacity_wait_seconds: Option<u64>,
            }
            let settings: Settings = decode(input.body.clone())?;
            if !["master", "s3"].contains(&settings.destination.as_str())
                || !(5..=3600).contains(&settings.interval_seconds)
                || !(1..=100).contains(&settings.retention)
                || !(128..=1_048_576).contains(&settings.budget_mi_b)
                || settings
                    .disconnect_timeout_seconds
                    .is_some_and(|v| !(10..=300).contains(&v))
                || settings
                    .shutdown_timeout_seconds
                    .is_some_and(|v| !(30..=300).contains(&v))
                || settings.max_capacity_wait_seconds.is_some_and(|v| v > 3600)
            {
                return Err(Error::bad("Invalid recovery settings."));
            }
            if settings.destination == "s3" {
                crate::archive_storage::Storage::configured(s)?
                    .validate()
                    .await?;
            }
            let mut value = backups::settings(s).await?;
            for (key, item) in json!(settings).as_object().unwrap() {
                if !item.is_null() {
                    value[key] = item.clone();
                }
            }
            s.store
                .set("node-backup-settings", value.clone(), None)
                .await?;
            s.store
                .audit("node.recovery.configured", value.clone())
                .await?;
            Ok(value)
        }
        ("GET", ["nodes", "backups", run]) => {
            crate::validation::uuid(run)?;
            Ok(s.store
                .list("node-backups")
                .await?
                .into_iter()
                .filter(|point| point["runId"] == *run)
                .map(backups::public)
                .collect::<Vec<_>>()
                .into())
        }

        ("GET", ["nodes"]) => {
            let attempts = s.store.list("node-attempts").await?;
            let volumes = s.store.list("node-volumes").await?;
            Ok(s.store
                .list("nodes")
                .await?
                .into_iter()
                .map(|mut node| {
                    for key in ["cpu", "memoryMiB", "diskMiB"] {
                        let used = if key == "diskMiB" {
                            volumes
                                .iter()
                                .filter(|v| v["nodeId"] == node["id"])
                                .map(|v| v["diskMiB"].as_u64().unwrap_or(0))
                                .sum::<u64>()
                        } else {
                            attempts
                                .iter()
                                .filter(|a| a["nodeId"] == node["id"] && a["released"] != true)
                                .map(|a| a["resources"][key].as_u64().unwrap_or(0))
                                .sum::<u64>()
                        };
                        node["reserved"][key] = used.into();
                        node["available"][key] = node["limits"][key]
                            .as_u64()
                            .unwrap_or(0)
                            .saturating_sub(used)
                            .into();
                    }
                    public(node)
                })
                .collect::<Vec<_>>()
                .into())
        }
        ("PUT", ["nodes", node]) => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Configuration {
                name: String,
                tags: Vec<String>,
                limits: Resources,
                accepting: bool,
            }
            let request: Configuration = decode(input.body.clone())?;
            let label = name(&request.name)?;
            request.limits.validate()?;
            if request.tags.len() > 32
                || request.tags.iter().any(|tag| {
                    tag.is_empty()
                        || tag.len() > 40
                        || !tag
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_:./".contains(&b))
                })
            {
                return Err(Error::bad(
                    "Use at most 32 tags of 1 to 40 letters, digits or -_:./.",
                ));
            }
            let node = (*node).to_owned();
            s.store
                .transaction(move |db| {
                    let mut record = db
                        .get("nodes", &node)?
                        .ok_or_else(|| Error::new(404, "Node not found."))?;
                    if record["revoked"] == true {
                        return Err(Error::new(409, "This node is revoked."));
                    }
                    let resources = json!(request.limits);
                    for key in ["cpu", "memoryMiB", "diskMiB"] {
                        if resources[key].as_u64() > record["capabilities"][key].as_u64() {
                            return Err(Error::bad("Limits exceed the node's detected capacity."));
                        }
                    }
                    record["name"] = label.into();
                    record["tags"] = json!(request.tags);
                    record["limits"] = resources;
                    record["accepting"] = request.accepting.into();
                    db.put("nodes", &record)?;
                    db.audit("node.configured", &json!({"nodeId":node}))?;
                    Ok(public(record))
                })
                .await
        }
        ("POST", ["nodes", "enrollments"]) => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Invitation {
                name: String,
            }
            let request: Invitation = decode(input.body.clone())?;
            let name = name(&request.name)?;
            let code = token();
            let expires = now() + 600_000;
            s.store
                .set(
                    &format!("node-enrollment:{}", digest(&code)),
                    json!({"name":name}),
                    Some(expires),
                )
                .await?;
            let install = maintenance::release()
                .ok()
                .and_then(|_| connector::master(&s.config.public_url).ok())
                .map(|origin| {
                    format!(
                        "curl --fail --silent --show-error '{}' | sudo bash",
                        format!("{}internal/nodes/install.sh", origin).replace('\'', "'\\''")
                    )
                });
            Ok(json!({"code":code,"expiresAt":expires,"installCommand":install}))
        }
        ("POST", ["nodes", node, "revoke"]) => {
            let node = (*node).to_owned();
            s.store
                .transaction(move |db| {
                    let mut record = db
                        .get("nodes", &node)?
                        .ok_or_else(|| Error::new(404, "Node not found."))?;
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
                        if let Some(nodes) = agent["access"]["nodes"].as_array_mut() {
                            let before = nodes.len();
                            nodes.retain(|id| id != &node);
                            if nodes.len() != before {
                                db.put("agents", &agent)?;
                            }
                        }
                    }
                    db.put("nodes", &record)?;
                    db.audit("node.revoked", &json!({"nodeId":node}))?;
                    Ok(public(record))
                })
                .await
        }
        _ => Err(Error::new(404, "Not found")),
    }
}
pub async fn internal(State(app): State<App>, request: Request) -> Result<Json<Value>> {
    let input = Input::read(request).await?;
    let s = &app.service;
    if input.method != "POST" {
        return Err(Error::new(405, "Method not allowed."));
    }
    match input.path.as_str() {
        "/internal/nodes/maintenance" => {
            let node = transport::authenticate(s, &input.headers).await?;
            Ok(Json(maintenance::request(s, &node, &input.body).await?))
        }
        "/internal/nodes/poll" | "/internal/nodes/reply" => {
            let node = transport::authenticate(s, &input.headers).await?;
            Ok(Json(if input.path.ends_with("/poll") {
                s.node_transport.poll(&node).await?
            } else {
                s.node_transport.reply(&node, input.body).await?
            }))
        }
        "/internal/nodes/enroll" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct Enrollment {
                code: String,
                name: String,
                protocol: u32,
                capabilities: Capabilities,
                runtime_id: String,
            }
            let request: Enrollment = decode(input.body)?;
            if request.protocol != 1 {
                return Err(Error::new(409, "Unsupported node protocol."));
            }
            request.capabilities.validate()?;
            name(&request.name)?;
            let runtime = name(&request.runtime_id)?;
            if request.code.len() != 43 {
                return Err(Error::new(401, "Invalid or expired enrollment code."));
            }
            let key = format!("node-enrollment:{}", digest(&request.code));
            let node_id = id();
            let credential = token();
            let token_key = format!("node-token:{}", digest(&credential));
            let value = s.store.transaction(move |db| {
                let invitation = db.kv(&key)?.ok_or_else(|| Error::new(401, "Invalid or expired enrollment code."))?;
                db.delete(&key)?;
                let node = json!({"id":node_id,"name":invitation["name"],"local":false,"accepting":false,"revoked":false,"tags":[],"systemTags":request.capabilities.tags(),"capabilities":request.capabilities.value(),"limits":request.capabilities.limits(),"runtimeId":runtime,"lastSeen":now(),"createdAt":now()});
                db.put("nodes", &node)?;
                db.set(&token_key, &json!(node_id), None)?;
                db.audit("node.enrolled", &json!({"nodeId":node_id}))?;
                Ok(json!({"nodeId":node_id,"token":credential,"heartbeatIntervalMs":10_000,"disconnectTimeoutMs":HEARTBEAT_TIMEOUT_MS}))
            }).await?;
            Ok(Json(value))
        }
        "/internal/nodes/heartbeat" => {
            let credential = input
                .headers
                .get("authorization")
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.strip_prefix("Bearer "))
                .filter(|token| token.len() == 43)
                .ok_or_else(|| Error::new(401, "Invalid node identity."))?;
            let key = format!("node-token:{}", digest(credential));
            let expected_data = s.config.data_dir.to_string_lossy().into_owned();
            let expected_image = maintenance::release().ok().map(|r| r["image"].clone());
            let lease_ms = backups::settings(s).await?["disconnectTimeoutSeconds"]
                .as_i64()
                .unwrap_or(60)
                * 1000;
            let value = s.store.transaction(move |db| {
                let node_id = db.kv(&key)?.and_then(|v| v.as_str().map(str::to_owned)).ok_or_else(|| Error::new(401, "Invalid node identity."))?;
                let mut node = db.get("nodes", &node_id)?.filter(|v| v["revoked"] != true).ok_or_else(|| Error::new(401, "Invalid node identity."))?;
                if let Some(runtime) = input.body.get("runtimeId") {
                    node["runtimeId"] = name(runtime.as_str().unwrap_or(""))?.into();
                }
                node["runtimes"]=input.body["runtimes"].clone();
                node["lastSeen"] = now().into();
                node["imageDigest"]=input.body["imageDigest"].clone();
                node["executionReady"]=(input.body["executionReady"]==true && input.body["dataRoot"]==expected_data && expected_image.as_ref().is_none_or(|image|node["imageDigest"]==*image || node["updateError"].is_string())).into();
                db.put("nodes", &node)?;
                let mut leases=Vec::new();
                for mut attempt in db.list("node-attempts")? {
                    if attempt["nodeId"]!=node_id || attempt["released"]==true {continue;}
                    let run=db.run(crate::validation::text(&attempt,"runId"))?.unwrap_or_default();
                    let checkpoint=db.kv(&format!("run-checkpoint:{}",crate::validation::text(&attempt,"runId")))?.unwrap_or_default();
                    let agent=db.get("agents",crate::validation::text(&run["snapshot"]["agent"],"id"))?.unwrap_or_default();
                    if run["status"]=="running" && run["cancelRequestedAt"].is_null() && checkpoint["runnerId"]==attempt["id"] && crate::service::allowed(&crate::service::policy(&agent)["nodes"],&node_id) {
                        attempt["leaseExpiresAt"]=(now()+lease_ms).into();
                        attempt["leaseDurationMs"]=attempt["leaseDurationMs"].as_i64().unwrap_or(0).max(lease_ms).into();
                        db.put("node-attempts",&attempt)?;
                        leases.push(json!({"id":attempt["id"],"remainingMs":lease_ms}));
                    }
                }
                Ok(json!({"nodeId":node_id,"accepting":node["accepting"],"limits":node["limits"],"leases":leases,"heartbeatIntervalMs":10_000,"disconnectTimeoutMs":HEARTBEAT_TIMEOUT_MS}))
            }).await?;
            for lease in value["leases"].as_array().into_iter().flatten() {
                record_lease(
                    s,
                    crate::validation::text(lease, "id"),
                    lease["remainingMs"].as_u64().unwrap_or(60000),
                )
                .await;
            }
            Ok(Json(value))
        }
        _ => Err(Error::new(404, "Not found")),
    }
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
        .map_err(|_| Error::new(503, "Local runner unavailable."))?
        .json::<Value>()
        .await
        .map_err(Error::internal)?;
    let capabilities = health["capabilities"].clone();
    if !capabilities.is_object() {
        return Ok(());
    }
    let detected: Capabilities = decode(capabilities.clone())?;
    s.store.transaction(move |db| {
        let mut record=db.get("nodes",LOCAL_NODE_ID)?.unwrap_or_else(||json!({"id":LOCAL_NODE_ID,"name":"Current runner","local":true,"revoked":false,"accepting":true,"tags":[],"createdAt":now(),"limits":detected.limits()}));
        record["systemTags"]=detected.tags();record["runtimes"]=health["runtimes"].clone();record["capabilities"]=capabilities;record["runtimeId"]=health["runtimeId"].clone();record["executionReady"]=(health["status"]=="ok").into();record["lastSeen"]=now().into();db.put("nodes",&record)?;Ok(())
    }).await
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
    let (runner_first, result) =
        tokio::select! {result=&mut runner=>(true,result),result=&mut connector=>(false,result)};
    stop.cancel();
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
