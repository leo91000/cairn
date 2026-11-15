//! Managed Docker for cooperative administrator runs. The daemon inherits the
//! runner's mount namespace and containers stay below its delegated cgroup.
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

mod network;

pub use network::daemon;

pub(crate) const WRAPPER: &str = include_str!("../../../../../deploy/nodes/docker-administrator");

/// Accept only this attempt's service (or its delegated runner subgroup), never
/// an unrelated system service, a reused PID, or an agent-supplied parent.
fn parent(membership: &str, attempt: &str) -> Result<String> {
    let unit = super::super::systemd::unit(attempt)?;
    let group = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| Error::unavailable("Docker requires cgroup v2."))?;
    let (prefix, suffix) = group
        .split_once(&format!("/{unit}"))
        .ok_or_else(|| Error::forbidden("Docker must start in its native attempt's cgroup."))?;
    if !matches!(suffix, "" | "/runner") || !prefix.starts_with('/') {
        return Err(Error::forbidden(
            "Docker must start in its native attempt's cgroup.",
        ));
    }
    Ok(format!("{prefix}/{unit}"))
}

/// Before starting a retained store, disable legacy policies offline. Docker
/// must never activate a previous attempt's containers during daemon restore.
async fn quarantine(store: &Path, entry: &Path) -> Result<()> {
    let directory = store.join("unavailable-containers");
    tokio::fs::create_dir_all(&directory).await?;
    let destination = directory.join(crate::config::id());
    tokio::fs::rename(entry, destination).await?;
    tokio::fs::File::open(store.join("containers"))
        .await?
        .sync_all()
        .await?;
    tokio::fs::File::open(directory).await?.sync_all().await?;
    tracing::warn!(
        "A saved Docker container with incomplete metadata was retained in unavailable-containers"
    );
    Ok(())
}

async fn disable_restarts(store: &Path, group: &str) -> Result<()> {
    let mut entries = match tokio::fs::read_dir(store.join("containers")).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        if !super::containers::canonical(&entry.file_name().to_string_lossy())
            || !entry.file_type().await?.is_dir()
        {
            quarantine(store, &entry.path()).await?;
            continue;
        }
        let path = entry.path().join("hostconfig.json");
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if metadata.is_none_or(|metadata| !metadata.is_file() || metadata.len() > 1_000_000) {
            quarantine(store, &entry.path()).await?;
            continue;
        }
        let mut host = match super::config::decode(&tokio::fs::read(&path).await?) {
            Ok(value) if value.is_object() => value,
            _ => {
                quarantine(store, &entry.path()).await?;
                continue;
            }
        };
        if super::config::fields(&host, &["CgroupParent", "CgroupnsMode", "RestartPolicy"]).is_err()
        {
            quarantine(store, &entry.path()).await?;
            continue;
        }
        let object = host.as_object_mut().unwrap();
        object.insert("CgroupParent".into(), group.into());
        object.insert("CgroupnsMode".into(), "private".into());
        object.insert(
            "RestartPolicy".into(),
            json!({ "Name": "no", "MaximumRetryCount": 0 }),
        );
        crate::native::atomic_write(&path, &serde_json::to_vec(&host)?).await?;
    }
    Ok(())
}

pub async fn ensure(stop: tokio_util::sync::CancellationToken) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(Error::forbidden(
            "Managed administrator Docker requires sudo.",
        ));
    }
    let plan: Value = serde_json::from_slice(&tokio::fs::read("/run/cairn-plan.json").await?)?;
    if plan["nativeMode"]["mode"] != "administrator" {
        return Err(Error::forbidden("This is not an administrator attempt."));
    }
    let group = parent(
        &tokio::fs::read_to_string("/proc/self/cgroup").await?,
        text(&plan, "id"),
    )?;
    let runtime = Path::new(super::RUNTIME);
    // Separate CLI invocations race to start one daemon. Retain the flock until
    // readiness, and check the socket again after acquiring it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let _lock = loop {
        let client = crate::native::controller_client(Path::new(super::SOCKET))?;
        if client
            .get("http://localhost/_ping")
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        match crate::file_lock::exclusive(
            &runtime.join("startup.lock"),
            "Docker startup is active.",
        ) {
            Err(error) if error.is_conflict() && tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            result => break result?,
        }
    };
    let client = crate::native::controller_client(Path::new(super::SOCKET))?;
    if client
        .get("http://localhost/_ping")
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
    {
        return Ok(());
    }
    // A failed launch is not retried in the same attempt: two daemons must never
    // race over the retained store after an uncertain readiness acknowledgement.
    let _started = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(runtime.join("daemon.started"))
        .await?;
    disable_restarts(Path::new(super::DATA), &group).await?;
    let listener = tokio::net::UnixListener::bind("/run/docker/plugins/cairn-administrator.sock")?;
    let network_state = network::State::new(group.clone())?;
    let router = axum::Router::new()
        .route(
            "/Plugin.Activate",
            axum::routing::post(|| async { axum::Json(json!({ "Implements": ["authz"] })) }),
        )
        .route("/AuthZPlugin.AuthZReq", axum::routing::post(authorize))
        .route(
            "/AuthZPlugin.AuthZRes",
            axum::routing::post(network::response),
        )
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(network_state.clone());
    let plugin_stop = stop.child_token();
    let _plugin_guard = plugin_stop.clone().drop_guard();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(plugin_stop.cancelled_owned())
            .await;
    });
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/cairn-dockerd.log")?;
    tokio::fs::write(runtime.join("resolv.conf"), b"nameserver 10.0.2.3\n").await?;
    let mut daemon = Command::new(std::env::current_exe()?)
        .arg("native-administrator-daemon")
        .args([
            "--host=unix:///run/cairn-docker/docker.sock",
            "--data-root=/var/lib/cairn-docker",
            "--exec-root=/run/cairn-docker/exec",
            "--pidfile=/run/cairn-docker/dockerd.pid",
            "--config-file=/run/cairn-docker/daemon.json",
            "--exec-opt=native.cgroupdriver=cgroupfs",
            &format!("--cgroup-parent={group}"),
            "--live-restore=false",
            "--authorization-plugin=cairn-administrator",
            &format!(
                "--group={}",
                plan["nativeDockerGid"]
                    .as_u64()
                    .ok_or_else(|| Error::bad("Missing managed Docker group."))?
            ),
            "--ip=127.0.0.1",
            "--default-network-opt=bridge=com.docker.network.bridge.host_binding_ipv4=127.0.0.1",
        ])
        .env_clear()
        .env(
            "PATH",
            "/opt/cairn-docker/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", super::RUNTIME)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    let mut network = network::start(
        daemon
            .id()
            .ok_or_else(|| Error::unavailable("Managed Docker process is missing."))?,
    )
    .await?;
    loop {
        if let Some(status) = daemon.try_wait()? {
            return Err(Error::unavailable(format!(
                "Managed Docker exited ({status}); see /tmp/cairn-dockerd.log."
            )));
        }
        if client
            .get("http://localhost/_ping")
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::timeout(
                "Managed Docker startup timed out; see /tmp/cairn-dockerd.log.",
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    crate::native::atomic_write(&runtime.join("network.ready"), b"1").await?;
    drop(_lock);
    tokio::select! {
        result = daemon.wait() => {
            let status = result?;
            Err(Error::unavailable(format!("Managed Docker exited ({status}); see /tmp/cairn-dockerd.log.")))
        }
        result = network.wait() => {
            let status = result?;
            Err(Error::unavailable(format!(
                "Managed Docker network exited ({:?}); the run must be fenced.",
                status.code()
            )))
        }
        () = network_state.observe(stop.clone()) => Ok(()),
    }
}

pub async fn ready() -> Result<String> {
    let plan: Value = serde_json::from_slice(&tokio::fs::read("/run/cairn-plan.json").await?)?;
    let group = parent(
        &tokio::fs::read_to_string("/proc/self/cgroup").await?,
        text(&plan, "id"),
    )?;
    let client = crate::native::controller_client(Path::new(super::SOCKET))?;
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            let network_ready = tokio::fs::try_exists(Path::new(super::RUNTIME).join("network.ready")).await.unwrap_or(false);
            if network_ready {
                let response = client.get("http://localhost/_ping").timeout(Duration::from_secs(1)).send().await;
                if response.is_ok_and(|response| response.status().is_success()) { return; }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await.map_err(|_| Error::timeout("Managed Docker startup failed; see /tmp/cairn-docker-supervisor.log and /tmp/cairn-dockerd.log."))?;
    Ok(group)
}

fn configuration(body: &[u8], group: &str, create: bool) -> Result<()> {
    let value = super::config::decode(body)?;
    super::config::fields(&value, &["HostConfig", "CgroupParent", "RestartPolicy"])?;
    let host = if create { &value["HostConfig"] } else { &value };
    super::config::fields(
        host,
        &[
            "CgroupParent",
            "RestartPolicy",
            "CgroupnsMode",
            "PortBindings",
        ],
    )?;
    network::validate_ports(&host["PortBindings"])?;
    if host
        .get("CgroupnsMode")
        .is_some_and(|mode| !mode.is_null() && mode != "" && mode != "private")
    {
        return Err(Error::forbidden(
            "Managed Docker requires private container cgroup namespaces.",
        ));
    }
    if host
        .get("CgroupParent")
        .is_some_and(|parent| !parent.is_null() && parent != "" && parent != group)
    {
        return Err(Error::forbidden(
            "Docker containers must remain in this attempt's cgroup.",
        ));
    }
    super::config::fields(&host["RestartPolicy"], &["Name", "MaximumRetryCount"])?;
    if host["RestartPolicy"]
        .get("Name")
        .is_some_and(|name| !name.is_null() && name != "" && name != "no")
    {
        return Err(Error::forbidden(
            "Docker restart policies are unavailable; start services explicitly after resuming.",
        ));
    }
    Ok(())
}

fn authorize_request(request: &Value, group: &str) -> Result<()> {
    use base64::Engine;

    let path = super::api_path(text(request, "RequestUri").split('?').next().unwrap_or(""))?;
    if !(path.starts_with("/images/") || path.starts_with("/distribution/"))
        && (path.contains('%') || path.contains("//") || path.contains(".."))
    {
        return Err(Error::forbidden("Invalid Docker management path."));
    }
    let segments: Vec<_> = path.trim_start_matches('/').split('/').collect();
    match segments.first().copied() {
        Some(
            "_ping" | "version" | "info" | "events" | "system" | "distribution" | "containers"
            | "exec" | "volumes" | "networks" | "images" | "auth" | "commit",
        ) => {}
        _ => {
            return Err(Error::forbidden(
                "Use the managed docker-container Buildx driver for builds; orchestration and managed daemon plugins are unavailable.",
            ));
        }
    }
    let create = segments.as_slice() == ["containers", "create"];
    let update = segments.len() == 3 && segments[0] == "containers" && segments[2] == "update";
    if text(request, "RequestMethod") == "POST" && (create || update) {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(text(request, "RequestBody"))
            .map_err(Error::internal)?;
        configuration(&decoded, group, create)?;
    }
    Ok(())
}

async fn authorize(
    axum::extract::State(state): axum::extract::State<network::State>,
    body: axum::body::Bytes,
) -> axum::Json<Value> {
    let result = serde_json::from_slice(&body)
        .map_err(Error::from)
        .and_then(|request| authorize_request(&request, &state.group));
    axum::Json(match result {
        Ok(()) => json!({ "Allow": true }),
        Err(error) => json!({ "Allow": false, "Msg": error.message }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn administrator_docker_cannot_default_to_a_host_scope() {
        let attempt = crate::config::id();
        let unit = super::super::super::systemd::unit(&attempt).unwrap();
        let expected = format!("/workloads.slice/{unit}");
        for suffix in ["", "/runner"] {
            assert_eq!(
                parent(&format!("0::{expected}{suffix}\n"), &attempt).unwrap(),
                expected
            );
        }
        for group in [
            "/system.slice/docker.service".to_owned(),
            format!("{expected}-other"),
            format!("{expected}/escape"),
        ] {
            assert!(parent(&format!("0::{group}"), &attempt).is_err());
        }
    }

    #[test]
    fn builds_and_orchestration_cannot_bypass_container_lifecycle_checks() {
        for path in [
            "/build?cgroupparent=/system.slice",
            "/grpc",
            "/session",
            "/swarm/init",
            "/services/create",
            "/plugins/create",
            "/v1.56/containers/%63reate",
        ] {
            assert!(
                authorize_request(
                    &json!({ "RequestMethod": "POST", "RequestUri": path }),
                    "/current.service"
                )
                .is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn administrator_api_retains_privileges_but_cannot_escape_lifecycle_ownership() {
        for body in [
            r#"{"HostConfig":{"CgroupParent":"/system.slice"}}"#,
            r#"{"hostConfig":{"CgroupParent":"/system.slice"}}"#,
            r#"{"HostConfig":{"cgroupParent":"/system.slice"}}"#,
            r#"{"HostConfig":{"RestartPolicy":{"Name":"always"}}}"#,
            r#"{"HostConfig":{"RestartPolicy":{"name":"always"}}}"#,
            r#"{"HostConfig":{},"HostConfig":{"CgroupParent":"/system.slice"}}"#,
        ] {
            assert!(
                configuration(body.as_bytes(), "/current.service", true).is_err(),
                "{body}"
            );
        }
        configuration(br#"{"HostConfig":{"Privileged":true,"Binds":["/:/host"],"CgroupParent":"/current.service","RestartPolicy":{"Name":"no"}}}"#, "/current.service", true).unwrap();
        assert!(
            configuration(
                br#"{"RestartPolicy":{"Name":"always"}}"#,
                "/current.service",
                false
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn retained_containers_never_restart_implicitly_and_other_settings_survive() {
        let store = tempfile::tempdir().unwrap();
        let directory = store.path().join("containers").join("a".repeat(64));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let partial = store.path().join("containers").join("b".repeat(64));
        tokio::fs::create_dir_all(&partial).await.unwrap();
        let corrupt = store.path().join("containers").join("c".repeat(64));
        tokio::fs::create_dir_all(&corrupt).await.unwrap();
        tokio::fs::write(corrupt.join("hostconfig.json"), b"{")
            .await
            .unwrap();
        let path = directory.join("hostconfig.json");
        tokio::fs::write(
            &path,
            br#"{"RestartPolicy":{"Name":"always"},"Binds":["/home/node:/data"]}"#,
        )
        .await
        .unwrap();
        disable_restarts(store.path(), "/current.service")
            .await
            .unwrap();
        let saved: Value = serde_json::from_slice(&tokio::fs::read(path).await.unwrap()).unwrap();
        assert_eq!(saved["RestartPolicy"]["Name"], "no");
        assert_eq!(saved["CgroupParent"], "/current.service");
        assert_eq!(saved["Binds"], json!(["/home/node:/data"]));
        assert!(!partial.exists());
        assert!(!corrupt.exists());
        let quarantined = std::fs::read_dir(store.path().join("unavailable-containers"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(quarantined.len(), 2);
        assert!(quarantined.iter().any(|entry| {
            std::fs::read(entry.as_ref().unwrap().path().join("hostconfig.json"))
                .is_ok_and(|bytes| bytes == b"{")
        }));
    }
}
