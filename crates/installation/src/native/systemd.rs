//! Linux process ownership lives in systemd, including when the node daemon crashes.
use crate::{
    error::{Error, Result},
    validation::text,
};
use serde_json::Value;
use std::{
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::process::{Child, Command};

pub struct Identity {
    pub user: String,
    pub uid: u32,
    pub gid: u32,
}

fn command(binary: &str) -> Command {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C.UTF-8")
        .kill_on_drop(true);
    command
}

async fn output(binary: &str, args: &[&str]) -> Result<std::process::Output> {
    tokio::time::timeout(Duration::from_secs(25), command(binary).args(args).output())
        .await
        .map_err(|_| Error::timeout("Native supervisor operation timed out."))?
        .map_err(Error::from)
}

pub fn unit(attempt: &str) -> Result<String> {
    crate::validation::uuid(attempt)?;
    Ok(format!("cairn-native-{attempt}.service"))
}

async fn require_controllers(root: &Path) -> Result<()> {
    let controllers = tokio::fs::read_to_string(root.join("cgroup.controllers"))
        .await
        .map_err(|_| {
            Error::unavailable(
                "Native nodes require cgroup v2 with CPU, memory and PID controllers.",
            )
        })?;
    if !["cpu", "memory", "pids"].iter().all(|required| {
        controllers
            .split_whitespace()
            .any(|controller| controller == *required)
    }) {
        return Err(Error::unavailable(
            "Native nodes require cgroup v2 with CPU, memory and PID controllers.",
        ));
    }
    Ok(())
}

pub async fn identity() -> Result<Identity> {
    if unsafe { libc::geteuid() } != 0 || !Path::new("/run/systemd/system").is_dir() {
        return Err(Error::unavailable(
            "Native nodes require a root-owned systemd supervisor and a separate unprivileged workload user.",
        ));
    }
    require_controllers(Path::new("/sys/fs/cgroup")).await?;
    let user = std::env::var("CAIRN_NATIVE_USER").unwrap_or_else(|_| "cairn-workload".into());
    if !user
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || user.is_empty()
    {
        return Err(Error::bad("Invalid native workload user."));
    }
    let uid = output("/usr/bin/id", &["-u", &user]).await?;
    let gid = output("/usr/bin/id", &["-g", &user]).await?;
    let parse = |value: std::process::Output| -> Result<u32> {
        if !value.status.success() {
            return Err(Error::bad("Native workload user is missing."));
        }
        std::str::from_utf8(&value.stdout)
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .filter(|value| *value != 0)
            .ok_or_else(|| Error::bad("Native workload identity must be unprivileged."))
    };
    let supervisor = supervisor_unit()?;
    if !output("/usr/bin/systemctl", &["is-active", "--quiet", &supervisor])
        .await?
        .status
        .success()
    {
        return Err(Error::unavailable(
            "The native node must run in its installed systemd service.",
        ));
    }
    Ok(Identity {
        user,
        uid: parse(uid)?,
        gid: parse(gid)?,
    })
}

fn supervisor_unit() -> Result<String> {
    let unit = std::env::var("CAIRN_NATIVE_SUPERVISOR_UNIT")
        .unwrap_or_else(|_| "cairn-native-node.service".into());
    if !unit.ends_with(".service")
        || unit.len() > 100
        || !unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(Error::bad("Invalid native supervisor unit."));
    }
    Ok(unit)
}

fn workload_slice() -> Result<String> {
    Ok(format!(
        "{}-workloads.slice",
        supervisor_unit()?.trim_end_matches(".service")
    ))
}

/// All attempts share one host ceiling. Per-attempt limits alone multiply the
/// owner's CPU/RAM budget when several conversations execute concurrently.
pub(super) async fn apply_budget(budget: &crate::microvm::budget::Budget) -> Result<()> {
    let slice = workload_slice()?;
    let started = output("/usr/bin/systemctl", &["start", &slice]).await?;
    if !started.status.success() {
        return Err(Error::unavailable(
            "Could not start the shared native workload slice.",
        ));
    }
    let usage = output(
        "/usr/bin/systemctl",
        &["show", &slice, "--property=MemoryCurrent", "--value"],
    )
    .await?;
    let current = std::str::from_utf8(&usage.stdout)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let current = current
        .filter(|value| *value != u64::MAX)
        .ok_or_else(|| Error::unavailable("Native shared memory usage is unavailable."))?;
    let memory = budget.limits.memory_mi_b.saturating_mul(1_048_576);
    if !usage.status.success() || current > memory {
        return Err(Error::conflict(
            "Shared native RAM budget is below current usage.",
        ));
    }
    let properties = budget_properties(budget);
    let mut args = vec!["set-property", "--runtime", &slice];
    args.extend(properties.iter().map(String::as_str));
    if !output("/usr/bin/systemctl", &args).await?.status.success() {
        return Err(Error::unavailable(
            "Could not apply the shared native CPU/RAM budget.",
        ));
    }
    Ok(())
}

fn budget_properties(budget: &crate::microvm::budget::Budget) -> Vec<String> {
    let memory = budget.limits.memory_mi_b.saturating_mul(1_048_576);
    vec![
        format!("CPUQuota={}%", u64::from(budget.limits.cpu) * 100),
        format!("MemoryMax={memory}"),
        format!("MemoryHigh={}", memory - memory / 10),
        "MemorySwapMax=0".into(),
        "CPUWeight=10".into(),
        "IOWeight=10".into(),
    ]
}

/// Begin fencing before aborting a dead backend's I/O. Completion is confirmed
/// separately so a task blocked in that I/O can exit after the device is stopped.
pub(super) async fn request_stop(attempt: &str) -> Result<()> {
    let unit = unit(attempt)?;
    let _ = output("/usr/bin/systemctl", &["thaw", &unit]).await?;
    let _ = output("/usr/bin/systemctl", &["--no-block", "stop", &unit]).await?;
    Ok(())
}

pub async fn stop_workload(attempt: &str) -> Result<()> {
    let unit = unit(attempt)?;
    // systemd refuses stop jobs for frozen units. Protection/lease fencing can
    // leave an owned workload frozen; restore signal delivery before stopping.
    let _ = output("/usr/bin/systemctl", &["thaw", &unit]).await?;
    let stopped = output("/usr/bin/systemctl", &["stop", &unit]).await?;
    // A collected or never-started transient unit has no processes to fence.
    let state = output(
        "/usr/bin/systemctl",
        &["show", &unit, "--property=ActiveState", "--value"],
    )
    .await?;
    let state = String::from_utf8_lossy(&state.stdout);
    let group = output(
        "/usr/bin/systemctl",
        &["show", &unit, "--property=ControlGroup", "--value"],
    )
    .await?;
    let group = String::from_utf8_lossy(&group.stdout);
    let populated = if group.trim().is_empty() {
        false
    } else {
        let path = Path::new("/sys/fs/cgroup")
            .join(group.trim().trim_start_matches('/'))
            .join("cgroup.events");
        match tokio::fs::read_to_string(path).await {
            Ok(events) => events.lines().any(|line| line == "populated 1"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        }
    };
    if !populated
        && (matches!(state.trim(), "inactive" | "failed")
            || state.trim().is_empty() && !state_is_present(attempt).await?)
    {
        return Ok(());
    }
    let _ = stopped;
    Err(Error::unavailable(
        "Native workload termination is not confirmed.",
    ))
}

fn guardian_unit(attempt: &str) -> Result<String> {
    crate::validation::uuid(attempt)?;
    Ok(format!("cairn-native-guardian-{attempt}.service"))
}

pub async fn stop(attempt: &str) -> Result<()> {
    stop_workload(attempt).await?;
    let _ = output("/usr/bin/systemctl", &["stop", &guardian_unit(attempt)?]).await?;
    Ok(())
}

pub async fn start_guardian(attempt: &str, lease: &Path) -> Result<()> {
    let parent = supervisor_unit()?;
    let binary = std::env::current_exe()?;
    let args = vec![
        "--quiet".into(),
        "--collect".into(),
        "--service-type=exec".into(),
        format!("--unit={}", guardian_unit(attempt)?),
        format!("--property=BindsTo={parent}"),
        format!("--property=After={parent}"),
        "--property=WatchdogSec=5s".into(),
        "--property=NotifyAccess=main".into(),
        binary.to_string_lossy().into_owned(),
        "native-watchdog".into(),
        path_property(lease)?,
        attempt.into(),
    ];
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        command("/usr/bin/systemd-run").args(args).output(),
    )
    .await
    .map_err(|_| Error::timeout("Native lease guardian startup timed out."))??;
    if !result.status.success() {
        return Err(Error::unavailable(
            "Could not start the native lease guardian.",
        ));
    }
    Ok(())
}

async fn state_is_present(attempt: &str) -> Result<bool> {
    let unit = unit(attempt)?;
    let output = output(
        "/usr/bin/systemctl",
        &["show", &unit, "--property=LoadState", "--value"],
    )
    .await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim() != "not-found")
}

fn path_property(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| Error::bad("Native paths must be UTF-8."))?;
    if !path.is_absolute()
        || text
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b"%:\\\n\r\"".contains(&b))
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(Error::bad(
            "Native paths must be absolute and free of systemd specifiers or separators.",
        ));
    }
    Ok(text.into())
}

pub struct Launch<'a> {
    pub id: &'a str,
    pub plan: &'a Value,
    pub data: &'a Path,
    pub root: &'a Path,
    pub home: &'a Path,
    pub workspace: &'a Path,
    pub output: &'a Path,
    pub identity: &'a Identity,
    pub temporary: &'a Path,
    pub auth: Option<&'a Path>,
}

pub fn arguments(launch: &Launch<'_>) -> Result<Vec<String>> {
    let resources = &launch.plan["resources"];
    let cpu = resources["cpu"]
        .as_u64()
        .filter(|v| (1..=4096).contains(v))
        .ok_or_else(|| Error::bad("Invalid native CPU budget."))?;
    let memory = resources["memoryMiB"]
        .as_u64()
        .filter(|v| *v >= 128 && *v <= 1_073_741_824)
        .ok_or_else(|| Error::bad("Invalid native memory budget."))?;
    let mut args = vec![
        "--quiet".into(),
        "--wait".into(),
        "--pipe".into(),
        "--collect".into(),
        "--service-type=exec".into(),
        format!("--unit={}", unit(launch.id)?),
        format!("--uid={}", launch.identity.user),
    ];
    let administrator = launch.plan["nativeMode"]["mode"] == "administrator";
    let mut property = |value: String| args.push(format!("--property={value}"));
    for value in [
        "KillMode=control-group",
        "TimeoutStopSec=10s",
        "SendSIGKILL=yes",
        "NoNewPrivileges=yes",
        "CapabilityBoundingSet=",
        "RestrictSUIDSGID=yes",
        "ProtectKernelTunables=yes",
        "ProtectKernelModules=yes",
        "ProtectControlGroups=yes",
        "ProtectClock=yes",
        "ProtectProc=invisible",
        "ProtectSystem=strict",
        "MountAPIVFS=yes",
        "DevicePolicy=closed",
        "UMask=0077",
        "TasksMax=4096",
        "MemorySwapMax=0",
        "OOMScoreAdjust=1000",
        "OOMPolicy=kill",
        "CPUWeight=10",
        "IOWeight=10",
        "Nice=10",
    ] {
        let restriction = value.starts_with("Protect")
            || value.starts_with("Restrict")
            || value.starts_with("NoNewPrivileges=")
            || value.starts_with("CapabilityBoundingSet=")
            || value.starts_with("DevicePolicy=")
            || value == "MountAPIVFS=yes";
        if !administrator || !restriction {
            property(value.into());
        }
    }
    property(format!(
        "BindsTo={} {}",
        supervisor_unit()?,
        guardian_unit(launch.id)?
    ));
    property(format!(
        "After={} {}",
        supervisor_unit()?,
        guardian_unit(launch.id)?
    ));
    property(format!("CPUQuota={}%", cpu * 100));
    property(format!("MemoryMax={}M", memory));
    property(format!("Slice={}", workload_slice()?));
    if !administrator {
        property(format!("RootDirectory={}", path_property(launch.root)?));
    } else {
        // The entrypoint exists only after BindReadOnlyPaths is applied. Tell
        // systemd-run to leave executable resolution to the service manager,
        // as it already does for protected units with RootDirectory.
        property("ExecSearchPath=/run/cairn-entrypoint".into());
        // Keep the same logical workspace/session paths even when their host
        // ancestors are owner-private. Administrator already grants full host
        // access; read/search capability avoids changing those directories or
        // replacing them with a namespace scaffold. Writes still use sudo.
        property("AmbientCapabilities=CAP_DAC_READ_SEARCH".into());
    }
    if administrator {
        // Ambient capabilities become effective at execve, after systemd's
        // own chdir. runner-entry then uses the unchanged plan.cwd when it
        // starts the engine, with read/search capability already effective.
        property("WorkingDirectory=/".into());
    } else {
        property(format!(
            "WorkingDirectory={}",
            path_property(Path::new(text(launch.plan, "cwd")))?
        ));
    }
    for source in [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc/ssl",
        "/etc/alternatives",
        "/etc/fonts",
        "/etc/os-release",
        "/etc/ld.so.cache",
        "/etc/nsswitch.conf",
        "/etc/passwd",
        "/etc/group",
        "/etc/hosts",
        "/etc/resolv.conf",
        "/etc/vulkan",
        "/etc/OpenCL",
        "/etc/nvidia",
    ] {
        if !administrator && Path::new(source).exists() {
            property(format!("BindReadOnlyPaths={source}"));
        }
    }
    let target = PathBuf::from(text(launch.plan, "projectRoot"));
    for (source, destination, read_only) in [
        (launch.home, Path::new("/home/node"), false),
        (
            launch.workspace,
            target.as_path(),
            crate::execution::Sandbox::is_read_only(&launch.plan["sandbox"]),
        ),
        (
            launch.output,
            PathBuf::from(
                launch.plan["guestDataRoot"]
                    .as_str()
                    .unwrap_or_else(|| launch.data.to_str().unwrap_or("/data")),
            )
            .join("runs")
            .join(text(launch.plan, "runId"))
            .join("output")
            .as_path(),
            false,
        ),
    ] {
        let key = if read_only {
            "BindReadOnlyPaths"
        } else {
            "BindPaths"
        };
        property(format!(
            "{key}={}:{}",
            path_property(source)?,
            path_property(destination)?
        ));
        if !read_only {
            property(format!("ReadWritePaths=+{}", path_property(destination)?));
        }
    }
    let original = crate::microvm::plan::Plan::new(launch.plan.clone());
    let empty = launch
        .data
        .join("runs")
        .join(text(launch.plan, "runId"))
        .join("empty");
    for import in original.imports().filter(|import| {
        import.source == empty && import.read_only && Path::new(import.target).starts_with(&target)
    }) {
        property(format!(
            "BindReadOnlyPaths={}:{}",
            path_property(import.source)?,
            path_property(Path::new(import.target))?
        ));
    }
    property(format!(
        "BindPaths={}:/tmp",
        path_property(launch.temporary)?
    ));
    property("ReadWritePaths=+/tmp".into());
    let bindings: Vec<super::home::Binding> = serde_json::from_value(
        launch
            .plan
            .get("nativeHomeBindings")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )?;
    for binding in bindings {
        let key = if binding.read_only {
            "BindReadOnlyPaths"
        } else {
            "BindPaths"
        };
        property(format!(
            "{key}={}:{}",
            path_property(&binding.source)?,
            path_property(&binding.target)?
        ));
        if !binding.read_only {
            property(format!(
                "ReadWritePaths=+{}",
                path_property(&binding.target)?
            ));
        }
    }
    let tools = tools(text(launch.plan, "runtimeId"))?;
    property(format!(
        "BindReadOnlyPaths={}:/opt/cairn-native-tools",
        path_property(&tools)?
    ));
    property(format!(
        "BindReadOnlyPaths={}:/run/cairn-plan.json",
        path_property(&launch.root.with_extension("plan.json"))?
    ));
    property(format!(
        "BindReadOnlyPaths={}:/run/cairn-entrypoint/cairn",
        path_property(&std::env::current_exe()?)?
    ));
    if let Some(auth) = launch.auth {
        property(format!(
            "BindReadOnlyPaths={}:/run/cairn-auth.sock",
            path_property(auth)?
        ));
    }
    let inbox = launch
        .data
        .join("runs")
        .join(text(launch.plan, "runId"))
        .join("chat-input");
    property(format!(
        "BindReadOnlyPaths={}:/run/cairn-chat",
        path_property(&inbox)?
    ));
    // Expose only GPU device nodes, never input devices, block devices or the Docker socket.
    let mut devices = Vec::new();
    if Path::new("/dev/kfd").exists() {
        devices.push(PathBuf::from("/dev/kfd"));
    }
    if Path::new("/dev/dri").exists() {
        devices.push(PathBuf::from("/dev/dri"));
    }
    if let Ok(entries) = std::fs::read_dir("/dev") {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("nvidia") {
                devices.push(entry.path());
            }
        }
    }
    for device in devices.into_iter().filter(|_| !administrator) {
        property(format!("BindPaths={}", path_property(&device)?));
        if device.is_dir() {
            for entry in std::fs::read_dir(&device)?.flatten() {
                if entry.file_type()?.is_char_device() {
                    property(format!("DeviceAllow={} rw", path_property(&entry.path())?));
                }
            }
        } else if std::fs::symlink_metadata(&device)?
            .file_type()
            .is_char_device()
        {
            property(format!("DeviceAllow={} rw", path_property(&device)?));
        }
    }
    args.extend([
        "--setenv=HOME=/home/node",
        "--setenv=CAIRN_AUTH_SOCKET=/run/cairn-auth.sock",
        "--setenv=PATH=/opt/cairn-native-tools/bin:/run/cairn-entrypoint:/home/node/.local/bin:/usr/local/bin:/usr/bin:/bin",
        "--",
        "/run/cairn-entrypoint/cairn",
        "runner-entry",
    ].map(String::from));
    Ok(args)
}

pub fn spawn(launch: &Launch<'_>) -> Result<Child> {
    command("/usr/bin/systemd-run")
        .args(arguments(launch)?)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(Error::from)
}

pub fn runtimes() -> Result<Vec<String>> {
    Ok(installed_tools()?
        .into_iter()
        .map(|(runtime, _)| runtime)
        .collect())
}

pub fn tools(runtime: &str) -> Result<PathBuf> {
    installed_tools()?
        .into_iter()
        .find(|(candidate, _)| candidate == runtime)
        .map(|(_, path)| path)
        .ok_or_else(|| {
            Error::unavailable(
                "The pinned coding-engine runtime is not installed on this native node.",
            )
        })
}

fn installed_tools() -> Result<Vec<(String, PathBuf)>> {
    let current = PathBuf::from(
        std::env::var("CAIRN_NATIVE_TOOLS")
            .map_err(|_| Error::bad("Missing native tool bundle."))?,
    );
    let release_path = current
        .parent()
        .ok_or_else(|| Error::bad("Invalid native tool bundle path."))?
        .join("release.json");
    let release: Value = serde_json::from_slice(&std::fs::read(release_path)?)?;
    let runtime = crate::nodes::component_runtime();
    if release["runtimeId"] != runtime
        || release["os"] != "linux"
        || release["arch"] != "x86_64"
        || !current.join("manifest.json").is_file()
    {
        return Err(Error::conflict(
            "Native tool version mismatch; reinstall the current component bundle.",
        ));
    }
    Ok(vec![(runtime, current)])
}

/// Freeze the entire workload, including descendants. An acknowledged command
/// alone is insufficient: report paused only once systemd confirms the state.
pub async fn set_paused(attempt: &str, paused: bool) -> Result<()> {
    let unit = unit(attempt)?;
    let action = if paused { "freeze" } else { "thaw" };
    let result = output("/usr/bin/systemctl", &[action, &unit]).await?;
    if !result.status.success() {
        return Err(Error::unavailable(
            "Native storage CPU transition was refused.",
        ));
    }
    let state = output(
        "/usr/bin/systemctl",
        &["show", &unit, "--property=FreezerState", "--value"],
    )
    .await?;
    let expected = if paused { "frozen" } else { "running" };
    if !state.status.success() || String::from_utf8_lossy(&state.stdout).trim() != expected {
        return Err(Error::unavailable(
            "Native storage CPU transition was not confirmed.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_attempts_share_the_owner_budget() {
        let budget = crate::microvm::budget::Budget {
            slots: 6,
            limits: crate::nodes::Resources {
                cpu: 12,
                memory_mi_b: 32768,
                disk_mi_b: 512000,
            },
        };
        let properties = budget_properties(&budget);
        assert!(properties.contains(&"CPUQuota=1200%".into()));
        assert!(properties.contains(&format!("MemoryMax={}", 32768_u64 * 1_048_576)));
        assert!(properties.contains(&"MemorySwapMax=0".into()));
        assert!(workload_slice().unwrap().ends_with("-workloads.slice"));
    }

    #[tokio::test]
    async fn legacy_or_incomplete_cgroups_cannot_admit_native_execution() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            require_controllers(root.path()).await.unwrap_err().status,
            503
        );
        tokio::fs::write(root.path().join("cgroup.controllers"), b"cpu io pids")
            .await
            .unwrap();
        assert_eq!(
            require_controllers(root.path()).await.unwrap_err().status,
            503
        );
        tokio::fs::write(
            root.path().join("cgroup.controllers"),
            b"cpuset cpu io memory pids",
        )
        .await
        .unwrap();
        require_controllers(root.path()).await.unwrap();
    }
}
