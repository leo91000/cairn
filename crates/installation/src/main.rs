use cairn_installation::{
    config::Config,
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::Path, process::ExitCode, sync::Arc, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// The supervisor passes its control socket as file descriptor 3.
fn has_control_socket() -> bool {
    let mut kind = 0_i32;
    let mut size = std::mem::size_of::<i32>() as libc::socklen_t;
    let status = unsafe {
        libc::getsockopt(
            3,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut size,
        )
    };
    status == 0 && kind == libc::SOCK_STREAM
}

fn main() -> ExitCode {
    main_with_router(cairn_installation::http::router)
}

/// The worker test example supplies a synthetic relay transport; the shipped binary always uses
/// the installation router. This is an in-process seam, never an environment flag.
pub fn main_with_router<F, Fut>(router: F) -> ExitCode
where
    F: FnOnce(Arc<Service>) -> Fut,
    Fut: std::future::Future<Output = Result<axum::Router>>,
{
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    // Validate the inherited control socket before Tokio creates any FDs.
    if args.first().is_some_and(|a| a == "supervise") && !has_control_socket() {
        eprintln!("Missing supervisor control socket.");
        return ExitCode::FAILURE;
    }
    #[cfg(feature = "ublk")]
    if args.first().is_some_and(|a| a == "runner-broker")
        && std::env::var("CAIRN_BLOCK_TRANSPORT").is_ok_and(|value| value == "ublk")
        && let Err(error) = cairn_installation::storage::ublk::prepare_io_threads()
    {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,cairn_performance=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let threads = std::env::var("CAIRN_HTTP_THREADS")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| *n > 0 && *n <= 32)
        .unwrap_or(2);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .expect("Tokio runtime");
    match runtime.block_on(entry(args, router)) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("{}", error.message);
            ExitCode::FAILURE
        }
    }
}

async fn shutdown(stop: CancellationToken) {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal handler");
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    stop.cancel();
}

fn arg<'a>(args: &'a [String], index: usize, missing: &str) -> Result<&'a String> {
    args.get(index).ok_or_else(|| Error::bad(missing))
}

async fn entry<F, Fut>(args: Vec<String>, router: F) -> Result<i32>
where
    F: FnOnce(Arc<Service>) -> Fut,
    Fut: std::future::Future<Output = Result<axum::Router>>,
{
    let mode = args.first().map_or("serve", String::as_str);
    if ["--version", "-V", "version"].contains(&mode) {
        println!("cairn {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if mode == "supervise" {
        return cairn_installation::supervisor::entry(
            arg(&args, 1, "Missing supervised executable.")?,
            &args[2..],
        )
        .await;
    }
    let stop = CancellationToken::new();
    tokio::spawn(shutdown(stop.clone()));
    match mode {
        "rotate-token" => {
            let config = Config::load()?;
            cairn_installation::relay::rotate_token(&config.data_dir.join("installation-relay"))
                .await?;
            println!(
                "Installation credential rotated. Restart the manager to load its new identity."
            );
        }
        "claim" => {
            let config = Config::load()?;
            let beacon = std::env::var("CAIRN_BEACON_ORIGIN").ok();
            let name = std::env::var("CAIRN_INSTALLATION_NAME")
                .unwrap_or_else(|_| "My installation".into());
            cairn_installation::relay::device_claim(
                beacon.as_deref(),
                &config.data_dir.join("installation-relay"),
                &name,
                stop,
                |url, code, name, fingerprint| {
                    println!("Open {url} and review this device claim code: {code}");
                    println!("Installation: {name}");
                    println!("Installation fingerprint: {fingerprint}");
                    println!("Compare this fingerprint in the app before confirming the claim.");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                },
            )
            .await?;
            println!("Installation claimed. Restart the manager to load its new identity.");
        }
        "node-enroll" => {
            cairn_installation::nodes::connector::enroll(
                arg(&args, 1, "Missing master HTTPS origin.")?,
                Path::new(arg(&args, 2, "Missing private node directory.")?),
            )
            .await?;
        }
        "node-daemon" => {
            let directory = arg(&args, 1, "Missing node identity directory.")?;
            cairn_installation::nodes::daemon(Path::new(directory), stop).await?;
        }
        "node-connect" => {
            let directory = arg(&args, 1, "Missing private node directory.")?;
            cairn_installation::nodes::connector::connect(Path::new(directory), stop).await?;
        }
        "guest" => cairn_installation::microvm::guest::serve(stop).await?,
        "toolkit-env" => {
            let config = Config::load()?;
            let environment =
                cairn_installation::toolkit::environment(&config.home, std::env::vars().collect())
                    .await?;
            println!("{}", serde_json::to_string(&environment)?);
        }
        "prepare-execution" => prepare_execution().await?,
        "runner-broker" => cairn_installation::runner::serve(stop).await?,
        "runner-client" => {
            let runner = arg(&args, 1, "Missing runner identifier.")?;
            return cairn_installation::runner::client(runner, stop).await;
        }
        "chat" => return chat_command(args.get(1), stop).await,
        "codex-service" => {
            let config = Config::load()?;
            let home =
                std::env::var("CODEX_HOME").map_err(|_| Error::bad("Missing Codex home."))?;
            let socket = arg(&args, 1, "Missing Codex service socket.")?;
            cairn_installation::chat_process::resident::serve_with_ready(
                &config,
                Path::new(&home),
                Path::new(socket),
                stop,
                || {
                    let mut stdout = std::io::stdout().lock();
                    std::io::Write::write_all(&mut stdout, b"{\"ready\":true}\n")?;
                    std::io::Write::flush(&mut stdout)?;
                    Ok(())
                },
            )
            .await?;
        }
        "runner-entry" => return runner_entry(stop).await,
        "serve" => serve(stop, router).await?,
        _ => {
            return Err(Error::bad(
                "Unknown command. Use serve, claim, rotate-token, runner-broker, runner-entry, runner-client, chat, or --version.",
            ));
        }
    }
    Ok(0)
}

async fn read_stdin_json() -> Result<Value> {
    let bytes = cairn_installation::process::read_bounded(tokio::io::stdin(), 8_000_000).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn prepare_execution() -> Result<()> {
    let input = read_stdin_json().await?;
    let config: Config = serde_json::from_value(input["config"].clone())?;
    let mut run = input["run"].clone();
    let mut prepared =
        cairn_installation::execution::prepare(&run, &config, None, None, None).await?;
    run["workspace"] = prepared["cwd"].clone();
    run["workspaces"] = prepared["workspaces"].clone();
    prepared["args"] = serde_json::to_value(cairn_installation::run_output::args(
        &run,
        text(&prepared, "output"),
        None,
    ))?;
    println!("{prepared}");
    Ok(())
}

async fn chat_command(binary: Option<&String>, stop: CancellationToken) -> Result<i32> {
    let plan = read_stdin_json().await?;
    let mut config = Config::load()?;
    if let Some(binary) = binary {
        if plan["provider"] == "claude" {
            config.claude_bin = binary.clone();
        } else {
            config.codex_bin = binary.clone();
        }
    }
    chat(&config, plan, stop).await
}

async fn runner_entry(stop: CancellationToken) -> Result<i32> {
    let plan: Value = serde_json::from_slice(&tokio::fs::read("/run/cairn-plan.json").await?)?;
    let prepared = std::time::Instant::now();
    let run_id = cairn_installation::performance::identity(text(&plan, "runId"));
    let attempt_id = cairn_installation::performance::identity(text(&plan, "id"));
    let mut env = cairn_installation::toolkit::environment(
        Path::new("/home/node"),
        std::env::vars().collect(),
    )
    .await?;
    tracing::info!(target: "cairn_performance", operation = "runner_entry", run_id, attempt_id,
        phase = "toolkit", elapsed_ms = prepared.elapsed().as_millis() as u64);
    if let Some(token) = plan["mcpEnv"]["CAIRN_MCP_RUN_TOKEN"].as_str() {
        env.insert("CAIRN_MCP_RUN_TOKEN".into(), token.into());
    }
    let cwd = Path::new(text(&plan, "cwd"));
    if plan["chat"].is_object() {
        // Launch a Rust chat process with its complete private environment;
        // changing global process environment after Tokio starts is unsafe.
        let binary = std::env::current_exe()?;
        let claude = plan["chat"]["provider"] == "claude";
        if claude {
            env.insert("CLAUDE_CONFIG_DIR".into(), "/home/node/.claude".into());
        }
        let provider = if claude { "claude" } else { "codex" };
        let args = vec!["chat".to_owned(), provider.to_owned()];
        let binary = binary
            .to_str()
            .ok_or_else(|| Error::bad("Invalid executable path."))?;
        let command = cairn_installation::process::command(binary, &args, &env, Some(cwd));
        return agent_command(command, plan["chat"].to_string(), stop).await;
    }
    let invalid = || Error::bad("Invalid runner command.");
    let args = plan["args"]
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or_else(invalid))
        .collect::<Result<Vec<_>>>()?;
    let command = cairn_installation::process::command("codex", &args, &env, Some(cwd));
    agent_command(command, text(&plan, "prompt").into(), stop).await
}

/// Runs a periodic maintenance job until shutdown; failures are logged and retried.
fn spawn_periodic(
    service: &Arc<Service>,
    operation: &'static str,
    period: Duration,
) -> tokio::task::JoinHandle<()> {
    let s = service.clone();
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = s.shutdown.cancelled() => break,
                _ = timer.tick() => {
                    let result = s.accounts.poll(&s, true).await;
                    if let Err(error) = result {
                        tracing::warn!(operation, error = %error, "Background operation failed");
                    }
                }
            }
        }
    })
}

async fn serve<F, Fut>(stop: CancellationToken, make_router: F) -> Result<()>
where
    F: FnOnce(Arc<Service>) -> Fut,
    Fut: std::future::Future<Output = Result<axum::Router>>,
{
    let config = Config::load()?;
    cairn_installation::toolkit::environment(&config.home, std::env::vars().collect()).await?;
    let service = Service::new(config).await?;
    let listener =
        tokio::net::TcpListener::bind((service.config.host.as_str(), service.config.port)).await?;
    let router = make_router(service.clone()).await?;
    let mut background = Vec::new();
    let relay_directory = service.config.data_dir.join("installation-relay");
    let identity_exists = tokio::fs::try_exists(relay_directory.join("identity.json")).await?;
    if !identity_exists && let Ok(code) = std::env::var("CAIRN_INSTALLATION_CLAIM_CODE") {
        let claimed = async {
            let origin = std::env::var("CAIRN_BEACON_ORIGIN")
                .map_err(|_| Error::bad("Set CAIRN_BEACON_ORIGIN to claim this installation."))?;
            let name = std::env::var("CAIRN_INSTALLATION_NAME")
                .unwrap_or_else(|_| "My installation".into());
            cairn_installation::relay::claim(&origin, &relay_directory, &code, &name).await
        }
        .await;
        if let Err(error) = claimed {
            tracing::warn!(
                error = %error,
                "Installation claim failed; continuing without a relay. Check the beacon origin and obtain a new claim code before restarting"
            );
        }
    }

    if identity_exists || tokio::fs::try_exists(relay_directory.join("identity.json")).await? {
        let relay_router = router.clone();
        let relay_stop = service.shutdown.clone();
        let relay_service = service.clone();
        background.push(tokio::spawn(async move {
            if cairn_installation::relay::connect(
                relay_directory,
                relay_router,
                relay_service,
                relay_stop,
            )
            .await
            .is_err()
            {
                tracing::error!(
                    "Installation relay could not start; check the private installation identity"
                );
            }
        }));
    }

    if service.config.worker_enabled {
        service.worker.start(service.clone()).await?;
        background.push(spawn_periodic(
            &service,
            "accounts",
            Duration::from_secs(15),
        ));
    }
    println!("Listening on http://{}", listener.local_addr()?);
    let s = service.clone();
    let closing = async move {
        stop.cancelled().await;
        s.shutdown.cancel();
    };
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(closing)
    .await?;
    service.shutdown.cancel();
    service.avatars.close().await;
    service.worker.close().await;
    service.connections.cancel().await;
    if let Err(error) = service.accounts.cancel(&service).await {
        tracing::warn!(error = %error, "Account shutdown failed");
    }
    for task in background {
        // A panicked background job has already been reported by the runtime.
        let _ = task.await;
    }
    Ok(())
}

async fn chat(config: &Config, plan: Value, stop: CancellationToken) -> Result<i32> {
    let home = std::env::var("CODEX_HOME").map_err(|_| Error::bad("Missing Codex home."))?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(32);
    let run_id = cairn_installation::performance::identity(text(&plan, "runId"));
    let attempt_id = cairn_installation::performance::identity(text(&plan, "attemptId"));
    let span =
        tracing::info_span!(target: "cairn_performance", "agent_attempt", run_id, attempt_id);
    let provider = if plan["provider"] == "claude" {
        cairn_installation::provider::Provider::Claude
    } else {
        cairn_installation::provider::Provider::Codex
    };
    let mut activity =
        cairn_installation::performance::Activity::new(run_id, attempt_id, "adapter", provider);
    let output = tokio::spawn(
        async move {
            let mut stdout = tokio::io::stdout();
            let mut heartbeat = cairn_installation::performance::heartbeat();
            loop {
                let value = tokio::select! {
                    value = rx.recv() => match value {
                        Some(value) => value,
                        None => break,
                    },
                    _ = heartbeat.tick() => {
                        activity.heartbeat("receive_agent_event");
                        continue;
                    }
                };
                activity.output();
                activity.observe(&value);
                let mut bytes = value.to_string().into_bytes();
                bytes.push(b'\n');
                cairn_installation::performance::wait("stdout_write", stdout.write_all(&bytes))
                    .await?;
            }
            // Tokio stdout can still have a blocking write in flight. The final
            // turn marker must reach the worker before this process reports success.
            cairn_installation::performance::wait("stdout_flush", stdout.flush()).await?;
            activity.finish();
            Ok::<_, std::io::Error>(())
        }
        .instrument(span.clone()),
    );
    let result = async {
        if plan["provider"] == "claude" {
            return cairn_installation::claude_process::run(config, plan, tx.clone(), stop).await;
        }
        if let Ok(socket) = std::env::var("CAIRN_CODEX_SERVICE") {
            return cairn_installation::chat_process::resident::run(
                Path::new(&socket),
                plan,
                tx.clone(),
                stop,
            )
            .await;
        }
        cairn_installation::chat_process::run(config, Path::new(&home), plan, tx.clone(), stop)
            .await
    }
    .instrument(span)
    .await;
    if let Err(error) = &result {
        let failure = json!({
            "type": "turn.failed",
            "error": {
                "message": error.message
            }
        });
        // The output task only stops when stdout is gone; nobody would read the event.
        let _ = tx.send(failure).await;
    }
    drop(tx);
    output.await.map_err(Error::internal)??;
    Ok(i32::from(result.is_err()))
}

fn signal_group(pid: u32, signal: libc::c_int) {
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

async fn agent_command(
    mut command: tokio::process::Command,
    prompt: String,
    stop: CancellationToken,
) -> Result<i32> {
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    tokio::spawn(async move {
        // The agent may exit before reading its whole prompt.
        let _ = stdin.write_all(prompt.as_bytes()).await;
    });
    let code = tokio::select! {
        result = child.wait() => result?.code().unwrap_or(1),
        () = stop.cancelled() => {
            signal_group(pid, libc::SIGTERM);
            if tokio::time::timeout(Duration::from_secs(2), child.wait()).await.is_err() {
                signal_group(pid, libc::SIGKILL);
                let _ = child.wait().await;
            }
            143
        }
    };
    signal_group(pid, libc::SIGKILL);
    Ok(code)
}
