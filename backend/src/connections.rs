use crate::{
    config::{Config, now},
    error::{Error, Result},
    process::{bounded_output, codex_environment, command},
    run_status::RunStatus,
    service::Service,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    path::Path,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Child,
    sync::{Mutex, watch},
};
use tokio_util::sync::CancellationToken;

static CODE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b[A-Z0-9]{4}-[A-Z0-9]{4,5}\b").unwrap());

static URL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"https://github\.com/[\w/-]+").unwrap());

const LOGIN_FAILED: &str =
    "Sign-in did not complete. Retry or use the documented container login command.";

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginState {
    Pending,
    Complete,
    Failed,
}

/// Progress of a device sign-in, as shown to the owner.
#[derive(Clone, Serialize)]
pub struct LoginFlow {
    provider: &'static str,
    state: LoginState,
    url: String,
    code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
}

#[derive(Clone)]
pub struct DeviceLogin {
    pub flow: watch::Sender<LoginFlow>,
    pub stop: CancellationToken,
    finished: watch::Receiver<bool>,
}

impl DeviceLogin {
    /// GitHub's device flow through the `gh` CLI.
    pub fn start(config: &Config, home: &Path) -> Result<Self> {
        let args = [
            "auth",
            "login",
            "--hostname",
            "github.com",
            "--git-protocol",
            "https",
            "--web",
            "--scopes",
            "workflow",
        ]
        .map(str::to_owned);
        let mut command = command(
            &config.gh_bin,
            &args,
            &codex_environment(config, &home.join(".codex")),
            None,
        );
        command.stdin(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(|_| {
            Error::unavailable("Unable to start sign-in. Check the CLI installation.")
        })?;
        let (flow, _) = watch::channel(LoginFlow {
            provider: "github",
            state: LoginState::Pending,
            url: String::new(),
            code: String::new(),
            error: None,
        });
        let (done, finished) = watch::channel(false);
        let stop = CancellationToken::new();
        let login = Self {
            flow: flow.clone(),
            stop: stop.clone(),
            finished,
        };
        tokio::spawn(async move {
            let success = follow(&mut child, &flow, &stop).await;
            if !success {
                crate::rpc::terminate(&mut child).await;
            }
            flow.send_modify(|flow| {
                if success {
                    flow.state = LoginState::Complete;
                } else {
                    flow.state = LoginState::Failed;
                    flow.error = Some(LOGIN_FAILED);
                }
            });
            let _ = done.send(true);
        });
        Ok(login)
    }

    pub fn running(&self) -> bool {
        !*self.finished.borrow()
    }

    pub async fn wait(&self) {
        let mut finished = self.finished.clone();
        while !*finished.borrow() {
            if finished.changed().await.is_err() {
                break;
            }
        }
    }

    pub async fn cancel(&self) {
        self.stop.cancel();
        self.wait().await;
    }

    pub fn view(&self) -> Value {
        serde_json::to_value(&*self.flow.borrow()).expect("login flows serialize")
    }
}

/// Relays the device code and URL from the CLI output until it exits, is
/// cancelled or times out. Returns whether sign-in succeeded.
async fn follow(
    child: &mut Child,
    flow: &watch::Sender<LoginFlow>,
    stop: &CancellationToken,
) -> bool {
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // Accept the CLI's "press Enter" prompt; it may not read stdin at all.
    let _ = stdin.write_all(b"\n").await;
    drop(stdin);
    let mut buffer = Vec::new();
    let mut out = [0; 4096];
    let mut err = [0; 4096];
    let mut stdout_open = true;
    let mut stderr_open = true;
    let deadline = tokio::time::sleep(Duration::from_secs(15 * 60));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = stop.cancelled() => return false,
            () = &mut deadline => return false,
            result = child.wait() => return result.is_ok_and(|s| s.success()),
            result = stdout.read(&mut out), if stdout_open => match result {
                Ok(0) | Err(_) => stdout_open = false,
                Ok(n) => receive(flow, &mut buffer, &out[..n]),
            },
            result = stderr.read(&mut err), if stderr_open => match result {
                Ok(0) | Err(_) => stderr_open = false,
                Ok(n) => receive(flow, &mut buffer, &err[..n]),
            },
        }
    }
}

fn receive(flow: &watch::Sender<LoginFlow>, buffer: &mut Vec<u8>, bytes: &[u8]) {
    buffer.extend_from_slice(bytes);
    if buffer.len() > 16000 {
        buffer.drain(..buffer.len() - 16000);
    }
    let output = String::from_utf8_lossy(buffer);
    flow.send_modify(|flow| {
        if let Some(code) = CODE.find(&output) {
            flow.code = code.as_str().into();
        }
        if let Some(url) = URL.find(&output) {
            flow.url = url.as_str().into();
        }
    });
}

#[derive(Default)]
pub struct Connections {
    cache: Mutex<Option<(i64, Value)>>,
    pub login: Mutex<Option<DeviceLogin>>,
}

impl Connections {
    pub async fn status(&self, s: &Service, force: bool) -> Result<Value> {
        let mut cache = self.cache.lock().await;
        if !force
            && let Some((at, value)) = &*cache
            && now() - at < 30000
        {
            return Ok(value.clone());
        }
        let value = serde_json::to_value([check(&s.config).await])?;
        *cache = Some((now(), value.clone()));
        Ok(value)
    }

    pub async fn start(&self, s: &Arc<Service>) -> Result<Value> {
        let mut login = self.login.lock().await;
        if login.as_ref().is_some_and(DeviceLogin::running) {
            return Err(Error::conflict("A sign-in is already in progress."));
        }
        let owner = format!("github-login-{}", crate::config::id());
        let lease = s.worker.deployment_lease(s, owner.clone(), false).await?;
        let active = async {
            Ok::<bool, Error>(
                lease["activeRuns"].as_u64().unwrap_or(1) > 0
                    || s.store
                        .read(|db| {
                            Ok(db.active()?.iter().any(|run| {
                                run["status"] == RunStatus::Running
                                    || run["recoveryPending"] == true
                            }))
                        })
                        .await?,
            )
        }
        .await;
        let flow = match active {
            Ok(false) => DeviceLogin::start(&s.config, &s.config.home),
            Ok(true) => Err(Error::conflict(
                "Wait for active runs to finish before changing the shared GitHub connection.",
            )),
            Err(error) => Err(error),
        };
        let flow = match flow {
            Ok(flow) => flow,
            Err(error) => {
                s.worker.deployment_lease(s, owner, true).await?;
                return Err(error);
            }
        };
        let device = flow.clone();
        let service = s.clone();
        tokio::spawn(async move {
            device.wait().await;
            if let Err(error) = service.worker.deployment_lease(&service, owner, true).await {
                tracing::warn!(%error, "could not release the GitHub sign-in deployment lease");
            }
        });
        let result = flow.view();
        *login = Some(flow);
        *self.cache.lock().await = None;
        Ok(result)
    }

    pub async fn flow(&self) -> Value {
        self.login
            .lock()
            .await
            .as_ref()
            .map_or(Value::Null, DeviceLogin::view)
    }

    pub async fn cancel(&self) {
        if let Some(login) = self.login.lock().await.take() {
            login.cancel().await;
        }
        *self.cache.lock().await = None;
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GithubStatus {
    provider: &'static str,
    installed: bool,
    connected: bool,
    account: String,
    version: String,
    /// Reported (possibly as null: unknown) only when the CLI is installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_permission: Option<Option<bool>>,
}

async fn check(config: &Config) -> GithubStatus {
    let env = codex_environment(config, &config.home.join(".codex"));
    let unavailable = GithubStatus {
        provider: "github",
        installed: false,
        connected: false,
        account: "CLI not installed".into(),
        version: String::new(),
        workflow_permission: None,
    };
    let Ok(version) = bounded_output(
        command(&config.gh_bin, &["--version".into()], &env, None),
        Duration::from_secs(5),
        10000,
    )
    .await
    else {
        return unavailable;
    };
    if !version.success {
        return unavailable;
    }
    let args = ["api", "--include", "user", "--jq", ".login"].map(str::to_owned);
    let login = bounded_output(
        command(&config.gh_bin, &args, &env, None),
        Duration::from_secs(10),
        10000,
    )
    .await
    .ok()
    .filter(|o| o.success);
    let account = login.as_ref().map_or_else(
        || "Not signed in".to_owned(),
        |o| o.stdout.lines().last().unwrap_or("").trim().to_owned(),
    );
    GithubStatus {
        provider: "github",
        installed: true,
        connected: login.is_some(),
        account,
        version: version.stdout.lines().next().unwrap_or("").to_owned(),
        workflow_permission: Some(login.as_ref().and_then(|o| workflow_scope(&o.stdout))),
    }
}

fn workflow_scope(output: &str) -> Option<bool> {
    output.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("x-oauth-scopes")
            .then(|| value.split(',').any(|scope| scope.trim() == "workflow"))
    })
}

#[cfg(test)]
mod capability_tests {
    #[test]
    fn workflow_scope_is_exact_and_unknown_for_fine_grained_tokens() {
        assert_eq!(
            super::workflow_scope("X-OAuth-Scopes: repo, workflow\r\n\r\nleo"),
            Some(true)
        );
        assert_eq!(
            super::workflow_scope("x-oauth-scopes: repo, not-workflow"),
            Some(false)
        );
        assert_eq!(super::workflow_scope("HTTP/2 200\r\n\r\nleo"), None);
    }
}
