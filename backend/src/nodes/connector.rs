//! Outbound node registration and presence. Credentials never enter process arguments.
use crate::{
    error::{Error, Result},
    skills::private_dir,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identity {
    master: String,
    node_id: String,
    token: String,
}
fn master(input: &str) -> Result<url::Url> {
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
    std::env::var("APP_RUNTIME_ID").unwrap_or_else(|_| format!("leo-{}", env!("CARGO_PKG_VERSION")))
}
pub fn capabilities(path: &Path) -> Result<Value> {
    if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
        return Err(Error::bad("Nodes require Linux x86-64."));
    }
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let kvm = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok_and(|file| unsafe { libc::ioctl(file.as_raw_fd(), 0xae00) } == 12);
    let memory = std::fs::read_to_string("/proc/meminfo")?
        .lines()
        .find_map(|line| {
            line.strip_prefix("MemTotal:")
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse::<u64>().ok())
        })
        .ok_or_else(|| Error::bad("Cannot detect node memory."))?
        / 1024;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::bad("Invalid node directory."))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stat = unsafe { stat.assume_init() };
    let disk =
        (stat.f_blocks as u128 * stat.f_frsize as u128 / 1_048_576).min(u64::MAX as u128) as u64;
    Ok(
        json!({"os":"linux","arch":"x86_64","kvm":kvm,"cpu":std::thread::available_parallelism()?.get(),"memoryMiB":memory,"diskMiB":disk}),
    )
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
            Error::new(
                409,
                "Node identity already exists or its directory is not writable.",
            )
        })?;
    let result: Result<()> = async {
        let code = crate::process::read_bounded(tokio::io::stdin(), 256).await?;
        let code = std::str::from_utf8(&code).map_err(|_| Error::bad("Invalid enrollment code."))?.trim();
        let response = client()?.post(origin.join("internal/nodes/enroll").map_err(Error::internal)?)
            .json(&json!({"code":code,"name":"Linux node","protocol":1,"capabilities":capabilities(directory)?,"runtimeId":runtime()})).send().await.map_err(|_|Error::new(503,"Cannot reach the master."))?;
        if !response.status().is_success() { return Err(Error::new(response.status().as_u16(),"Node enrollment was rejected. Check the code and master version.")); }
        let value: Value = response.json().await.map_err(|_|Error::bad("Invalid enrollment response."))?;
        let node_id = value["nodeId"].as_str().ok_or_else(||Error::bad("Missing node identity."))?;
        crate::validation::uuid(node_id)?;
        let token = value["token"].as_str().filter(|v|v.len()==43).ok_or_else(||Error::bad("Missing node credential."))?;
        let identity = Identity { master:origin.to_string(),node_id:node_id.into(),token:token.into() };
        file.write_all(&serde_json::to_vec(&identity)?).await?;
        file.sync_all().await?;
        Ok(())
    }.await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(identity_path).await;
    }
    result?;
    println!("Node enrolled. Identity stored privately; no execution has been started.");
    Ok(())
}
pub async fn connect(directory: &Path, stop: CancellationToken) -> Result<()> {
    let identity: Identity =
        serde_json::from_slice(&tokio::fs::read(directory.join("identity.json")).await?)?;
    let origin = master(&identity.master)?;
    let client = client()?;
    loop {
        let request = client
            .post(
                origin
                    .join("internal/nodes/heartbeat")
                    .map_err(Error::internal)?,
            )
            .bearer_auth(&identity.token)
            .json(&json!({"runtimeId":runtime()}))
            .send();
        let result =
            tokio::select! { _ = stop.cancelled() => return Ok(()), value = request => value };
        match result {
            Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
                return Err(Error::new(
                    401,
                    "Node identity revoked. Register the node again.",
                ));
            }
            Ok(response) if response.status().is_success() => {}
            _ => tracing::warn!("Node heartbeat failed; retrying without starting work."),
        }
        tokio::select! { _ = stop.cancelled() => return Ok(()), _ = tokio::time::sleep(Duration::from_secs(10)) => {} }
    }
}
