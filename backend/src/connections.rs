use crate::{
    config::{Config, now},
    error::{Error, Result},
    process::{bounded_output, codex_environment, command},
    service::Service,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, watch},
};
use tokio_util::sync::CancellationToken;

static CODE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b[A-Z0-9]{4}-[A-Z0-9]{4,5}\b").unwrap());

static URL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"https://github\.com/[\w/-]+").unwrap());

#[derive(Clone)]
pub struct DeviceLogin {
    pub flow: watch::Sender<Value>,
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
        ];
        let mut command = command(
            &config.gh_bin,
            &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            &codex_environment(config, &home.join(".codex")),
            None,
        );
        command.stdin(std::process::Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| Error::new(503, "Unable to start sign-in. Check the CLI installation."))?;
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let (flow, _) = watch::channel(json!({
            "provider": "github",
            "state": "pending",
            "url": "",
            "code": ""
        }));
        let (done, finished) = watch::channel(false);
        let stop = CancellationToken::new();
        let login = Self {
            flow: flow.clone(),
            stop: stop.clone(),
            finished,
        };
        tokio::spawn(async move {
            let _ = stdin.write_all(b"\n").await;
            drop(stdin);
            let mut buffer = Vec::new();
            let mut out = [0; 4096];
            let mut err = [0; 4096];
            let mut stdout_open = true;
            let mut stderr_open = true;
            let deadline = tokio::time::sleep(Duration::from_secs(15 * 60));
            tokio::pin!(deadline);
            let success = loop {
                tokio::select! {
                    _ = stop.cancelled() => break false,
                    _ = &mut deadline => break false,
                    result = child.wait() => break result.is_ok_and(|s|s.success()),
                    result = stdout.read(&mut out),
                    if stdout_open => match result {
                Ok(0)|Err(_)=>stdout_open=false,Ok(n)=>receive(&flow,&mut buffer,&out[..n])}
                ,
                    result = stderr.read(&mut err),
                    if stderr_open => match result {
                Ok(0)|Err(_)=>stderr_open=false,Ok(n)=>receive(&flow,&mut buffer,&err[..n])}
                ,
                }
            };
            if !success {
                if let Some(pid) = child.id() {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGTERM);
                    }
                }
                if tokio::time::timeout(Duration::from_secs(2), child.wait())
                    .await
                    .is_err()
                {
                    let _ = child.kill().await;
                }
            }
            flow.send_modify(|value| {
                value["state"] = if success { "complete" } else { "failed" }.into();
                if !success {
                    value["error"] = "Sign-in did not complete. Retry or use the documented container login command.".into();
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
        self.flow.borrow().clone()
    }
}

fn receive(flow: &watch::Sender<Value>, buffer: &mut Vec<u8>, bytes: &[u8]) {
    buffer.extend_from_slice(bytes);
    if buffer.len() > 16000 {
        buffer.drain(..buffer.len() - 16000);
    }
    let output = String::from_utf8_lossy(buffer);
    flow.send_modify(|value| {
        if let Some(code) = CODE.find(&output) {
            value["code"] = code.as_str().into();
        }
        if let Some(url) = URL.find(&output) {
            value["url"] = url.as_str().into();
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
        let value = json!([check(&s.config).await]);
        *cache = Some((now(), value.clone()));
        Ok(value)
    }

    pub async fn start(&self, s: &Arc<Service>) -> Result<Value> {
        let mut login = self.login.lock().await;
        if login.as_ref().is_some_and(DeviceLogin::running) {
            return Err(Error::new(409, "A sign-in is already in progress."));
        }
        let owner = format!("github-login-{}", crate::config::id());
        let lease = s.worker.deployment_lease(s, owner.clone(), false).await?;
        let active = async {
            Ok::<bool, Error>(
                lease["activeRuns"].as_u64().unwrap_or(1) > 0
                    || s.store
                        .read(|db| {
                            Ok(db.active()?.iter().any(|run| {
                                run["status"] == "running" || run["recoveryPending"] == true
                            }))
                        })
                        .await?,
            )
        }
        .await;
        let flow = match active {
            Ok(false) => DeviceLogin::start(&s.config, &s.config.home),
            Ok(true) => Err(Error::new(
                409,
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
            let _ = service.worker.deployment_lease(&service, owner, true).await;
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
            .map(DeviceLogin::view)
            .unwrap_or(Value::Null)
    }

    pub async fn cancel(&self) {
        if let Some(login) = self.login.lock().await.take() {
            login.cancel().await;
        }
        *self.cache.lock().await = None;
    }
}

async fn check(config: &Config) -> Value {
    let env = codex_environment(config, &config.home.join(".codex"));
    let unavailable = json!({
        "provider": "github",
        "installed": false,
        "connected": false,
        "account": "CLI not installed",
        "version": ""
    });
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
    let login = bounded_output(
        command(
            &config.gh_bin,
            &["api", "--include", "user", "--jq", ".login"]
                .iter()
                .map(|s| (*s).into())
                .collect::<Vec<_>>(),
            &env,
            None,
        ),
        Duration::from_secs(10),
        10000,
    )
    .await
    .ok()
    .filter(|o| o.success);
    let account = login
        .as_ref()
        .map(|o| o.stdout.lines().last().unwrap_or("").trim().to_owned());
    json!({
        "workflowPermission": login.as_ref().and_then(|o| workflow_scope(&o.stdout)),
        "provider": "github",
        "installed": true,
        "version": version.stdout.lines().next().unwrap_or(""),
        "connected": login.is_some(),
        "account": account.as_deref().unwrap_or("Not signed in")
    })
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
