use leo_agent_manager::{
    config::Config,
    error::{Error, Result},
    service::Service,
    validation::text,
};
use serde_json::Value;
use std::{path::Path, process::ExitCode};
use tokio_util::sync::CancellationToken;
fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|a| a == "supervise") {
        // Validate the inherited control socket before Tokio creates any FDs.
        let mut kind = 0_i32;
        let mut size = std::mem::size_of::<i32>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                3,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut kind as *mut i32).cast(),
                &mut size,
            )
        } != 0
            || kind != libc::SOCK_STREAM
        {
            eprintln!("Missing supervisor control socket.");
            return ExitCode::FAILURE;
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let threads = std::env::var("LEO_HTTP_THREADS")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| *n > 0 && *n <= 32)
        .unwrap_or(2);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .expect("Tokio runtime");
    match runtime.block_on(entry(args)) {
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
    _=terminate.recv()=>{
    }
    ,_=tokio::signal::ctrl_c()=>{
    }
    }
    stop.cancel();
}
async fn entry(args: Vec<String>) -> Result<i32> {
    let mode = args.first().map(String::as_str).unwrap_or("serve");
    if ["--version", "-V", "version"].contains(&mode) {
        println!("leo {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if mode == "supervise" {
        return leo_agent_manager::supervisor::entry(
            args.get(1)
                .ok_or_else(|| Error::bad("Missing supervised executable."))?,
            &args[2..],
        )
        .await;
    }
    let stop = CancellationToken::new();
    tokio::spawn(shutdown(stop.clone()));
    match mode {
        "node-enroll" => {
            leo_agent_manager::nodes::connector::enroll(
                args.get(1)
                    .ok_or_else(|| Error::bad("Missing master HTTPS origin."))?,
                Path::new(
                    args.get(2)
                        .ok_or_else(|| Error::bad("Missing private node directory."))?,
                ),
            )
            .await?;
            Ok(0)
        }
        "node-daemon" => {
            leo_agent_manager::nodes::daemon(
                Path::new(
                    args.get(1)
                        .ok_or_else(|| Error::bad("Missing node identity directory."))?,
                ),
                stop,
            )
            .await?;
            Ok(0)
        }
        "node-connect" => {
            leo_agent_manager::nodes::connector::connect(
                Path::new(
                    args.get(1)
                        .ok_or_else(|| Error::bad("Missing private node directory."))?,
                ),
                stop,
            )
            .await?;
            Ok(0)
        }
        "guest-warm" => {
            let config = Config::load()?;
            leo_agent_manager::toolkit::environment(&config.home, std::env::vars().collect())
                .await?;
            // Anonymous initialization warms executable pages and local capabilities only.
            // No thread, inference, account login or run-scoped MCP is started here.
            let mut session = leo_agent_manager::rpc::Session::codex(
                &config,
                &config.home.join(".codex"),
                &["-c".into(), "features.apps=false".into()],
                Some(Path::new("/tmp")),
            )
            .await?;
            let result = session.request("model/list", serde_json::json!({})).await;
            session.close().await;
            result?;
            Ok(0)
        }
        "guest" => {
            leo_agent_manager::microvm::guest::serve(stop).await?;
            Ok(0)
        }
        "toolkit-env" => {
            let config = Config::load()?;
            let environment =
                leo_agent_manager::toolkit::environment(&config.home, std::env::vars().collect())
                    .await?;
            println!("{}", serde_json::to_string(&environment)?);
            Ok(0)
        }
        "prepare-execution" => {
            let bytes =
                leo_agent_manager::process::read_bounded(tokio::io::stdin(), 8_000_000).await?;
            let input: Value = serde_json::from_slice(&bytes)?;
            let config: Config = serde_json::from_value(input["config"].clone())?;
            let mut run = input["run"].clone();
            let mut prepared =
                leo_agent_manager::execution::prepare(&run, &config, None, None, None).await?;
            run["workspace"] = prepared["cwd"].clone();
            run["workspaces"] = prepared["workspaces"].clone();
            prepared["args"] = serde_json::to_value(leo_agent_manager::run_output::args(
                &run,
                text(&prepared, "output"),
                None,
            ))?;
            println!("{prepared}");
            Ok(0)
        }
        "runner-broker" => {
            leo_agent_manager::runner::serve(stop).await?;
            Ok(0)
        }
        "runner-client" => {
            leo_agent_manager::runner::client(
                args.get(1)
                    .ok_or_else(|| Error::bad("Missing runner identifier."))?,
                stop,
            )
            .await
        }
        "chat" => {
            let bytes =
                leo_agent_manager::process::read_bounded(tokio::io::stdin(), 8_000_000).await?;
            let plan: Value = serde_json::from_slice(&bytes)?;
            let mut config = Config::load()?;
            if let Some(binary) = args.get(1) {
                if plan["provider"] == "claude" {
                    config.claude_bin = binary.clone();
                } else {
                    config.codex_bin = binary.clone();
                }
            }
            chat(&config, plan, stop).await
        }
        "runner-entry" => {
            let plan: Value =
                serde_json::from_slice(&tokio::fs::read("/run/leo-plan.json").await?)?;
            let mut env = leo_agent_manager::toolkit::environment(
                Path::new("/home/node"),
                std::env::vars().collect(),
            )
            .await?;
            if let Some(token) = plan["mcpEnv"]["LEO_MCP_RUN_TOKEN"].as_str() {
                env.insert("LEO_MCP_RUN_TOKEN".into(), token.into());
            }
            if plan["chat"].is_object() {
                // Launch a Rust chat process with its complete private environment;
                // changing global process environment after Tokio starts is unsafe.
                let binary = std::env::current_exe()?;
                let claude = plan["chat"]["provider"] == "claude";
                if claude {
                    env.insert("CLAUDE_CONFIG_DIR".into(), "/home/node/.claude".into());
                }
                let args = vec![
                    "chat".into(),
                    if claude { "claude" } else { "codex" }.into(),
                ];
                let command = leo_agent_manager::process::command(
                    binary
                        .to_str()
                        .ok_or_else(|| Error::bad("Invalid executable path."))?,
                    &args,
                    &env,
                    Some(Path::new(text(&plan, "cwd"))),
                );
                return agent_command(command, plan["chat"].to_string(), stop).await;
            }
            let args = plan["args"]
                .as_array()
                .ok_or_else(|| Error::bad("Invalid runner command."))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| Error::bad("Invalid runner command."))
                })
                .collect::<Result<Vec<_>>>()?;
            let command = leo_agent_manager::process::command(
                "codex",
                &args,
                &env,
                Some(Path::new(text(&plan, "cwd"))),
            );
            agent_command(command, text(&plan, "prompt").into(), stop).await
        }
        "serve" => {
            let mut config = Config::load()?;
            if config.setup_token.is_empty() {
                config.setup_token =
                    leo_agent_manager::execution::secret(&config.data_dir, "setup-token").await?;
            }
            let _ =
                leo_agent_manager::toolkit::environment(&config.home, std::env::vars().collect())
                    .await?;
            let service = Service::new(config).await?;
            let listener =
                tokio::net::TcpListener::bind((service.config.host.as_str(), service.config.port))
                    .await?;
            let router = leo_agent_manager::http::router(service.clone()).await?;
            let mut background = Vec::new();
            if service.config.worker_enabled {
                service.worker.start(service.clone()).await?;
                for kind in ["accounts", "notifications"] {
                    let s = service.clone();
                    background.push(tokio::spawn(async move {
                        let mut timer = tokio::time::interval(std::time::Duration::from_secs(if kind == "accounts" { 15 } else { 1 }));
                        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        loop {
                            tokio::select! {
                            _=s.shutdown.cancelled()=>break,_=timer.tick()=>{
                            let result=if kind=="accounts"{
                            s.accounts.poll(&s,true).await}
                            else{
                            s.notifications.flush(&s).await}
                            ;
                            if let Err(error)=result{
                            tracing::warn!(operation=kind,error=%error,"Background operation failed");
                            }
                            }
                            }
                        }
                    }));
                }
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
            let _ = service.accounts.cancel(&service).await;
            for task in background {
                let _ = task.await;
            }
            Ok(0)
        }
        _ => Err(Error::bad(
            "Unknown command. Use serve, runner-broker, runner-entry, runner-client, chat, or --version.",
        )),
    }
}
async fn chat(config: &Config, plan: Value, stop: CancellationToken) -> Result<i32> {
    use tokio::io::AsyncWriteExt;
    let home = std::env::var("CODEX_HOME").map_err(|_| Error::bad("Missing Codex home."))?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(32);
    let output = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(value) = rx.recv().await {
            let mut bytes = value.to_string().into_bytes();
            bytes.push(b'\n');
            stdout.write_all(&bytes).await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let result = if plan["provider"] == "claude" {
        leo_agent_manager::claude_process::run(config, plan, tx.clone(), stop).await
    } else {
        leo_agent_manager::chat_process::run(config, Path::new(&home), plan, tx.clone(), stop).await
    };
    if let Err(error) = &result {
        let _ = tx
            .send(serde_json::json!({
            "type":"turn.failed","error":{
            "message":error.message}
            }
            ))
            .await;
    }
    drop(tx);
    output.await.map_err(Error::internal)??;
    Ok(if result.is_ok() { 0 } else { 1 })
}
async fn agent_command(
    mut command: tokio::process::Command,
    prompt: String,
    stop: CancellationToken,
) -> Result<i32> {
    use tokio::io::AsyncWriteExt;
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    tokio::spawn(async move {
        let _ = stdin.write_all(prompt.as_bytes()).await;
    });
    let code = tokio::select! {
    result=child.wait()=>result?.code().unwrap_or(1),_=stop.cancelled()=>{
    unsafe{
    libc::kill(-(pid as i32),libc::SIGTERM);
    }
    if tokio::time::timeout(std::time::Duration::from_secs(2),child.wait()).await.is_err(){
    unsafe{
    libc::kill(-(pid as i32),libc::SIGKILL);
    }
    let _=child.wait().await;
    }
    143}
    };
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    Ok(code)
}
