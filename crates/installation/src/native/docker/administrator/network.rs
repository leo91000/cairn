//! Rootful Docker owns a private network and /run. Slirp provides egress and
//! published IPv4 TCP/UDP ports without editing the node's firewall or bridges.
use crate::{
    error::{Error, Result},
    validation::text,
};
use axum::{Json, extract::State as ExtractState};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::File,
    net::Ipv4Addr,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::Mutex,
};

const RUNTIME: &str = "/run/cairn-docker";
const API: &str = "/run/cairn-docker/slirp.sock";

fn checked(status: i32, operation: &str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(Error::unavailable(format!(
            "Managed Docker could not {operation}: {}",
            std::io::Error::last_os_error()
        )))
    }
}

fn filesystem<T>(operation: &str, result: std::io::Result<T>) -> Result<T> {
    result.map_err(|error| {
        Error::unavailable(format!("Managed Docker could not {operation}: {error}"))
    })
}

/// Called by main before any runtime threads. The bind descriptors retain only
/// this attempt's runtime and plugin directory across the private /run mount.
pub fn daemon(args: &[String]) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(Error::forbidden(
            "Managed Docker namespace setup requires root.",
        ));
    }
    let plan: Value = serde_json::from_slice(&filesystem(
        "read its namespace plan",
        std::fs::read("/run/cairn-plan.json"),
    )?)?;
    if plan["nativeMode"]["mode"] != "administrator" {
        return Err(Error::forbidden("This is not an administrator attempt."));
    }
    super::parent(
        &std::fs::read_to_string("/proc/self/cgroup")?,
        text(&plan, "id"),
    )?;
    checked(
        unsafe { libc::unshare(libc::CLONE_NEWNS | libc::CLONE_NEWNET) },
        "create private namespaces",
    )?;
    checked(
        unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        },
        "make mount propagation private",
    )?;
    // Bind sources must belong to the newly cloned mount namespace. FDs
    // opened before unshare still refer to the ancestor's mount objects.
    let runtime = filesystem("open its runtime directory", File::open(RUNTIME))?;
    let plugins = filesystem(
        "open its plugin directory",
        File::open("/run/docker/plugins"),
    )?;
    let cgroups = filesystem(
        "retain the workload cgroup mount",
        File::open("/sys/fs/cgroup"),
    )?;
    checked(
        unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                c"/run".as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"size=16m,mode=755".as_ptr().cast(),
            )
        },
        "mount the private runtime",
    )?;
    for (source, target) in [(runtime, RUNTIME), (plugins, "/run/docker/plugins")] {
        filesystem(
            "create a private runtime binding",
            std::fs::create_dir_all(target),
        )?;
        let source = std::ffi::CString::new(format!("/proc/self/fd/{}", source.as_raw_fd()))
            .map_err(Error::internal)?;
        let target = std::ffi::CString::new(target).map_err(Error::internal)?;
        checked(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            },
            "preserve runtime bindings",
        )?;
    }
    // Sysfs records the network namespace at mount time. Keeping the host's
    // sysfs would let bridge paths name host interfaces despite CLONE_NEWNET.
    checked(
        unsafe {
            libc::mount(
                c"sysfs".as_ptr(),
                c"/sys".as_ptr(),
                c"sysfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                std::ptr::null(),
            )
        },
        "mount private network sysfs",
    )?;
    checked(
        unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                c"/sys/fs".as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                c"size=1m,mode=755".as_ptr().cast(),
            )
        },
        "prepare private filesystem mountpoints",
    )?;
    filesystem(
        "create the workload cgroup binding",
        std::fs::create_dir_all("/sys/fs/cgroup"),
    )?;
    let source = std::ffi::CString::new(format!("/proc/self/fd/{}", cgroups.as_raw_fd()))
        .map_err(Error::internal)?;
    checked(
        unsafe {
            libc::mount(
                source.as_ptr(),
                c"/sys/fs/cgroup".as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        },
        "restore the workload cgroup mount",
    )?;
    checked(
        unsafe {
            libc::mount(
                c"/run/cairn-docker/resolv.conf".as_ptr(),
                c"/etc/resolv.conf".as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        },
        "bind the private resolver",
    )?;
    // Slirp host forwarding targets the daemon's loopback-bound Docker proxy.
    // This sysctl belongs only to the newly created network namespace.
    filesystem(
        "configure private loopback forwarding",
        std::fs::write("/proc/sys/net/ipv4/conf/all/route_localnet", b"1"),
    )?;
    // Docker protects loopback publications from packets on other interfaces.
    // Permit only the trusted slirp TAP to reach this private loopback; ordinary
    // container interfaces retain Docker's rules. No host rule is modified.
    let status = std::process::Command::new("iptables")
        .args([
            "-t",
            "raw",
            "-I",
            "PREROUTING",
            "1",
            "-i",
            "tap0",
            "-d",
            "127.0.0.0/8",
            "-j",
            "ACCEPT",
        ])
        .env_clear()
        .env(
            "PATH",
            "/opt/cairn-docker/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .status()
        .map_err(|error| {
            Error::unavailable(format!(
                "Managed Docker could not configure private forwarding: {error}"
            ))
        })?;
    if !status.success() {
        return Err(Error::unavailable(
            "Managed Docker could not configure its private loopback forwarding.",
        ));
    }
    filesystem(
        "record private namespace readiness",
        std::fs::write(Path::new(RUNTIME).join("namespace.ready"), b"1"),
    )?;
    let error = std::process::Command::new("/opt/cairn-docker/bin/dockerd")
        .args(args)
        .exec();
    Err(Error::unavailable(format!(
        "Managed Docker could not execute dockerd: {error}"
    )))
}

pub(super) async fn start(pid: u32) -> Result<Child> {
    use std::os::unix::fs::MetadataExt;

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if tokio::fs::try_exists(Path::new(RUNTIME).join("namespace.ready")).await? {
                let guest = std::fs::metadata(format!("/proc/{pid}/ns/net"))?;
                let host = std::fs::metadata("/proc/self/ns/net")?;
                if guest.ino() == host.ino() {
                    return Err(Error::unavailable(
                        "Managed Docker did not establish a private network namespace.",
                    ));
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/cairn-slirp.log")?;
        let mut child = Command::new("/opt/cairn-docker/bin/slirp4netns")
            .args([
                "--configure",
                "--disable-host-loopback",
                "--enable-sandbox",
                "--ready-fd=1",
                &format!("--api-socket={API}"),
                &pid.to_string(),
                "tap0",
            ])
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .spawn()?;
        let mut ready = [0];
        child
            .stdout
            .take()
            .ok_or_else(|| Error::internal("Missing network readiness pipe."))?
            .read_exact(&mut ready)
            .await?;
        Ok(child)
    })
    .await
    .map_err(|_| {
        Error::timeout("Managed Docker network setup timed out; see /tmp/cairn-slirp.log.")
    })?
}

pub(super) fn validate_ports(ports: &Value) -> Result<()> {
    if let Some(ports) = ports.as_object() {
        for (port, bindings) in ports {
            if !port.ends_with("/tcp") && !port.ends_with("/udp") {
                return Err(Error::forbidden(
                    "Managed administrator Docker publishes TCP/UDP ports only.",
                ));
            }
            if let Some(bindings) = bindings.as_array() {
                for binding in bindings {
                    super::super::config::fields(binding, &["HostIp", "HostPort"])?;
                    let address = text(binding, "HostIp");
                    if !address.is_empty()
                        && !address
                            .parse::<Ipv4Addr>()
                            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
                    {
                        return Err(Error::forbidden(
                            "Managed administrator Docker publishes on IPv4 loopback or wildcard addresses only.",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

async fn call_api(path: &Path, command: Value) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = tokio::net::UnixStream::connect(path).await?;
        stream.write_all(&serde_json::to_vec(&command)?).await?;
        stream.shutdown().await?;
        let mut bytes = Vec::new();
        let result = loop {
            let mut chunk = [0; 1024];
            let length = stream.read(&mut chunk).await?;
            if length == 0 || bytes.len() + length > 8192 {
                return Err(Error::unavailable("Managed Docker network response is missing or exceeds its limit."));
            }
            bytes.extend_from_slice(&chunk[..length]);
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(result) => break result,
                Err(error) if error.is_eof() => continue,
                Err(error) => return Err(error.into()),
            }
        };
        if result.get("error").is_some() {
            return Err(Error::unavailable("Managed Docker could not reserve the published host port; check for a conflicting listener."));
        }
        Ok(result["return"].clone())
    }).await.map_err(|_| Error::timeout("Managed Docker port publication timed out."))?
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Publication {
    protocol: String,
    address: String,
    port: u16,
}

fn publications(containers: &[Value]) -> Result<HashMap<String, Vec<Publication>>> {
    let mut desired = HashMap::new();
    for container in containers {
        if !matches!(text(container, "State"), "running" | "paused") {
            continue;
        }
        let id = text(container, "Id");
        if !super::super::containers::canonical(id) {
            return Err(Error::unavailable(
                "Invalid managed Docker container identity.",
            ));
        }
        let mut published = Vec::new();
        if let Some(ports) = container["Ports"].as_array() {
            for port in ports {
                let Some(number) = port["PublicPort"].as_u64() else {
                    continue;
                };
                let number = u16::try_from(number)
                    .ok()
                    .filter(|port| *port > 0)
                    .ok_or_else(|| Error::unavailable("Invalid published Docker port."))?;
                let protocol = text(port, "Type");
                let address = text(port, "IP");
                let binding = json!({ format!("{number}/{protocol}"): [{ "HostIp": address }] });
                validate_ports(&binding)?;
                if address.is_empty() {
                    return Err(Error::unavailable("Missing published Docker address."));
                }
                published.push(Publication {
                    protocol: protocol.into(),
                    address: address.into(),
                    port: number,
                });
            }
        }
        published.sort();
        published.dedup();
        if !published.is_empty() {
            desired.insert(id.into(), published);
        }
    }
    Ok(desired)
}

struct Forwarding {
    publications: Vec<Publication>,
    rules: Vec<u64>,
}

struct Inner {
    client: reqwest::Client,
    api: std::path::PathBuf,
    forwarded: Mutex<HashMap<String, Forwarding>>,
}

#[derive(Clone)]
pub(super) struct State {
    pub group: String,
    inner: Arc<Inner>,
}

impl State {
    pub fn new(group: String) -> Result<Self> {
        Self::connect(group, Path::new(super::super::SOCKET), Path::new(API))
    }

    fn connect(group: String, socket: &Path, api: &Path) -> Result<Self> {
        Ok(Self {
            group,
            inner: Arc::new(Inner {
                client: crate::native::controller_client(socket)?,
                api: api.to_owned(),
                forwarded: Mutex::default(),
            }),
        })
    }

    async fn reconcile(&self) -> Result<()> {
        // A complete current inventory avoids name/ID alias races and frees a
        // naturally exited container's ports before a new start reuses them.
        let mut forwarded = self.inner.forwarded.lock().await;
        let mut response = self
            .inner
            .client
            .get("http://localhost/containers/json?all=true")
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(Error::internal)?
            .error_for_status()
            .map_err(Error::internal)?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(Error::internal)? {
            if bytes.len() + chunk.len() > 1_000_000 {
                return Err(Error::unavailable(
                    "Managed Docker container inventory exceeds its limit.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let containers: Vec<Value> = serde_json::from_slice(&bytes)?;
        let desired = publications(&containers)?;
        // Retire every obsolete or incomplete mapping before claiming any new
        // port. Preserve failed receipts so later passes can finish cleanup.
        let mut removed = Vec::new();
        for (id, entry) in forwarded.iter_mut() {
            if desired.get(id) == Some(&entry.publications)
                && entry.rules.len() == entry.publications.len()
            {
                continue;
            }
            while let Some(rule) = entry.rules.last().copied() {
                call_api(
                    &self.inner.api,
                    json!({ "execute": "remove_hostfwd", "arguments": { "id": rule } }),
                )
                .await?;
                entry.rules.pop();
            }
            removed.push(id.clone());
        }
        for id in removed {
            forwarded.remove(&id);
        }
        for (id, ports) in desired {
            if forwarded.contains_key(&id) {
                continue;
            }
            let entry = forwarded.entry(id).or_insert_with(|| Forwarding {
                publications: ports.clone(),
                rules: Vec::new(),
            });
            for port in ports {
                let guest_address = if port.address == "0.0.0.0" {
                    "127.0.0.1"
                } else {
                    port.address.as_str()
                };
                let rule = call_api(
                    &self.inner.api,
                    json!({
                        "execute": "add_hostfwd",
                        "arguments": {
                            "proto": port.protocol,
                            "host_addr": port.address,
                            "host_port": port.port,
                            "guest_addr": guest_address,
                            "guest_port": port.port,
                        },
                    }),
                )
                .await?;
                entry.rules.push(
                    rule["id"]
                        .as_u64()
                        .ok_or_else(|| Error::unavailable("Invalid Docker forwarding receipt."))?,
                );
            }
        }
        Ok(())
    }

    pub async fn observe(&self, stop: tokio_util::sync::CancellationToken) {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut reported: Option<std::time::Instant> = None;
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                _ = timer.tick() => {
                    match self.reconcile().await {
                        Ok(()) => reported = None,
                        Err(error) if reported.is_none_or(|last| last.elapsed() >= Duration::from_secs(30)) => {
                            tracing::warn!(%error, "Managed Docker port reconciliation is incomplete");
                            reported = Some(std::time::Instant::now());
                        }
                        Err(_) => {}
                    }
                }
            }
        }
    }
}

pub(super) async fn response(
    ExtractState(state): ExtractState<State>,
    body: axum::body::Bytes,
) -> Json<Value> {
    let result = async {
        let request: Value = serde_json::from_slice(&body)?;
        if request["ResponseStatusCode"]
            .as_u64()
            .is_none_or(|code| !(200..300).contains(&code))
        {
            return Ok(());
        }
        let path =
            super::super::api_path(text(&request, "RequestUri").split('?').next().unwrap_or(""))?;
        let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
        let mutation = matches!(text(&request, "RequestMethod"), "POST" | "DELETE")
            && matches!(segments.first(), Some(&"containers" | &"networks"));
        if mutation {
            tokio::time::timeout(Duration::from_secs(5), state.reconcile())
                .await
                .map_err(|_| Error::timeout("Managed Docker port reconciliation timed out."))??;
        }
        Ok::<_, Error>(())
    }
    .await;
    Json(match result {
        Ok(()) => json!({ "Allow": true }),
        Err(error) => json!({ "Allow": false, "Msg": error.message }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slirp_control_accepts_a_fragmented_reply_without_waiting_for_eof() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("slirp.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["execute"], "add_hostfwd");
            stream.write_all(br#"{"return":{"id":"#).await.unwrap();
            tokio::task::yield_now().await;
            stream.write_all(b"7}}").await.unwrap();
            std::future::pending::<()>().await;
        });
        let result = call_api(&path, json!({ "execute": "add_hostfwd" }))
            .await
            .unwrap();
        assert_eq!(result["id"], 7);
        server.abort();
    }

    #[tokio::test]
    async fn exited_container_ports_are_released_before_reuse_and_without_a_client_mutation() {
        crate::tls::install_provider();
        let root = tempfile::tempdir().unwrap();
        let docker_path = root.path().join("docker.sock");
        let api_path = root.path().join("slirp.sock");
        let first = "a".repeat(64);
        let second = "b".repeat(64);
        let inventory = Arc::new(Mutex::new(vec![json!({
            "Id": first,
            "State": "running",
            "Ports": [{ "IP": "127.0.0.2", "PublicPort": 12345, "PrivatePort": 80, "Type": "tcp" }],
        })]));
        let listener = tokio::net::UnixListener::bind(&docker_path).unwrap();
        let router = axum::Router::new().route(
            "/containers/json",
            axum::routing::get({
                let inventory = inventory.clone();
                move || {
                    let inventory = inventory.clone();
                    async move { Json(inventory.lock().await.clone()) }
                }
            }),
        );
        let docker = tokio::spawn(async move { axum::serve(listener, router).await });
        let listener = tokio::net::UnixListener::bind(&api_path).unwrap();
        let history = Arc::new(Mutex::new(Vec::new()));
        let retired = Arc::new(tokio::sync::Notify::new());
        let network = tokio::spawn({
            let history = history.clone();
            let retired = retired.clone();
            async move {
                let mut current = None;
                let mut next = 1u64;
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut bytes = Vec::new();
                    stream.read_to_end(&mut bytes).await.unwrap();
                    let request: Value = serde_json::from_slice(&bytes).unwrap();
                    let operation = text(&request, "execute");
                    history.lock().await.push(operation.to_owned());
                    let response = if operation == "add_hostfwd" {
                        assert!(
                            current.is_none(),
                            "New mapping claimed a port before retiring its previous owner"
                        );
                        assert_eq!(request["arguments"]["host_port"], 12345);
                        assert_eq!(request["arguments"]["host_addr"], "127.0.0.2");
                        assert_eq!(request["arguments"]["guest_addr"], "127.0.0.2");
                        current = Some(next);
                        next += 1;
                        json!({ "return": { "id": current.unwrap() } })
                    } else {
                        assert_eq!(request["arguments"]["id"].as_u64(), current);
                        current = None;
                        retired.notify_one();
                        json!({ "return": {} })
                    };
                    stream
                        .write_all(&serde_json::to_vec(&response).unwrap())
                        .await
                        .unwrap();
                }
            }
        });
        let state = State::connect("fixture".into(), &docker_path, &api_path).unwrap();
        state.reconcile().await.unwrap();
        *inventory.lock().await = vec![
            json!({ "Id": first, "State": "exited", "Ports": [] }),
            json!({
                "Id": second,
                "State": "running",
                "Ports": [{ "IP": "127.0.0.2", "PublicPort": 12345, "PrivatePort": 80, "Type": "tcp" }],
            }),
        ];
        state.reconcile().await.unwrap();
        assert_eq!(
            *history.lock().await,
            vec!["add_hostfwd", "remove_hostfwd", "add_hostfwd"]
        );
        // Consume the first retirement's notification, then stop the second
        // container without any API mutation: only the observer can release it.
        retired.notified().await;
        inventory.lock().await.clear();
        let stop = tokio_util::sync::CancellationToken::new();
        let observer = tokio::spawn({
            let state = state.clone();
            let stop = stop.clone();
            async move { state.observe(stop).await }
        });
        tokio::time::timeout(Duration::from_secs(3), retired.notified())
            .await
            .unwrap();
        stop.cancel();
        observer.await.unwrap();
        assert!(state.inner.forwarded.lock().await.is_empty());
        network.abort();
        docker.abort();
    }

    #[test]
    fn only_running_or_paused_publications_are_retained() {
        let running = "a".repeat(64);
        let exited = "b".repeat(64);
        let paused = "c".repeat(64);
        let containers = vec![
            json!({ "Id": running, "State": "running", "Ports": [{ "IP": "127.0.0.1", "PublicPort": 12345, "PrivatePort": 80, "Type": "tcp" }] }),
            json!({ "Id": exited, "State": "exited", "Ports": [{ "IP": "127.0.0.1", "PublicPort": 12346, "Type": "tcp" }] }),
            json!({ "Id": paused, "State": "paused", "Ports": [{ "IP": "0.0.0.0", "PublicPort": 12347, "Type": "udp" }, { "PrivatePort": 80, "Type": "tcp" }] }),
        ];
        let desired = publications(&containers).unwrap();
        assert_eq!(desired.len(), 2);
        assert_eq!(desired[&running][0].port, 12345);
        assert_eq!(desired[&paused][0].protocol, "udp");
        assert!(!desired.contains_key(&exited));
    }

    #[test]
    fn publications_reject_unsupported_addresses_and_protocols_explicitly() {
        validate_ports(&json!({ "80/tcp": [{ "HostIp": "127.0.0.1", "HostPort": "0" }], "53/udp": [{ "HostIp": "0.0.0.0" }] })).unwrap();
        for ports in [
            json!({ "80/tcp": [{ "HostIp": "::1" }] }),
            json!({ "80/tcp": [{ "HostIp": "192.0.2.1" }] }),
            json!({ "80/sctp": [{ "HostIp": "127.0.0.1" }] }),
        ] {
            assert!(validate_ports(&ports).is_err());
        }
    }
}
