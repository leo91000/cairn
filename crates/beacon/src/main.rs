use std::{
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    routing::{any, get},
};
use cairn_beacon::{
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
            "subject": "Your Cairn sign-in code",
            "text": format!("Your Cairn sign-in code is {code}. It expires in 10 minutes. If you did not request it, ignore this email."),
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
            "subject": "Invitation to a Cairn installation",
            "text": format!("You have been invited to the Cairn installation \"{installation}\". Sign in or create your Cairn account with this email address to accept: {url}\nMembers use the owner's coding-agent accounts and secrets. This invitation expires in 7 days."),
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
    let prefix = format!("CAIRN_BEACON_{name}");
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
    let configured_origin = required("CAIRN_BEACON_ORIGIN")?;
    let origin_url = Url::parse(&configured_origin).map_err(|_| "Invalid CAIRN_BEACON_ORIGIN")?;
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
            "CAIRN_BEACON_ORIGIN must be an HTTPS origin (HTTP is allowed only on loopback)".into(),
        );
    }
    let origin = origin_url.origin().ascii_serialization();

    let endpoint = env::var("CAIRN_BEACON_EMAIL_ENDPOINT")
        .unwrap_or_else(|_| "https://api.resend.com/emails".into());
    let email_url = Url::parse(&endpoint).map_err(|_| "Invalid CAIRN_BEACON_EMAIL_ENDPOINT")?;
    if email_url.scheme() != "https" && !(loopback && email_url.scheme() == "http") {
        return Err("CAIRN_BEACON_EMAIL_ENDPOINT requires HTTPS outside development".into());
    }

    let sender = HttpEmailSender {
        client: Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Could not configure email delivery")?,
        endpoint,
        key: required("CAIRN_BEACON_EMAIL_KEY")?,
        from: required("CAIRN_BEACON_EMAIL_FROM")?,
    };

    let address: SocketAddr = env::var("CAIRN_BEACON_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:4311".into())
        .parse()
        .map_err(|_| "Invalid CAIRN_BEACON_LISTEN")?;

    let web = PathBuf::from(env::var("CAIRN_BEACON_WEB_DIR").unwrap_or_else(|_| "dist".into()));
    if !web.join("official.html").is_file() {
        return Err(
            "Build the web application with pnpm build before starting the official service".into(),
        );
    }

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&required("CAIRN_BEACON_DATABASE_URL")?)
        .await
        .map_err(|_| "Could not connect to the official Postgres database")?;

    let oauth = OAuthProviders {
        google: configured_oauth("GOOGLE", loopback)?,
        github: configured_oauth("GITHUB", loopback)?,
        android_certificates: env::var("CAIRN_BEACON_ANDROID_CERTIFICATES")
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
    let proxies: TrustedProxies = env::var("CAIRN_BEACON_TRUSTED_PROXIES")
        .unwrap_or_default()
        .parse()
        .map_err(str::to_owned)?;
    let web_push = match (
        env::var("CAIRN_BEACON_VAPID_PRIVATE_KEY")
            .ok()
            .filter(|value| !value.is_empty()),
        env::var("CAIRN_BEACON_VAPID_SUBJECT")
            .ok()
            .filter(|value| !value.is_empty()),
    ) {
        (None, None) => None,
        (Some(private), Some(subject)) => Some(WebPushSender::new(private, subject)?),
        _ => {
            return Err(
                "Set both CAIRN_BEACON_VAPID_PRIVATE_KEY and CAIRN_BEACON_VAPID_SUBJECT".into(),
            );
        }
    };
    let fcm_json = env::var("CAIRN_BEACON_FCM_SERVICE_ACCOUNT_JSON")
        .ok()
        .filter(|value| !value.is_empty());
    let fcm_file = env::var("CAIRN_BEACON_FCM_SERVICE_ACCOUNT")
        .ok()
        .filter(|value| !value.is_empty());
    let android_push = match (fcm_json, fcm_file) {
        (None, None) => None,
        (Some(account), None) => Some(FcmPushSender::new(&account)?),
        (None, Some(path)) => {
            let account = std::fs::read_to_string(path)
                .map_err(|_| "Could not read FCM service account file")?;
            Some(FcmPushSender::new(&account)?)
        }
        (Some(_), Some(_)) => {
            return Err("Configure only one FCM service account source".into());
        }
    };
    let push: Option<Arc<dyn cairn_beacon::PushSender>> =
        if web_push.is_some() || android_push.is_some() {
            Some(Arc::new(AccountPushSender {
                web: web_push,
                android: android_push,
            }))
        } else {
            None
        };
    let relay = Relay::default();
    relay.set_stun_url(
        env::var("CAIRN_BEACON_STUN_URL")
            .unwrap_or(cairn_beacon::stun::url(&origin)?.to_owned()),
    )?;
    let stun_address = env::var("CAIRN_BEACON_STUN_LISTEN").unwrap_or_else(|_| {
        if loopback {
            "127.0.0.1:0".into()
        } else {
            "0.0.0.0:3478".into()
        }
    });
    let stun_socket = tokio::net::UdpSocket::bind(&stun_address)
        .await
        .map_err(|_| "Could not bind official STUN listener")?;
    let stun_status = cairn_beacon::stun::Status::default();
    let stun = tokio::spawn(cairn_beacon::stun::serve_with_status(
        stun_socket,
        stun_status.clone(),
    ));
    let health_pool = pool.clone();
    let readiness = Arc::new(AtomicBool::new(false));
    let health_readiness = readiness.clone();
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
    .merge(cairn_beacon::installer::release_router(
        env::var("CAIRN_INSTALLATION_IMAGE").ok(),
    )?)
    .route(
        "/health",
        get(move || {
            let ready = health_readiness.load(Ordering::Relaxed);
            let stun = stun_status.snapshot();
            let commit = commit.clone();
            let runtime_id = runtime_id.clone();
            async move {
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
                        "stun": stun,
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
        .map_err(|_| "Could not bind CAIRN_BEACON_LISTEN")?;

    // Public probes read this cache, never borrow a database connection.
    // Start unavailable and bound sampling even if the API pool is saturated.
    let readiness_probe = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            let sample = tokio::time::timeout(
                Duration::from_secs(1),
                sqlx_core::query::query("SELECT 1").execute(&health_pool),
            )
            .await;
            let ready = matches!(sample, Ok(Ok(_)));
            readiness.store(ready, Ordering::Relaxed);
        }
    });

    let maintenance = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if cairn_beacon::cleanup_expired(&pool).await.is_err() {
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
    readiness_probe.abort();
    maintenance.abort();
    stun.abort();
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
