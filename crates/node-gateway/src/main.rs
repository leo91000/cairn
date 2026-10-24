use cairn_node_gateway::{Config, Gateway, configure};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "Usage: cairn-node-gateway configure BEACON INSTALLATION PIN | run";

fn root() -> PathBuf {
    std::env::var_os("CAIRN_GATEWAY_ROOT")
        .map_or_else(|| PathBuf::from("/var/lib/cairn-gateway"), PathBuf::from)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cairn_node_gateway=info".into()),
        )
        .with_writer(std::io::stderr)
        // systemd's journal records plain text.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = match arguments.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["configure", beacon, installation, pin] => configure(&root(), beacon, installation, pin)
            .map(|changed| println!("{}", if changed { "changed" } else { "unchanged" })),
        ["run"] => run().await,
        ["--version"] => {
            println!("cairn-node-gateway {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, USAGE)),
    };
    if let Err(error) = result {
        eprintln!("Cairn node gateway: {error}");
        std::process::exit(1);
    }
}

async fn run() -> std::io::Result<()> {
    let root = root();
    let gateway = Gateway::bind(Config::load(&root)?, &root).await?;
    println!(
        "Cairn node gateway listening on http://{}",
        gateway.local_addr()?
    );
    let stop = CancellationToken::new();
    let signal = stop.clone();
    tokio::spawn(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("signal handler");
        tokio::select! {
            _ = terminate.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        signal.cancel();
    });
    gateway.serve(stop).await;
    Ok(())
}
