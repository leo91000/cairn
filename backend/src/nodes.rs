//! Trusted node identities. Enrollment and revocation are serialized with heartbeats.
pub mod connector;
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

/// The existing runner remains the only execution adapter until remote execution
/// is installed. Never fall back to it when the agent only permits remote nodes.
pub fn require_local(agent: &Value) -> Result<()> {
    if !crate::service::allowed(&crate::service::policy(agent)["nodes"], LOCAL_NODE_ID) {
        return Err(Error::new(
            403,
            "This agent cannot use the current runner. Remote execution is not available yet.",
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
    fn validate(&self) -> Result<()> {
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
    fn value(&self) -> Value {
        let mut value = json!(self.resources());
        value["os"] = self.os.clone().into();
        value["arch"] = self.arch.clone().into();
        value["kvm"] = self.kvm.into();
        value
    }
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
    match (input.method.as_str(), segments.as_slice()) {
        ("GET", ["nodes"]) => Ok(s
            .store
            .list("nodes")
            .await?
            .into_iter()
            .map(public)
            .collect::<Vec<_>>()
            .into()),
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
            Ok(json!({"code":code,"expiresAt":expires}))
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
                let node = json!({"id":node_id,"name":invitation["name"],"local":false,"accepting":false,"revoked":false,"tags":[],"capabilities":request.capabilities.value(),"limits":request.capabilities.resources(),"runtimeId":runtime,"lastSeen":now(),"createdAt":now()});
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
            let value = s.store.transaction(move |db| {
                let node_id = db.kv(&key)?.and_then(|v| v.as_str().map(str::to_owned)).ok_or_else(|| Error::new(401, "Invalid node identity."))?;
                let mut node = db.get("nodes", &node_id)?.filter(|v| v["revoked"] != true).ok_or_else(|| Error::new(401, "Invalid node identity."))?;
                if let Some(runtime) = input.body.get("runtimeId") {
                    node["runtimeId"] = name(runtime.as_str().unwrap_or(""))?.into();
                }
                node["lastSeen"] = now().into();
                db.put("nodes", &node)?;
                Ok(json!({"nodeId":node_id,"accepting":node["accepting"],"limits":node["limits"],"heartbeatIntervalMs":10_000,"disconnectTimeoutMs":HEARTBEAT_TIMEOUT_MS}))
            }).await?;
            Ok(Json(value))
        }
        _ => Err(Error::new(404, "Not found")),
    }
}
