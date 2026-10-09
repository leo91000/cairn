//! Outbound node registration and presence. Credentials never enter process arguments.
use super::Capabilities;
use crate::{
    error::{Error, Result},
    skills::private_dir,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
    path::Path,
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

const HEARTBEAT_PERIOD: Duration = Duration::from_secs(3);
const NODE_PROTOCOL: u64 = 2;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identity {
    master: String,
    node_id: String,
    token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnrollRequest<'a> {
    code: &'a str,
    name: &'static str,
    protocol: u32,
    capabilities: Value,
    runtime_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeartbeatRequest<'a> {
    image_digest: Option<String>,
    runtime_id: String,
    execution_ready: bool,
    data_root: &'a Value,
    capabilities: &'a Value,
    runtimes: &'a Value,
    pool: &'a Value,
    usage: &'a Value,
    pressure: &'a Value,
    shared_resources: bool,
    budget: &'a Value,
    budget_error: &'a Value,
}

pub(crate) fn master(input: &str) -> Result<url::Url> {
    let value = url::Url::parse(input).map_err(|_| Error::bad("Invalid master URL."))?;
    let loopback = value.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !(value.scheme() == "https" || value.scheme() == "http" && loopback)
        || !value.username().is_empty()
        || value.password().is_some()
        || value.path() != "/"
        || value.query().is_some()
        || value.fragment().is_some()
    {
        return Err(Error::bad(
            "Use an HTTPS master origin (HTTP is allowed only on loopback for tests).",
        ));
    }
    Ok(value)
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(Error::internal)
}

fn runtime() -> String {
    std::env::var("APP_RUNTIME_ID")
        .unwrap_or_else(|_| format!("cairn-{}", env!("CARGO_PKG_VERSION")))
}

fn mi_b(blocks: libc::fsblkcnt_t, block_size: libc::c_ulong) -> u64 {
    (u128::from(blocks) * u128::from(block_size) / 1_048_576).min(u128::from(u64::MAX)) as u64
}

/// Memory in MiB, bounded by the cgroup limit when one applies.
fn memory_mi_b() -> Result<u64> {
    let mut memory = std::fs::read_to_string("/proc/meminfo")?
        .lines()
        .find_map(|line| {
            line.strip_prefix("MemTotal:")
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse::<u64>().ok())
        })
        .ok_or_else(|| Error::bad("Cannot detect node memory."))?
        / 1024;
    for limit in [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ] {
        if let Ok(value) = std::fs::read_to_string(limit)
            && let Ok(bytes) = value.trim().parse::<u64>()
        {
            memory = memory.min(bytes / 1_048_576);
        }
    }
    Ok(memory)
}

/// Read the namespace root quota, rather than the controller's configured leaf.
fn cpu_capacity(affinity: usize, quota: &str) -> Result<usize> {
    let mut fields = quota.split_whitespace();
    let quota = fields
        .next()
        .ok_or_else(|| Error::bad("Invalid node CPU quota."))?;
    let period = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|period| *period > 0)
        .ok_or_else(|| Error::bad("Invalid node CPU period."))?;
    if quota == "max" {
        return Ok(affinity);
    }
    let quota = quota.parse::<u64>().map_err(Error::internal)?;
    Ok(affinity.min((quota / period).max(1) as usize))
}

fn cpu_count() -> Result<usize> {
    let mut affinity = std::mem::MaybeUninit::<libc::cpu_set_t>::zeroed();
    if unsafe {
        libc::sched_getaffinity(
            0,
            std::mem::size_of::<libc::cpu_set_t>(),
            affinity.as_mut_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let count = unsafe { libc::CPU_COUNT(&affinity.assume_init()) } as usize;
    match std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
        Ok(quota) => cpu_capacity(count, &quota),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(std::thread::available_parallelism()?.get())
        }
        Err(error) => Err(error.into()),
    }
}

fn filesystem(path: &Path) -> Result<libc::statvfs> {
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::bad("Invalid node directory."))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { stat.assume_init() })
}

fn device_opens(path: &str) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .ok()
}

pub fn capabilities(path: &Path) -> Result<Value> {
    if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
        return Err(Error::bad("Nodes require Linux x86-64."));
    }
    // KVM_GET_API_VERSION must report the stable API version 12.
    let kvm = device_opens("/dev/kvm")
        .is_some_and(|file| unsafe { libc::ioctl(file.as_raw_fd(), 0xae00) } == 12);
    let memory = memory_mi_b()?;
    let stat = filesystem(path)?;
    let cpu = cpu_count()?;
    let detected = Capabilities {
        os: "linux".into(),
        arch: "x86_64".into(),
        kvm,
        fuse: device_opens("/dev/fuse").is_some(),
        cpu: u32::try_from(cpu).unwrap_or(u32::MAX),
        memory_mi_b: memory,
        disk_mi_b: mi_b(stat.f_bavail, stat.f_frsize),
        disk_total_mi_b: mi_b(stat.f_blocks, stat.f_frsize),
    };
    Ok(serde_json::to_value(detected)?)
}

pub async fn enroll(origin: &str, directory: &Path) -> Result<()> {
    let origin = master(origin)?;
    private_dir(directory).await?;
    let identity_path = directory.join("identity.json");
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&identity_path)
        .await
        .map_err(|_| {
            Error::conflict("Node identity already exists or its directory is not writable.")
        })?;
    let result = register(&origin, directory, &mut file).await;
    if result.is_err() {
        // The identity file is incomplete; the enrollment error is what matters.
        let _ = tokio::fs::remove_file(identity_path).await;
    }
    result?;
    println!("Node enrolled. Identity stored privately; no execution has been started.");
    Ok(())
}

async fn register(origin: &url::Url, directory: &Path, file: &mut tokio::fs::File) -> Result<()> {
    let code = crate::process::read_bounded(tokio::io::stdin(), 256).await?;
    let code = std::str::from_utf8(&code)
        .map_err(|_| Error::bad("Invalid enrollment code."))?
        .trim();
    let request = EnrollRequest {
        code,
        name: "Linux node",
        protocol: 1,
        capabilities: capabilities(directory)?,
        runtime_id: runtime(),
    };
    let response = client()?
        .post(
            origin
                .join("internal/nodes/enroll")
                .map_err(Error::internal)?,
        )
        .json(&request)
        .send()
        .await
        .map_err(|_| Error::unavailable("Cannot reach the master."))?;
    if !response.status().is_success() {
        return Err(Error::new(
            response.status().as_u16(),
            "Node enrollment was rejected. Check the code and master version.",
        ));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| Error::bad("Invalid enrollment response."))?;
    let node_id = value["nodeId"]
        .as_str()
        .ok_or_else(|| Error::bad("Missing node identity."))?;
    crate::validation::uuid(node_id)?;
    let token = value["token"]
        .as_str()
        .filter(|v| v.len() == 43)
        .ok_or_else(|| Error::bad("Missing node credential."))?;
    let identity = Identity {
        master: origin.to_string(),
        node_id: node_id.into(),
        token: token.into(),
    };
    file.write_all(&serde_json::to_vec(&identity)?).await?;
    file.sync_all().await?;
    Ok(())
}

async fn runner_credential() -> Result<String> {
    crate::execution::secret(&super::node_data_dir(), "runner-secret").await
}

pub async fn connect(directory: &Path, stop: CancellationToken) -> Result<()> {
    let identity: Identity =
        serde_json::from_slice(&tokio::fs::read(directory.join("identity.json")).await?)?;
    let origin = master(&identity.master)?;
    let client = client()?;
    let Ok(runner) = std::env::var("RUNNER_URL") else {
        return heartbeat(&client, &origin, &identity, &stop).await;
    };
    let relay_stop = stop.child_token();
    let mut relay = tokio::spawn(super::relay::run(
        origin.clone(),
        identity.token.clone(),
        runner,
        runner_credential().await?,
        relay_stop.clone(),
    ));
    let result = tokio::select! {
        result = heartbeat(&client, &origin, &identity, &stop) => result,
        result = &mut relay => {
            return match result {
                Ok(Ok(())) => Err(Error::unavailable("Execution relay stopped.")),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(Error::unavailable("Execution relay failed.")),
            };
        }
    };
    relay_stop.cancel();
    // The heartbeat result is reported; the relay only has to wind down.
    let _ = relay.await;
    result
}

/// The local controller's health report, or `null` when it is unreachable.
async fn local_health(client: &reqwest::Client, runner: &str) -> Value {
    if runner.is_empty() {
        return Value::Null;
    }
    match client
        .get(format!("{}/health", runner.trim_end_matches('/')))
        .timeout(Duration::from_secs(3))
        .send()
        .await
    {
        Ok(r) => r.json::<Value>().await.unwrap_or_default(),
        Err(_) => Value::Null,
    }
}

/// A refused desired budget must not hide a healthy controller's heartbeat.
pub(super) async fn budget_error(
    result: std::result::Result<reqwest::Response, reqwest::Error>,
) -> Value {
    match result {
        Ok(response) if response.status().is_success() => Value::Null,
        Ok(response) => {
            let status = response.status();
            let body = response.json::<Value>().await.unwrap_or_default();
            body["error"]
                .as_str()
                .map_or_else(
                    || format!("Controller rejected shared budget ({status}); retrying."),
                    str::to_owned,
                )
                .into()
        }
        Err(_) => "Controller could not apply shared budget; retrying.".into(),
    }
}

async fn heartbeat(
    client: &reqwest::Client,
    origin: &url::Url,
    identity: &Identity,
    stop: &CancellationToken,
) -> Result<()> {
    let mut budget_error_value = Value::Null;
    loop {
        let started = super::boot_ms();
        let runner = std::env::var("RUNNER_URL").unwrap_or_default();
        let health = local_health(client, &runner).await;
        let ready = health["nodeProtocol"] == NODE_PROTOCOL
            && health["dataRoot"] == super::node_data_dir().to_string_lossy().as_ref();
        let report = HeartbeatRequest {
            image_digest: std::env::var("CAIRN_NODE_IMAGE").ok(),
            runtime_id: health["runtimeId"]
                .as_str()
                .map_or_else(runtime, str::to_owned),
            execution_ready: ready,
            data_root: &health["dataRoot"],
            capabilities: &health["capabilities"],
            runtimes: &health["runtimes"],
            pool: &health["pool"],
            usage: &health["usage"],
            pressure: &health["pressure"],
            shared_resources: health["sharedResources"] == true,
            budget: &health["budget"],
            budget_error: &budget_error_value,
        };
        let request = client
            .post(
                origin
                    .join("internal/nodes/heartbeat")
                    .map_err(Error::internal)?,
            )
            .bearer_auth(&identity.token)
            .json(&report)
            .send();
        let result = tokio::select! {
            () = stop.cancelled() => return Ok(()),
            value = request => value,
        };
        match result {
            Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
                return Err(Error::unauthorized(
                    "Node identity revoked. Register the node again.",
                ));
            }
            Ok(response) if response.status().is_success() => {
                let value: Value = response
                    .json()
                    .await
                    .map_err(|_| Error::bad("Invalid master heartbeat."))?;
                if ready {
                    forward_leases(client, &runner, &value, started).await?;
                    let budget = json!({ "slots": value["slots"], "limits": value["limits"] });
                    if health["budget"].is_object() && health["budget"] != budget {
                        let response = client
                            .post(format!("{}/node-budget", runner.trim_end_matches('/')))
                            .bearer_auth(runner_credential().await?)
                            .json(&budget)
                            .send()
                            .await;
                        budget_error_value = budget_error(response).await;
                    } else {
                        budget_error_value = Value::Null;
                    }
                }
            }
            _ => tracing::warn!("Node heartbeat failed; retrying without starting work."),
        }
        tokio::select! {
            () = stop.cancelled() => return Ok(()),
            () = tokio::time::sleep(HEARTBEAT_PERIOD) => {}
        }
    }
}

/// Passes the master's lease grants to the local controller, minus the time already spent.
async fn forward_leases(
    client: &reqwest::Client,
    runner: &str,
    heartbeat: &Value,
    started: u64,
) -> Result<()> {
    let credential = runner_credential().await?;
    for lease in heartbeat["leases"].as_array().into_iter().flatten() {
        let elapsed = super::boot_ms().saturating_sub(started);
        let remaining = lease["remainingMs"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(elapsed);
        let attempt = crate::validation::text(lease, "id");
        if remaining == 0 || crate::validation::uuid(attempt).is_err() {
            continue;
        }
        // An expired or refused lease stops the VM; the next heartbeat retries.
        let _ = client
            .post(format!(
                "{}/runs/{attempt}/lease",
                runner.trim_end_matches('/')
            ))
            .bearer_auth(&credential)
            .json(&json!({ "remainingMs": remaining }))
            .timeout(Duration::from_secs(3))
            .send()
            .await;
    }
    Ok(())
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn parent_cpu_capacity_tracks_resize_and_affinity_independently_of_leaf_budget() {
        assert_eq!(cpu_capacity(32, "800000 100000").unwrap(), 8);
        assert_eq!(cpu_capacity(32, "1600000 100000").unwrap(), 16);
        assert_eq!(cpu_capacity(32, "400000 100000").unwrap(), 4);
        assert_eq!(cpu_capacity(2, "800000 100000").unwrap(), 2);
        assert_eq!(cpu_capacity(32, "max 100000").unwrap(), 32);
        assert!(cpu_capacity(32, "800000 0").is_err());
    }
}
