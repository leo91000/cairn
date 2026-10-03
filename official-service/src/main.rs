use std::{env, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    http::StatusCode,
    routing::{any, get},
};
use leo_official_service::{EmailSender, router};
use reqwest::Client;
use serde_json::json;
use sqlx_postgres::PgPoolOptions;
use tower_http::services::{ServeDir, ServeFile};
use url::Url;

struct HttpEmailSender {
    client: Client,
    endpoint: String,
    key: String,
    from: String,
}

#[async_trait]
impl EmailSender for HttpEmailSender {
    async fn send_code(&self, email: &str, code: &str) -> Result<(), String> {
        let message = json!({
            "from": self.from,
            "to": [email],
            "subject": "Your Leo sign-in code",
            "text": format!("Your Leo sign-in code is {code}. It expires in 10 minutes. If you did not request it, ignore this email."),
        });
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.key)
            .json(&message)
            .send()
            .await
            .map_err(|_| "Email delivery failed".to_owned())?;

        if !response.status().is_success() {
            return Err("Email delivery failed".into());
        }

        Ok(())
    }
}

fn required(name: &str) -> Result<String, String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("Set {name}"))
}

async fn run() -> Result<(), String> {
    let configured_origin = required("LEO_OFFICIAL_ORIGIN")?;
    let origin_url = Url::parse(&configured_origin).map_err(|_| "Invalid LEO_OFFICIAL_ORIGIN")?;
    let loopback = matches!(
        origin_url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]")
    );
    if !(origin_url.scheme() == "https" || origin_url.scheme() == "http" && loopback)
        || origin_url.path() != "/"
        || origin_url.query().is_some()
        || origin_url.fragment().is_some()
        || !origin_url.username().is_empty()
        || origin_url.password().is_some()
    {
        return Err(
            "LEO_OFFICIAL_ORIGIN must be an HTTPS origin (HTTP is allowed only on loopback)".into(),
        );
    }
    let origin = origin_url.origin().ascii_serialization();

    let endpoint = env::var("LEO_OFFICIAL_EMAIL_ENDPOINT")
        .unwrap_or_else(|_| "https://api.resend.com/emails".into());
    let email_url = Url::parse(&endpoint).map_err(|_| "Invalid LEO_OFFICIAL_EMAIL_ENDPOINT")?;
    if email_url.scheme() != "https" && !(loopback && email_url.scheme() == "http") {
        return Err("LEO_OFFICIAL_EMAIL_ENDPOINT requires HTTPS outside development".into());
    }

    let sender = HttpEmailSender {
        client: Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Could not configure email delivery")?,
        endpoint,
        key: required("LEO_OFFICIAL_EMAIL_KEY")?,
        from: required("LEO_OFFICIAL_EMAIL_FROM")?,
    };

    let address: SocketAddr = env::var("LEO_OFFICIAL_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:4311".into())
        .parse()
        .map_err(|_| "Invalid LEO_OFFICIAL_LISTEN")?;

    let web = PathBuf::from(env::var("LEO_OFFICIAL_WEB_DIR").unwrap_or_else(|_| "dist".into()));
    if !web.join("official.html").is_file() {
        return Err(
            "Build the web application with pnpm build before starting the official service".into(),
        );
    }

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&required("LEO_OFFICIAL_DATABASE_URL")?)
        .await
        .map_err(|_| "Could not connect to the official Postgres database")?;

    let app = router(pool, Arc::new(sender), origin)
        .await
        .map_err(|_| "Official database migration failed")?
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/api/{*path}", any(|| async { StatusCode::NOT_FOUND }))
        .route_service("/", ServeFile::new(web.join("official.html")))
        .route_service("/index.html", ServeFile::new(web.join("official.html")))
        .fallback_service(ServeDir::new(&web).fallback(ServeFile::new(web.join("official.html"))));

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|_| "Could not bind LEO_OFFICIAL_LISTEN")?;

    tracing::info!("Official service listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Could not register shutdown signal");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    })
    .await
    .map_err(|_| "Official HTTP server stopped unexpectedly".to_owned())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    if let Err(error) = run().await {
        // Configuration and transport errors above never include secrets or provider bodies.
        tracing::error!("{error}");
        std::process::exit(1);
    }
}
