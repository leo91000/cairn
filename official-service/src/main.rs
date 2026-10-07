use std::{env, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    routing::{any, get},
};
use leo_official_service::{
    AccountPushSender, EmailSender, FcmPushSender, OAuthProvider, OAuthProviders, Relay,
    TrustedProxies, WebPushSender, router_with_network_and_push,
};
use reqwest::Client;
use serde_json::json;
use sqlx_postgres::PgPoolOptions;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
};
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
        self.deliver(message).await
    }

    async fn send_invitation(
        &self,
        email: &str,
        installation: &str,
        url: &str,
    ) -> Result<(), String> {
        self.deliver(json!({
            "from": self.from,
            "to": [email],
            "subject": "Invitation to a Leo installation",
            "text": format!("You have been invited to the Leo installation \"{installation}\". Sign in or create your Leo account with this email address to accept: {url}\nMembers use the owner's coding-agent accounts and secrets. This invitation expires in 7 days."),
        })).await
    }
}

impl HttpEmailSender {
    async fn deliver(&self, message: serde_json::Value) -> Result<(), String> {
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

fn configured_oauth(name: &str, loopback: bool) -> Result<Option<OAuthProvider>, String> {
    let prefix = format!("LEO_OFFICIAL_{name}");
    let client_id = env::var(format!("{prefix}_CLIENT_ID"))
        .ok()
        .filter(|value| !value.trim().is_empty());
    let client_secret = env::var(format!("{prefix}_CLIENT_SECRET"))
        .ok()
        .filter(|value| !value.trim().is_empty());
    let (client_id, client_secret) = match (client_id, client_secret) {
        (None, None) => return Ok(None),
        (Some(id), Some(secret)) => (id, secret),
        _ => {
            return Err(format!(
                "Set both {prefix}_CLIENT_ID and {prefix}_CLIENT_SECRET"
            ));
        }
    };

    let endpoint = |suffix: &str, default: &str| -> Result<String, String> {
        let key = format!("{prefix}_{suffix}");
        let value = env::var(&key).unwrap_or_else(|_| default.into());
        let url = Url::parse(&value).map_err(|_| format!("Invalid {key}"))?;
        let local_endpoint = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(url.scheme() == "https" || loopback && local_endpoint && url.scheme() == "http")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(format!(
                "{key} requires HTTPS (HTTP endpoints are only allowed on loopback in development)"
            ));
        }

        Ok(value)
    };

    let (authorization, token, userinfo, emails) = match name {
        "GOOGLE" => (
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            "https://openidconnect.googleapis.com/v1/userinfo",
            None,
        ),
        "GITHUB" => (
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
            "https://api.github.com/user",
            Some("https://api.github.com/user/emails"),
        ),
        _ => return Err("Unknown OAuth provider".into()),
    };

    Ok(Some(OAuthProvider {
        client_id,
        client_secret,
        authorization_url: endpoint("AUTHORIZATION_URL", authorization)?,
        token_url: endpoint("TOKEN_URL", token)?,
        userinfo_url: endpoint("USERINFO_URL", userinfo)?,
        emails_url: emails
            .map(|value| endpoint("EMAILS_URL", value))
            .transpose()?,
    }))
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

    let oauth = OAuthProviders {
        google: configured_oauth("GOOGLE", loopback)?,
        github: configured_oauth("GITHUB", loopback)?,
        android_certificates: env::var("LEO_OFFICIAL_ANDROID_CERTIFICATES")
            .unwrap_or_default()
            .split(',')
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                let decoded = hex::decode(value.trim().replace(':', ""))
                    .map_err(|_| "Invalid Android signing certificate fingerprint")?;
                <[u8; 32]>::try_from(decoded)
                    .map_err(|_| "Android signing fingerprints require SHA-256")
            })
            .collect::<Result<Vec<_>, _>>()?,
        ..Default::default()
    };
    let proxies: TrustedProxies = env::var("LEO_OFFICIAL_TRUSTED_PROXIES")
        .unwrap_or_default()
        .parse()
        .map_err(str::to_owned)?;
    let web_push = match (
        env::var("LEO_OFFICIAL_VAPID_PRIVATE_KEY")
            .ok()
            .filter(|value| !value.is_empty()),
        env::var("LEO_OFFICIAL_VAPID_SUBJECT")
            .ok()
            .filter(|value| !value.is_empty()),
    ) {
        (None, None) => None,
        (Some(private), Some(subject)) => Some(WebPushSender::new(private, subject)?),
        _ => {
            return Err(
                "Set both LEO_OFFICIAL_VAPID_PRIVATE_KEY and LEO_OFFICIAL_VAPID_SUBJECT".into(),
            );
        }
    };
    let android_push = env::var("LEO_OFFICIAL_FCM_SERVICE_ACCOUNT")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|path| {
            let account = std::fs::read_to_string(path)
                .map_err(|_| "Could not read FCM service account file")?;
            FcmPushSender::new(&account)
        })
        .transpose()?;
    let push: Option<Arc<dyn leo_official_service::PushSender>> =
        if web_push.is_some() || android_push.is_some() {
            Some(Arc::new(AccountPushSender {
                web: web_push,
                android: android_push,
            }))
        } else {
            None
        };
    let relay = Relay::default();
    let health_pool = pool.clone();
    let commit = env::var("APP_COMMIT").unwrap_or_else(|_| "development".into());
    let runtime_id = env::var("APP_RUNTIME_ID").unwrap_or_else(|_| commit.clone());
    let mut app = router_with_network_and_push(
        pool.clone(),
        Arc::new(sender),
        origin,
        oauth,
        relay.clone(),
        proxies,
        push,
    )
    .await
    .map_err(|_| "Official database migration failed")?
    .merge(leo_official_service::installer::release_router(
        env::var("LEO_INSTALLATION_IMAGE").ok(),
    )?)
    .route(
        "/health",
        get(move || {
            let pool = health_pool.clone();
            let commit = commit.clone();
            let runtime_id = runtime_id.clone();
            async move {
                let ready = sqlx_core::query::query("SELECT 1")
                    .execute(&pool)
                    .await
                    .is_ok();
                let status = if ready {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                };
                (
                    status,
                    [(header::CACHE_CONTROL, "no-store")],
                    Json(json!({
                        "status": if ready { "ok" } else { "unavailable" },
                        "commit": commit,
                        "runtimeId": runtime_id,
                    })),
                )
            }
        }),
    )
    .route("/api/{*path}", any(|| async { StatusCode::NOT_FOUND }))
    .route_service("/", ServeFile::new(web.join("official.html")))
    .route_service("/index.html", ServeFile::new(web.join("official.html")))
    .fallback_service(ServeDir::new(&web).fallback(ServeFile::new(web.join("official.html"))))
    .layer(SetResponseHeaderLayer::if_not_present(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    ))
    .layer(SetResponseHeaderLayer::if_not_present(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static("DENY"),
    ));

    // TLS terminates at the configured official origin's trusted proxy.
    if origin_url.scheme() == "https" {
        app = app.layer(SetResponseHeaderLayer::overriding(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        ));
    }

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|_| "Could not bind LEO_OFFICIAL_LISTEN")?;

    let maintenance = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if leo_official_service::cleanup_expired(&pool).await.is_err() {
                tracing::warn!("Official expiration cleanup failed; will retry next hour");
            }
        }
    });

    tracing::info!("Official service listening");
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Could not register shutdown signal");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        relay.shutdown();
    })
    .await
    .map_err(|_| "Official HTTP server stopped unexpectedly".to_owned());
    maintenance.abort();
    result
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
