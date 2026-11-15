//! Rootful Docker owns a private network and /run. Slirp provides egress and
//! published IPv4 TCP/UDP ports without editing the node's firewall or bridges.
use crate::{
    error::{Error, Result},
    validation::text,
};
use axum::{Json, extract::State as ExtractState};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    net::Ipv4Addr,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, process::CommandExt},
    },
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

fn prepare_resolver_target(target: &Path, private_run: &Path) -> Result<()> {
    if !target.starts_with(private_run) {
        return Ok(());
    }
    let parent = target
        .parent()
        .ok_or_else(|| Error::unavailable("Invalid private resolver target."))?;
    filesystem(
        "prepare the private resolver directory",
        std::fs::create_dir_all(parent),
    )?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(Error::unavailable(format!(
            "Managed Docker could not prepare its private resolver target: {error}"
        ))),
    }
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
                // Receive host unmounts for inherited views of other runs'
                // disks, while preventing daemon mounts from reaching out.
                libc::MS_REC | libc::MS_SLAVE,
                std::ptr::null(),
            )
        },
        "isolate outgoing mount propagation",
    )?;
    // Bind sources must belong to the newly cloned mount namespace. FDs
    // opened before unshare still refer to the ancestor's mount objects.
    let runtime = filesystem("open its runtime directory", File::open(RUNTIME))?;
    let plugins = filesystem(
        "open its plugin directory",
        File::open("/run/docker/plugins"),
    )?;
    let resolver_target = filesystem(
        "resolve the host resolver target",
        std::fs::canonicalize("/etc/resolv.conf"),
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
    prepare_resolver_target(&resolver_target, Path::new("/run"))?;
    let resolver_target =
        std::ffi::CString::new(resolver_target.as_os_str().as_bytes()).map_err(Error::internal)?;
    checked(
        unsafe {
            libc::mount(
                c"/run/cairn-docker/resolv.conf".as_ptr(),
                resolver_target.as_ptr(),
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

// This runs after fork, before exec. Use only async-signal-safe syscalls and
// errno capture: the parent runtime can have other threads holding Rust locks.
fn isolate_slirp_mounts() -> std::io::Result<()> {
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_SLAVE,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
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
        let mut command = Command::new("/opt/cairn-docker/bin/slirp4netns");
        command
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
            .kill_on_drop(true);
        // Slirp's sandbox makes only / private, then detaches the old tree.
        // Shared submounts must become slaves before that recursive teardown,
        // otherwise it can propagate their removal back to the supervisor.
        unsafe {
            command.pre_exec(isolate_slirp_mounts);
        }
        let mut child = command.spawn()?;
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
    let response_limit = if command["execute"] == "list_hostfwd" {
        1_000_000
    } else {
        8192
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = tokio::net::UnixStream::connect(path).await?;
        stream.write_all(&serde_json::to_vec(&command)?).await?;
        stream.shutdown().await?;
        let mut bytes = Vec::new();
        let result = loop {
            let mut chunk = [0; 1024];
            let length = stream.read(&mut chunk).await?;
            if length == 0 || bytes.len() + length > response_limit {
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
        // The pinned slirp 1.3.6 implementation returns the listing directly,
        // unlike mutation replies (and the wrapped example in its manual).
        if command["execute"] == "list_hostfwd" && result.get("return").is_none() {
            Ok(result)
        } else {
            Ok(result["return"].clone())
        }
    }).await.map_err(|_| Error::timeout("Managed Docker port publication timed out."))?
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Publication {
    protocol: String,
    address: String,
    port: u16,
}

impl Publication {
    fn guest_address(&self) -> &str {
        if self.address == "0.0.0.0" {
            "127.0.0.1"
        } else {
            &self.address
        }
    }
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

struct Inner {
    client: reqwest::Client,
    api: std::path::PathBuf,
    reconciling: Mutex<()>,
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
                reconciling: Mutex::default(),
            }),
        })
    }

    async fn reconcile(&self) -> Result<()> {
        // A complete current inventory avoids name/ID alias races and frees a
        // naturally exited container's ports before a new start reuses them.
        let _reconciling = self.inner.reconciling.lock().await;
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
        let desired = publications(&containers)?
            .into_values()
            .flatten()
            .collect::<BTreeSet<_>>();
        // A mutation can commit before its reply is lost or cancelled. Query
        // the actual rules every pass instead of relying on received IDs.
        let inventory = call_api(&self.inner.api, json!({ "execute": "list_hostfwd" })).await?;
        let rules = inventory["entries"]
            .as_array()
            .ok_or_else(|| Error::unavailable("Invalid Docker forwarding inventory."))?;
        let mut retained = BTreeSet::new();
        // Retire obsolete, misdirected or duplicate rules before claiming ports.
        for rule in rules {
            let id = rule["id"]
                .as_u64()
                .ok_or_else(|| Error::unavailable("Invalid Docker forwarding identity."))?;
            let publication = Publication {
                protocol: text(rule, "proto").into(),
                address: text(rule, "host_addr").into(),
                port: rule["host_port"]
                    .as_u64()
                    .and_then(|port| u16::try_from(port).ok())
                    .filter(|port| *port > 0)
                    .ok_or_else(|| Error::unavailable("Invalid Docker forwarding port."))?,
            };
            let destination_matches = text(rule, "guest_addr") == publication.guest_address()
                && rule["guest_port"].as_u64() == Some(u64::from(publication.port));
            let keep = desired.contains(&publication)
                && destination_matches
                && retained.insert(publication);
            if !keep {
                call_api(
                    &self.inner.api,
                    json!({ "execute": "remove_hostfwd", "arguments": { "id": id } }),
                )
                .await?;
            }
        }
        for port in desired.difference(&retained) {
            let rule = call_api(
                &self.inner.api,
                json!({
                    "execute": "add_hostfwd",
                    "arguments": {
                        "proto": port.protocol,
                        "host_addr": port.address,
                        "host_port": port.port,
                        "guest_addr": port.guest_address(),
                        "guest_port": port.port,
                    },
                }),
            )
            .await?;
            if rule["id"].as_u64().is_none() {
                return Err(Error::unavailable("Invalid Docker forwarding receipt."));
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

    #[test]
    fn symlinked_resolver_target_is_recreated_only_in_the_private_run() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        let host = root.path().join("host-run");
        let etc = root.path().join("etc");
        let relative = Path::new("systemd/resolve/stub-resolv.conf");
        std::fs::create_dir_all(run.join(relative).parent().unwrap()).unwrap();
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::write(run.join(relative), b"nameserver 127.0.0.53\n").unwrap();
        symlink(run.join(relative), etc.join("resolv.conf")).unwrap();
        let target = std::fs::canonicalize(etc.join("resolv.conf")).unwrap();
        // Model mounting a fresh /run: the original host directory still
        // exists independently, while the symlink's pathname is now dangling.
        std::fs::rename(&run, &host).unwrap();
        std::fs::create_dir(&run).unwrap();
        assert!(!etc.join("resolv.conf").exists());
        prepare_resolver_target(&target, &run).unwrap();
        assert!(etc.join("resolv.conf").is_file());
        assert_eq!(
            std::fs::read(host.join(relative)).unwrap(),
            b"nameserver 127.0.0.53\n"
        );
        assert!(std::fs::read(target).unwrap().is_empty());
    }

    #[test]
    fn regular_host_resolver_contents_are_never_rewritten() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("resolv.conf");
        std::fs::write(&target, b"nameserver 192.0.2.53\n").unwrap();
        prepare_resolver_target(&target, &root.path().join("run")).unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"nameserver 192.0.2.53\n");
    }

    #[test]
    fn existing_private_resolver_target_is_not_truncated() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        std::fs::create_dir(&run).unwrap();
        let target = run.join("resolv.conf");
        std::fs::write(&target, b"private existing contents").unwrap();
        prepare_resolver_target(&target, &run).unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"private existing contents");
    }

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

    async fn recover_lost_mutation_reply(operation: &'static str) {
        crate::tls::install_provider();
        let root = tempfile::tempdir().unwrap();
        let docker_path = root.path().join("docker.sock");
        let api_path = root.path().join("slirp.sock");
        let inventory = Arc::new(Mutex::new(vec![json!({
            "Id": "a".repeat(64),
            "State": "running",
            "Ports": [{ "IP": "127.0.0.1", "PublicPort": 12345, "Type": "tcp" }],
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
        let rules = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let network = tokio::spawn({
            let rules = rules.clone();
            async move {
                let mut next = 1u64;
                let mut lost = false;
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut bytes = Vec::new();
                    stream.read_to_end(&mut bytes).await.unwrap();
                    let request: Value = serde_json::from_slice(&bytes).unwrap();
                    let command = text(&request, "execute");
                    let response = {
                        let mut rules = rules.lock().await;
                        match command {
                            "list_hostfwd" => {
                                let entries = rules.values().cloned().collect::<Vec<Value>>();
                                json!({ "entries": entries })
                            }
                            "add_hostfwd" => {
                                let mut rule = request["arguments"].clone();
                                rule["id"] = next.into();
                                rules.insert(next, rule);
                                next += 1;
                                json!({ "return": { "id": next - 1 } })
                            }
                            "remove_hostfwd" => {
                                let id = request["arguments"]["id"].as_u64().unwrap();
                                if rules.remove(&id).is_some() {
                                    json!({ "return": {} })
                                } else {
                                    json!({ "error": { "desc": "unknown rule" } })
                                }
                            }
                            _ => panic!("Unexpected slirp command"),
                        }
                    };
                    // Apply the mutation, then lose its acknowledgment. This
                    // models the same uncertainty as an expired reply deadline.
                    if command == operation && !lost {
                        lost = true;
                        continue;
                    }
                    stream
                        .write_all(&serde_json::to_vec(&response).unwrap())
                        .await
                        .unwrap();
                }
            }
        });
        let state = State::connect("fixture".into(), &docker_path, &api_path).unwrap();
        if operation == "add_hostfwd" {
            assert!(state.reconcile().await.is_err());
        } else {
            state.reconcile().await.unwrap();
        }
        assert_eq!(rules.lock().await.len(), 1);
        inventory.lock().await.clear();
        if operation == "remove_hostfwd" {
            assert!(state.reconcile().await.is_err());
        }
        state.reconcile().await.unwrap();
        assert!(
            rules.lock().await.is_empty(),
            "Stopped container retained its host publication"
        );
        network.abort();
        docker.abort();
    }

    #[tokio::test]
    async fn lost_add_reply_does_not_strand_a_stopped_containers_publication() {
        recover_lost_mutation_reply("add_hostfwd").await;
    }

    #[tokio::test]
    async fn lost_remove_reply_does_not_block_later_reconciliation() {
        recover_lost_mutation_reply("remove_hostfwd").await;
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
                let mut current: Option<Value> = None;
                let mut next = 1u64;
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut bytes = Vec::new();
                    stream.read_to_end(&mut bytes).await.unwrap();
                    let request: Value = serde_json::from_slice(&bytes).unwrap();
                    let operation = text(&request, "execute");
                    history.lock().await.push(operation.to_owned());
                    let response = if operation == "list_hostfwd" {
                        let entries = current.iter().cloned().collect::<Vec<_>>();
                        json!({ "entries": entries })
                    } else if operation == "add_hostfwd" {
                        assert!(
                            current.is_none(),
                            "New mapping claimed a port before retiring its previous owner"
                        );
                        assert_eq!(request["arguments"]["host_port"], 12345);
                        let (host, guest) = if next == 1 {
                            ("127.0.0.2", "127.0.0.2")
                        } else {
                            ("0.0.0.0", "127.0.0.1")
                        };
                        assert_eq!(request["arguments"]["host_addr"], host);
                        assert_eq!(request["arguments"]["guest_addr"], guest);
                        let mut rule = request["arguments"].clone();
                        rule["id"] = next.into();
                        current = Some(rule);
                        next += 1;
                        json!({ "return": { "id": next - 1 } })
                    } else {
                        assert_eq!(operation, "remove_hostfwd");
                        assert_eq!(request["arguments"]["id"], current.as_ref().unwrap()["id"]);
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
                "Ports": [{ "IP": "0.0.0.0", "PublicPort": 12345, "PrivatePort": 80, "Type": "tcp" }],
            }),
        ];
        state.reconcile().await.unwrap();
        assert_eq!(
            *history.lock().await,
            vec![
                "list_hostfwd",
                "add_hostfwd",
                "list_hostfwd",
                "remove_hostfwd",
                "add_hostfwd"
            ]
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
