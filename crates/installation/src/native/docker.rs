//! Lazy Docker inside the conversation's RootlessKit enclave. The host daemon
//! is never involved. Docker's mandatory authorization plugin bounds its API
//! to the same files and permissions as the engine.
use crate::{
    error::{Error, Result},
    validation::text,
};
use axum::{Json, Router, extract::State, routing::post};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

mod config;
mod hardening;
mod mounts;
mod proxy;

const RUNTIME: &str = "/run/cairn-docker";
const SOCKET: &str = "/run/cairn-docker/docker.sock";
const DATA: &str = "/var/lib/cairn-docker";
const DOCKER: &str = "/opt/cairn-docker/bin/docker";

#[derive(Clone)]
struct Policy {
    plan: Arc<Value>,
}

fn present(value: &Value, name: &str) -> bool {
    match &value[name] {
        Value::Null => false,
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        Value::String(value) => !value.is_empty(),
        Value::Bool(value) => *value,
        _ => true,
    }
}

impl Policy {
    fn roots(&self) -> Vec<PathBuf> {
        vec![
            PathBuf::from(text(&self.plan, "projectRoot")),
            "/home/node".into(),
            "/tmp".into(),
            PathBuf::from(text(&self.plan, "guestDataRoot"))
                .join("runs")
                .join(text(&self.plan, "runId"))
                .join("output"),
        ]
    }

    fn bind(&self, source: &str, read_only: bool) -> Result<()> {
        if Path::new(source).starts_with(Path::new(RUNTIME).join("mounts")) {
            return mounts::validate(self, Path::new(source), read_only);
        }
        self.bind_original(source, read_only)
    }

    fn bind_original(&self, source: &str, read_only: bool) -> Result<()> {
        let path = Path::new(source);
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(Error::forbidden(
                "Docker bind sources must be absolute conversation paths.",
            ));
        }
        let workspace = Path::new(text(&self.plan, "projectRoot"));
        if !self.roots().iter().any(|root| path.starts_with(root)) {
            return Err(Error::forbidden(
                "Docker bind source is outside this conversation.",
            ));
        }
        if crate::execution::Sandbox::is_read_only(&self.plan["sandbox"])
            && path.starts_with(workspace)
            && !read_only
        {
            return Err(Error::forbidden(
                "This conversation requires read-only workspace bind mounts.",
            ));
        }
        // Do not let a symlink turn a permitted name into another root. Missing
        // sources are rejected too: Docker must not create a path as namespace root.
        if std::fs::canonicalize(path).ok().as_deref() != Some(path) {
            return Err(Error::forbidden(
                "Docker bind source is missing or redirected.",
            ));
        }
        Ok(())
    }

    fn host_config(&self, host: &Value) -> Result<()> {
        config::fields(
            host,
            &[
                "MaskedPaths",
                "ReadonlyPaths",
                "Privileged",
                "CapAdd",
                "Devices",
                "DeviceRequests",
                "DeviceCgroupRules",
                "VolumesFrom",
                "CgroupParent",
                "UsernsMode",
                "PidMode",
                "UTSMode",
                "ContainerIDFile",
                "StorageOpt",
                "IpcMode",
                "NetworkMode",
                "CgroupnsMode",
                "Runtime",
                "SecurityOpt",
                "RestartPolicy",
                "Binds",
                "Mounts",
            ],
        )?;
        config::fields(&host["RestartPolicy"], &["Name"])?;
        for field in ["MaskedPaths", "ReadonlyPaths"] {
            if host.get(field).is_some_and(|value| !value.is_null()) {
                return Err(Error::forbidden(
                    "Docker must retain its default kernel path masks.",
                ));
            }
        }
        for field in [
            "Privileged",
            "CapAdd",
            "Devices",
            "DeviceRequests",
            "DeviceCgroupRules",
            "VolumesFrom",
            "CgroupParent",
            "UsernsMode",
            "PidMode",
            "UTSMode",
            "ContainerIDFile",
            "StorageOpt",
        ] {
            if present(host, field) {
                return Err(Error::forbidden(format!(
                    "Docker {field} is unavailable in native conversations."
                )));
            }
        }
        if host["IpcMode"]
            .as_str()
            .is_some_and(|mode| !matches!(mode, "" | "private" | "shareable"))
        {
            return Err(Error::forbidden("Docker requires a private IPC namespace."));
        }
        if host["NetworkMode"]
            .as_str()
            .is_some_and(|mode| mode == "host" || mode.starts_with("container:"))
        {
            return Err(Error::forbidden(
                "Docker host/container network namespace overrides are unavailable.",
            ));
        }
        if host["CgroupnsMode"]
            .as_str()
            .is_some_and(|mode| !mode.is_empty() && mode != "private")
            || host["Runtime"]
                .as_str()
                .is_some_and(|runtime| !runtime.is_empty() && runtime != "runc")
        {
            return Err(Error::forbidden(
                "Docker requires its private namespaces and standard runtime.",
            ));
        }
        if let Some(options) = host["SecurityOpt"].as_array() {
            for option in options {
                if !matches!(
                    option.as_str(),
                    Some("no-new-privileges" | "no-new-privileges:true" | "no-new-privileges=true")
                ) {
                    return Err(Error::forbidden(
                        "Docker security overrides are unavailable.",
                    ));
                }
            }
        }
        // A daemon restart must not run containers created under a previous
        // attempt's rights before the current policy has checked their mounts.
        if host["RestartPolicy"]["Name"]
            .as_str()
            .is_some_and(|name| !name.is_empty() && name != "no")
        {
            return Err(Error::forbidden(
                "Docker restart policies are unavailable; recreate services in each turn.",
            ));
        }
        if let Some(binds) = host["Binds"].as_array() {
            for bind in binds {
                let parts: Vec<_> = bind.as_str().unwrap_or("").split(':').collect();
                if parts.len() < 2 || parts.len() > 3 {
                    return Err(Error::forbidden("Invalid Docker bind mount."));
                }
                let options: Vec<_> = parts.get(2).copied().unwrap_or("rw").split(',').collect();
                if options
                    .iter()
                    .any(|option| !matches!(*option, "ro" | "rw" | "rprivate" | "private"))
                {
                    return Err(Error::forbidden(
                        "Docker bind propagation and relabeling are unavailable.",
                    ));
                }
                if parts[0].starts_with('/') {
                    self.bind(parts[0], options.contains(&"ro"))?;
                } else if parts[0].is_empty()
                    || !parts[0]
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                {
                    return Err(Error::forbidden("Invalid Docker volume name."));
                }
            }
        }
        if let Some(mounts) = host["Mounts"].as_array() {
            for mount in mounts {
                config::fields(
                    mount,
                    &["Type", "Source", "ReadOnly", "BindOptions", "VolumeOptions"],
                )?;
                config::fields(&mount["BindOptions"], &["NonRecursive", "Propagation"])?;
                config::fields(&mount["VolumeOptions"], &["DriverConfig"])?;
                match mount["Type"].as_str() {
                    Some("bind") => {
                        if present(&mount["BindOptions"], "NonRecursive")
                            || mount["BindOptions"]["Propagation"]
                                .as_str()
                                .is_some_and(|mode| {
                                    !mode.is_empty() && mode != "rprivate" && mode != "private"
                                })
                        {
                            return Err(Error::forbidden(
                                "Docker must retain recursive conversation mount restrictions.",
                            ));
                        }
                        self.bind(text(mount, "Source"), mount["ReadOnly"] == true)?;
                    }
                    Some("volume") => {
                        self.volume_driver(&mount["VolumeOptions"]["DriverConfig"])?;
                    }
                    Some("tmpfs") => {}
                    _ => return Err(Error::forbidden("Unsupported native Docker mount type.")),
                }
            }
        }
        Ok(())
    }

    fn volume_driver(&self, driver: &Value) -> Result<()> {
        config::fields(driver, &["Name", "Options"])?;
        if driver["Name"]
            .as_str()
            .is_some_and(|name| !name.is_empty() && name != "local")
            || present(driver, "Options")
        {
            return Err(Error::forbidden(
                "Docker supports ordinary local volumes without driver options.",
            ));
        }
        Ok(())
    }

    fn authorize(&self, request: &Value) -> Result<()> {
        let uri = text(request, "RequestUri");
        let mut path = uri.split('?').next().unwrap_or("");
        if let Some(versioned) = path.strip_prefix("/v")
            && versioned.as_bytes().first().is_some_and(u8::is_ascii_digit)
        {
            let (version, rest) = versioned
                .split_once('/')
                .ok_or_else(|| Error::forbidden("Invalid Docker API path."))?;
            if !version
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
            {
                return Err(Error::forbidden("Invalid Docker API version."));
            }
            path = &path[version.len() + 2..];
            let _ = rest;
        }
        // Docker escapes registry/repository separators inside image references.
        // Keep all management routes strict, including encoded container paths.
        let image_path = path.replace("%2F", "/").replace("%2f", "/");
        if path.starts_with("/images/") || path.starts_with("/distribution/") {
            path = &image_path;
        }
        if path.contains('%') || path.contains("..") || path.contains("//") {
            return Err(Error::forbidden("Invalid Docker API path."));
        }
        let segments: Vec<_> = path.trim_start_matches('/').split('/').collect();
        let method = text(request, "RequestMethod");
        let body = request["RequestBody"].as_str().unwrap_or("");
        let decoded = STANDARD.decode(body).map_err(Error::internal)?;
        // Build archives and upgraded sessions remain streams. Every endpoint
        // whose JSON controls authorization uses a duplicate-rejecting decoder;
        // Go merges duplicate map/struct fields where serde would replace them.
        let configuration = match segments.first().copied() {
            Some("containers") => {
                segments.get(1) == Some(&"create")
                    || matches!(segments.get(2), Some(&"update" | &"exec"))
            }
            Some("exec") => segments.get(2) == Some(&"start"),
            Some("volumes" | "networks") => segments.get(1) == Some(&"create"),
            _ => false,
        };
        let json_body = if configuration && !decoded.is_empty() {
            config::decode(&decoded)?
        } else {
            Value::Null
        };
        if configuration && method == "POST" && !json_body.is_object() {
            return Err(Error::forbidden(
                "Docker configuration must be a visible JSON object.",
            ));
        }
        match segments.first().copied() {
            Some("_ping" | "version" | "info" | "events" | "system" | "distribution")
                if method == "GET" || method == "HEAD" => {}
            Some("containers") => {
                if segments.get(1) == Some(&"create") && method == "POST" {
                    config::fields(&json_body, &["HostConfig"])?;
                    self.host_config(&json_body["HostConfig"])?;
                } else if !matches!(segments.get(1), Some(&"json" | &"prune")) {
                    let name = segments.get(1).copied().unwrap_or("");
                    if name.is_empty()
                        || !name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                    {
                        return Err(Error::forbidden("Invalid Docker container name."));
                    }
                    // Names and unambiguous short IDs are both part of Docker's
                    // public API. Resolve only directories in this private store.
                    let mut found = None;
                    for entry in std::fs::read_dir(Path::new(DATA).join("containers"))? {
                        let entry = entry?;
                        let id = entry.file_name();
                        let id = id.to_string_lossy();
                        if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                            continue;
                        }
                        let config: Value = serde_json::from_slice(&std::fs::read(
                            entry.path().join("config.v2.json"),
                        )?)?;
                        if (id.starts_with(name) || config["Name"] == format!("/{name}"))
                            && found.replace(entry.path()).is_some()
                        {
                            return Err(Error::forbidden("Ambiguous Docker container ID."));
                        }
                    }
                    let directory = found
                        .ok_or_else(|| Error::forbidden("Docker container is unavailable."))?;
                    if method != "DELETE" && !(method == "GET" && segments.get(2) == Some(&"json"))
                    {
                        let mut host: Value = serde_json::from_slice(&std::fs::read(
                            directory.join("hostconfig.json"),
                        )?)?;
                        // Docker fills these with its default masks after the
                        // create request. User overrides were refused at create.
                        if let Some(object) = host.as_object_mut() {
                            object.remove("MaskedPaths");
                            object.remove("ReadonlyPaths");
                        }
                        self.host_config(&host)?;
                    }
                    if segments.get(2) == Some(&"update") {
                        self.host_config(&json_body)?;
                    }
                    if segments.get(2) == Some(&"exec") {
                        config::fields(&json_body, &["Privileged"])?;
                        if json_body["Privileged"] == true {
                            return Err(Error::forbidden("Privileged Docker exec is unavailable."));
                        }
                    }
                }
            }
            Some("exec") => {
                config::fields(&json_body, &["Privileged"])?;
                if json_body["Privileged"] == true {
                    return Err(Error::forbidden("Privileged Docker exec is unavailable."));
                }
            }
            Some("volumes") => {
                if method == "POST" && segments.get(1) == Some(&"create") {
                    config::fields(&json_body, &["Driver", "DriverOpts"])?;
                    self.volume_driver(&json!({
                        "Name": json_body["Driver"],
                        "Options": json_body["DriverOpts"],
                    }))?;
                }
            }
            Some("networks") => {
                config::fields(&json_body, &["Driver"])?;
                if method == "POST"
                    && segments.get(1) == Some(&"create")
                    && json_body["Driver"]
                        .as_str()
                        .is_some_and(|driver| !driver.is_empty() && driver != "bridge")
                {
                    return Err(Error::forbidden(
                        "Native Docker supports private bridge networks.",
                    ));
                }
            }
            Some("images" | "build" | "grpc" | "session" | "auth" | "commit") => {}
            _ => {
                return Err(Error::forbidden(
                    "This Docker API operation is unavailable in native conversations.",
                ));
            }
        }
        Ok(())
    }
}

async fn authorization(State(policy): State<Policy>, body: axum::body::Bytes) -> Json<Value> {
    // Docker's private plugin RPC does not promise an application/json header.
    // Decode its bounded JSON body directly; malformed requests still fail shut.
    let result = serde_json::from_slice::<Value>(&body)
        .map_err(|_| Error::bad("Invalid Docker authorization request."))
        .and_then(|request| policy.authorize(&request));
    match result {
        Ok(()) => Json(json!({ "Allow": true })),
        Err(error) => Json(json!({
            "Allow": false,
            "Msg": error.message,
        })),
    }
}

async fn relay(listener: tokio::net::TcpListener, stop: CancellationToken) {
    let _ = crate::microvm::listener::serve(
        || listener.accept(),
        |(mut client, _)| async move {
            if let Ok(mut server) = tokio::net::UnixStream::connect("/run/cairn-mcp.sock").await {
                let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
            }
        },
        32,
        stop,
    )
    .await;
}

async fn clear_store(daemon: &mut Option<tokio::process::Child>) -> Result<()> {
    if let Some(child) = daemon.as_mut() {
        if let Some(pid) = child.id()
            && unsafe { libc::kill(pid as i32, libc::SIGTERM) } != 0
        {
            return Err(Error::unavailable(
                "Docker shutdown could not be requested; state retained.",
            ));
        }
        let status = tokio::time::timeout(Duration::from_secs(20), child.wait())
            .await
            .map_err(|_| Error::timeout("Docker shutdown is unconfirmed; state retained."))??;
        if !status.success() {
            return Err(Error::unavailable(
                "Docker exited unsuccessfully; resume in a new turn before resetting state.",
            ));
        }
    }
    *daemon = None;
    let mut entries = tokio::fs::read_dir(DATA).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            tokio::fs::remove_dir_all(entry.path()).await?;
        } else {
            tokio::fs::remove_file(entry.path()).await?;
        }
    }
    tokio::fs::File::open(DATA).await?.sync_all().await?;
    Ok(())
}

fn unprivileged(command: &mut Command) {
    // This is namespace root, with no capability over the initial host user
    // namespace. Keep inherited render/video groups for native GPU access.
    unsafe {
        command.pre_exec(|| {
            for capability in 0..=63 {
                if libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL)
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if libc::setgid(1000) != 0 || libc::setuid(1000) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Runs only inside RootlessKit. All children, including slirp, dockerd,
/// containerd, builds and containers remain in the original workload cgroup.
pub async fn enclave(stop: CancellationToken) -> Result<i32> {
    if unsafe { libc::geteuid() } != 0
        || std::env::var_os("ROOTLESSKIT_STATE_DIR").is_none()
        || std::fs::read_to_string("/proc/self/uid_map")?
            .lines()
            .count()
            != 2
    {
        return Err(Error::forbidden(
            "Native enclave requires its managed rootless namespace.",
        ));
    }
    let plan: Value = serde_json::from_slice(&tokio::fs::read("/run/cairn-plan.json").await?)?;
    let identity: super::identity::Identity =
        serde_json::from_value(plan["nativeIdentity"].clone())?;
    identity.verify_maps(
        &tokio::fs::read_to_string("/proc/self/uid_map").await?,
        &tokio::fs::read_to_string("/proc/self/gid_map").await?,
    )?;
    hardening::apply()?;
    let policy = Policy {
        plan: Arc::new(plan),
    };
    // RootlessKit copied /etc into a namespace-local tmpfs. Replace its
    // read-only backing symlinks, never the host account databases.
    for (name, contents) in [
        (
            "passwd",
            "root:x:0:0:namespace supervisor:/run/cairn-docker:/usr/sbin/nologin\nnode:x:1000:1000:conversation:/home/node:/bin/sh\n",
        ),
        ("group", "root:x:0:\nnode:x:1000:\n"),
    ] {
        let path = Path::new("/etc").join(name);
        tokio::fs::remove_file(&path).await?;
        tokio::fs::write(&path, contents).await?;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).await?;
    }
    // The tmpfs and network driver's files must be readable but immutable to
    // UID1000. Never chmod the original host binds reached through symlinks.
    tokio::fs::set_permissions("/etc", std::fs::Permissions::from_mode(0o755)).await?;
    let mut entries = tokio::fs::read_dir("/etc").await?;
    while let Some(entry) = entries.next_entry().await? {
        let metadata = tokio::fs::symlink_metadata(entry.path()).await?;
        if metadata.is_file() && metadata.uid() == 0 {
            tokio::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o644))
                .await?;
        }
    }
    let runtime = Path::new(RUNTIME);
    tokio::fs::set_permissions(runtime, std::fs::Permissions::from_mode(0o755)).await?;
    crate::skills::private_dir(std::path::Path::new("/run/docker/plugins")).await?;
    // Docker discovers Unix sockets here in both rootful and rootless modes;
    // .spec files in this directory are not part of its discovery contract.
    let plugin = tokio::net::UnixListener::bind("/run/docker/plugins/cairn.sock")?;
    let router = Router::new()
        .route(
            "/Plugin.Activate",
            post(|| async { Json(json!({ "Implements": ["authz"] })) }),
        )
        .route("/AuthZPlugin.AuthZReq", post(authorization))
        .route(
            "/AuthZPlugin.AuthZRes",
            post(|| async { Json(json!({ "Allow": true })) }),
        )
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(policy.clone());
    let plugin_stop = stop.clone();
    tokio::spawn(async move {
        axum::serve(plugin, router)
            .with_graceful_shutdown(plugin_stop.cancelled_owned())
            .await
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:5202").await?;
    tokio::spawn(relay(listener, stop.clone()));
    let starter = tokio::net::UnixListener::bind(runtime.join("start.sock"))?;
    std::os::unix::fs::chown(runtime.join("start.sock"), None, Some(1000))?;
    tokio::fs::set_permissions(
        runtime.join("start.sock"),
        std::fs::Permissions::from_mode(0o660),
    )
    .await?;
    let daemon = Arc::new(Mutex::new(None::<tokio::process::Child>));
    let daemon_task = daemon.clone();
    let start_stop = stop.clone();
    let launch_policy = policy.clone();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                () = start_stop.cancelled() => break,
                result = starter.accept() => result,
            };
            let Ok((mut client, _)) = accepted else {
                break;
            };
            let mut operation = [0u8; 5];
            match tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut operation))
                .await
            {
                Ok(Ok(_)) => {}
                _ => continue,
            };
            let mut daemon = daemon_task.lock().await;
            if &operation == b"reset" {
                let reply = match clear_store(&mut daemon).await {
                    Ok(()) => "OK".to_owned(),
                    Err(error) => error.message,
                };
                let _ = client.write_all(reply.as_bytes()).await;
                continue;
            }
            if &operation != b"start" {
                continue;
            }
            let ready = match daemon.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => false,
            };
            if !ready {
                if let Err(error) = mounts::restore(&launch_policy).await {
                    let _ = client.write_all(error.message.as_bytes()).await;
                    continue;
                }
                // The daemon's home is private namespace-root state, not the
                // engine's configuration, and plugins cannot be user supplied.
                let mut command = Command::new("/opt/cairn-docker/bin/dockerd");
                command
                    .args([
                        "--rootless",
                        "--host=unix:///run/cairn-docker/private/docker.sock",
                        "--group=0",
                        "--data-root=/var/lib/cairn-docker",
                        "--exec-root=/run/cairn-docker/exec",
                        "--pidfile=/run/cairn-docker/dockerd.pid",
                        "--config-file=/run/cairn-docker/daemon.json",
                        "--authorization-plugin=cairn",
                        "--storage-driver=overlay2",
                        "--shutdown-timeout=5",
                    ])
                    .env("HOME", "/run/cairn-docker")
                    .env("XDG_RUNTIME_DIR", RUNTIME)
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true);
                *daemon = command.spawn().ok();
                if daemon.is_some() {
                    let _ = tokio::fs::write(Path::new(RUNTIME).join("daemon.started"), b"1").await;
                    let _ = tokio::fs::set_permissions(
                        Path::new(RUNTIME).join("daemon.started"),
                        std::fs::Permissions::from_mode(0o444),
                    )
                    .await;
                }
            }
            let _ = client
                .write_all(if daemon.is_some() {
                    b"OK"
                } else {
                    b"Docker daemon launch failed"
                })
                .await;
        }
    });
    // Daemon configuration remains namespace-root private.
    tokio::fs::write(
        runtime.join("daemon.json"),
        b"{\"features\":{\"containerd-snapshotter\":false}}",
    )
    .await?;
    crate::skills::private_dir(&runtime.join("private")).await?;
    proxy::serve(policy, stop.clone()).await?;
    let mut engine = Command::new("/run/cairn-entrypoint/cairn");
    engine
        .arg("runner-entry")
        .env("HOME", "/home/node")
        .env("DOCKER_HOST", format!("unix://{SOCKET}"))
        .env(
            "PATH",
            "/opt/cairn-docker:/opt/cairn-native-tools/bin:/run/cairn-entrypoint:/home/node/.local/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .kill_on_drop(true);
    unprivileged(&mut engine);
    let mut engine = engine.spawn()?;
    let status = tokio::select! {
        result = engine.wait() => result?.code().unwrap_or(143),
        () = stop.cancelled() => 143,
    };
    // systemd confirms all descendant termination before checkpoint/unmount.
    // Best-effort graceful daemon shutdown improves crash consistency, but is
    // never a substitute for cgroup fencing on cancellation or supervisor death.
    if let Some(daemon) = daemon.lock().await.as_mut()
        && let Some(pid) = daemon.id()
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), daemon.wait()).await;
    }
    Ok(status)
}

/// The immutable docker wrapper invokes this before commands needing a server.
async fn request(operation: &[u8]) -> Result<()> {
    let mut client = tokio::net::UnixStream::connect(Path::new(RUNTIME).join("start.sock")).await?;
    client.write_all(operation).await?;
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(120), client.read_to_end(&mut reply))
        .await
        .map_err(|_| Error::timeout("Native Docker startup request timed out."))??;
    if reply != b"OK" {
        return Err(Error::unavailable(
            String::from_utf8_lossy(&reply).into_owned(),
        ));
    }
    Ok(())
}

pub async fn reset() -> Result<()> {
    request(b"reset").await
}

pub async fn ensure() -> Result<()> {
    request(b"start").await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let result = Command::new(DOCKER)
            .args(["--host", &format!("unix://{SOCKET}"), "info"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status()
            .await?;
        if result.success() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::timeout(
                "This conversation's Docker daemon did not become ready; inspect the run's daemon diagnostics.",
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Set before Tokio starts: the only setuid helpers run in RootlessKit's outer
/// bootstrap. This process, the engine and dockerd can never gain host privileges.
pub fn harden_enclave() -> std::io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            plan: Arc::new(json!({
                "sandbox": "workspace-write",
                "projectRoot": "/workspace",
                "guestDataRoot": "/data",
                "runId": "run",
            })),
        }
    }

    #[test]
    fn unversioned_version_and_volume_routes_are_not_misread_as_api_versions() {
        for (path, method) in [("/version", "GET"), ("/volumes/create", "POST")] {
            assert!(
                policy()
                    .authorize(&json!({
                        "RequestUri": path,
                        "RequestMethod": method,
                    "RequestBody": STANDARD.encode(b"{}"),
                    }))
                    .is_ok()
            );
        }
    }

    #[tokio::test]
    async fn plugin_rpc_without_json_media_type_still_enforces_authorization() {
        use tower::ServiceExt;
        for (path, method, body, expected) in [
            ("/v1.56/info", "GET", Value::Null, true),
            (
                "/v1.56/containers/create",
                "POST",
                json!({ "HostConfig": { "Privileged": true } }),
                false,
            ),
        ] {
            let router = Router::new()
                .route("/AuthZPlugin.AuthZReq", post(authorization))
                .with_state(policy());
            let payload = json!({
                "RequestUri": path,
                "RequestMethod": method,
                "RequestBody": STANDARD.encode(serde_json::to_vec(&body).unwrap()),
            });
            let request = axum::http::Request::post("/AuthZPlugin.AuthZReq")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&payload).unwrap(),
                ))
                .unwrap();
            let response = router.oneshot(request).await.unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let response: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(response["Allow"], expected);
        }
    }

    #[test]
    fn docker_api_cannot_acquire_namespace_mount_device_or_restart_privileges() {
        for host in [
            json!({ "MaskedPaths": [] }),
            json!({ "ReadonlyPaths": [] }),
            json!({"Privileged":true}),
            json!({"CapAdd":["SYS_ADMIN"]}),
            json!({"PidMode":"host"}),
            json!({"VolumesFrom":["other"]}),
            json!({"Devices":[{}]}),
            json!({"SecurityOpt":["seccomp=unconfined"]}),
            json!({"RestartPolicy":{"Name":"always"}}),
            json!({"CgroupParent":"/"}),
            json!({"Runtime":"other"}),
        ] {
            assert!(policy().host_config(&host).is_err(), "{host}");
        }
        assert!(
            policy()
                .host_config(&json!({
                    "Binds": ["database:/var/lib/postgresql/data"],
                    "RestartPolicy": { "Name": "no" },
                }))
                .is_ok()
        );
    }

    #[test]
    fn raw_json_cannot_hide_controls_from_the_authorization_policy() {
        for (path, body) in [
            ("/volumes/create", ""),
            (
                "/volumes/create",
                r#"{"Driver":"local","driverOpts":{"device":"/run/cairn-docker","o":"bind","type":"none"}}"#,
            ),
            (
                "/volumes/create",
                r#"{"DriverOptſ":{"device":"/run/cairn-docker","o":"bind","type":"none"}}"#,
            ),
            (
                "/volumes/create",
                r#"{"DriverOpts":{"device":"/run/cairn-docker","o":"bind","type":"none"},"DriverOpts":{}}"#,
            ),
            (
                "/containers/create",
                r#"{"hostConfig":{"Binds":["/:/host"]}}"#,
            ),
            (
                "/containers/create",
                r#"{"HostConfig":{"privileged":true}}"#,
            ),
            (
                "/containers/create",
                r#"{"HostConfig":{"Privileged":true},"HostConfig":{}}"#,
            ),
            (
                "/containers/create",
                r#"{"HostConfig":{"RestartPolicy":{"name":"always"}}}"#,
            ),
            (
                "/containers/create",
                r#"{"HostConfig":{"Mounts":[{"Type":"volume","VolumeOptions":{"driverConfig":{"Options":{"device":"/"}}}}]}}"#,
            ),
            (
                "/containers/create",
                r#"{"HostConfig":{"Mounts":[{"Type":"volume","VolumeOptions":{"DriverConfig":{"options":{"device":"/"}}}}]}}"#,
            ),
            (
                "/containers/create",
                r#"{"HostConfig":{"Mounts":[{"Type":"bind","Source":"/tmp","BindOptions":{"nonRecursive":true}}]}}"#,
            ),
            ("/exec/id/start", r#"{"privileged":true}"#),
            (
                "/exec/id/start",
                r#"{"Privileged":true,"Privileged":false}"#,
            ),
            ("/networks/create", r#"{"driver":"host"}"#),
        ] {
            assert!(
                policy()
                    .authorize(&json!({
                        "RequestUri": path,
                        "RequestMethod": "POST",
                        "RequestBody": STANDARD.encode(body),
                    }))
                    .is_err(),
                "{path}: {body}"
            );
        }
        // These maps are user data, not Go struct field names.
        let ordinary = br#"{"Name":"development","Driver":"local","Labels":{"driverOpts":"label","DriverOpt\u017f":"label"}}"#;
        assert!(
            policy()
                .authorize(&json!({
                    "RequestUri": "/volumes/create",
                    "RequestMethod": "POST",
                    "RequestBody": STANDARD.encode(ordinary),
                }))
                .is_ok()
        );
    }

    #[test]
    fn docker_driver_options_and_unrecognized_management_apis_fail_closed() {
        assert!(
            policy()
                .volume_driver(&json!({
                    "Name": "local",
                    "Options": {
                        "device": "/host",
                        "type": "none",
                        "o": "bind",
                    },
                }))
                .is_err()
        );
        for path in [
            "/v1.55/plugins/create",
            "/swarm/init",
            "/v1.55/secrets/create",
            "/v1.55/containers/%2fhost/start",
        ] {
            assert!(
                policy()
                    .authorize(&json!({
                        "RequestUri": path,
                        "RequestMethod": "POST",
                    }))
                    .is_err()
            );
        }
        assert!(
            policy()
                .authorize(&json!({
                    "RequestUri": "/v1.55/images/ghcr.io%2Fexample%2Fapp:dev/json",
                    "RequestMethod": "GET",
                }))
                .is_ok()
        );
        assert!(
            policy()
                .authorize(&json!({
                    "RequestUri": "/v1.55/info",
                    "RequestMethod": "GET",
                }))
                .is_ok()
        );
    }
}
