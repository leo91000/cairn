//! Synthetic authenticated installation launcher. Never shipped as `cairn`.
#[path = "network_qualification/fault.rs"]
mod fault;

use cairn_installation::{
    config::Config,
    direct::{DirectConnections, peer},
    http, relay,
    service::Service,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
};
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

async fn control(
    path: PathBuf,
    plan: Arc<Mutex<fault::Plan>>,
    stop: CancellationToken,
) -> Result<()> {
    let listener = UnixListener::bind(path)?;
    loop {
        let socket = tokio::select! {
            () = stop.cancelled() => return Ok(()),
            socket = listener.accept() => socket?.0,
        };
        let (read, mut write) = socket.into_split();
        let mut reader = BufReader::new(read).take(4096);
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reader.read_line(&mut line),
        )
        .await??;
        let command: Value = serde_json::from_str(&line)?;
        let result = {
            let mut plan = plan.lock().unwrap();
            match command["command"].as_str() {
                Some("arm") => {
                    let mode = match command["mode"].as_str() {
                        Some("recoverable") => fault::Mode::Recoverable,
                        Some("starve") => fault::Mode::Starve,
                        Some("headers-only") => fault::Mode::HeadersOnly,
                        Some("opening") => fault::Mode::Opening,
                        Some("pass") => fault::Mode::Pass,
                        _ => return Err("unknown fixture mode".into()),
                    };
                    plan.arm(
                        mode,
                        command["fragmented"].as_bool().unwrap_or(false),
                        command["nextPeer"].as_bool().unwrap_or(true),
                    );
                    let opening_peers = command["openingPeers"].as_u64().unwrap_or(1);
                    if !(1..=2).contains(&opening_peers) {
                        return Err("invalid fixture opening peer count".into());
                    }
                    plan.opening_peers = opening_peers as usize;
                }
                Some("stats") => {}
                _ => return Err("unknown fixture command".into()),
            }
            json!({
                "armed": plan.armed,
                "attempts": plan.attempts,
                "droppedDatagrams": plan.dropped_datagrams,
                "droppedDataChunks": plan.dropped_data_chunks,
                "passedTarget": plan.passed_target,
                "attemptTimesMs": plan.times_ms,
                "errors": plan.errors,
                "passedHeaders": plan.passed_headers,
                "droppedHandshake": plan.dropped_handshake,
                "openingPeers": plan.opening_peers,
                "openingDrops": plan.opening_drops,
                "dispatch": plan.dispatch,
            })
        };
        write.write_all(format!("{result}\n").as_bytes()).await?;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 2 {
        return Err("qualification launcher needs private control socket path".into());
    }
    cairn_installation::tls::install_provider();
    let stop = CancellationToken::new();
    let config = Config::load()?;
    if config.worker_enabled {
        return Err("qualification workers must be disabled".into());
    }
    let service = Service::new(config).await?;
    let router = http::router(service.clone()).await?;
    let directory = service.config.data_dir.join("installation-relay");
    relay::claim(
        &std::env::var("CAIRN_BEACON_ORIGIN")?,
        &directory,
        &std::env::var("CAIRN_INSTALLATION_CLAIM_CODE")?,
        "Synthetic qualification installation",
    )
    .await?;
    let direct = DirectConnections::default();
    let config = peer::PeerConfig::load()?;
    if !config.enabled {
        return Err("qualification requires direct connections enabled".into());
    }
    let plan = Arc::new(Mutex::new(fault::Plan::default()));
    let count = AtomicUsize::new(0);
    let factory_plan = plan.clone();
    let factory: peer::PeerFactory = Arc::new(move || {
        fault::dependencies(count.fetch_add(1, Ordering::Relaxed), factory_plan.clone())
    });
    let direct_router = router.clone().layer(axum::middleware::from_fn_with_state(
        plan.clone(),
        |axum::extract::State(plan): axum::extract::State<Arc<Mutex<fault::Plan>>>,
         request: axum::extract::Request,
         next: axum::middleware::Next| async move {
            use axum::body::HttpBody;
            let method = match request.method().as_str() {
                "GET" => "GET",
                "POST" => "POST",
                "HEAD" => "HEAD",
                _ => "OTHER",
            };
            plan.lock().unwrap().dispatch_event(
                "request-dispatch",
                method,
                request.body().size_hint().exact(),
            );
            let response = next.run(request).await;
            plan.lock().unwrap().dispatch_event(
                "response-produced",
                method,
                response.body().size_hint().exact(),
            );
            response
        },
    ));
    let peers = tokio::spawn(peer::run_with_factory(
        direct_router,
        direct.clone(),
        config,
        stop.clone(),
        factory,
    ));
    let relay = tokio::spawn(relay::connect_with_direct(
        directory,
        router.clone(),
        service.clone(),
        stop.clone(),
        direct,
    ));
    let controls = tokio::spawn(control(PathBuf::from(&args[1]), plan, stop.clone()));
    let listener =
        tokio::net::TcpListener::bind((service.config.host.as_str(), service.config.port)).await?;
    let serving = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown({
        let stop = stop.clone();
        async move { stop.cancelled().await }
    });
    let serving = std::future::IntoFuture::into_future(serving);
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => result?,
        () = async {
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("fixture signal handler");
            tokio::select! {
                _ = terminate.recv() => {},
                _ = tokio::signal::ctrl_c() => {},
            }
        } => {},
    }
    stop.cancel();
    service.shutdown.cancel();
    peers.await?;
    relay.await??;
    controls.await??;
    Ok(())
}
