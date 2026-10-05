// The upstream migration macro expands through ::sqlx. Keep the Postgres-only
// crates: the full facade also resolves SQLite and conflicts with rusqlite.
extern crate sqlx_core as sqlx;

mod installations;
pub mod installer;
mod mcp;
mod methods;
mod network;
mod oauth;
mod passkeys;
mod relay;
mod sharing;

pub use network::TrustedProxies;
pub use oauth::{OAuthProvider, OAuthProviders};
pub use relay::Relay;

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::Request,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use rand::{Rng, RngCore};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx_core::{query::query, query_as::query_as};
use sqlx_postgres::PgPool;
use std::{net::SocketAddr, sync::Arc};
use subtle::ConstantTimeEq;

#[async_trait]
pub trait EmailSender: Send + Sync {
    /// Deliver the code without retaining or logging it.
    async fn send_code(&self, email: &str, code: &str) -> Result<(), String>;

    async fn send_invitation(
        &self,
        email: &str,
        installation: &str,
        url: &str,
    ) -> Result<(), String>;
}

#[derive(Clone)]
struct Service {
    pool: PgPool,
    sender: Arc<dyn EmailSender>,
    origin: String,
    oauth: OAuthProviders,
    relay: relay::Relay,
}

struct ApiError(StatusCode, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<sqlx_core::error::Error> for ApiError {
    fn from(_: sqlx_core::error::Error) -> Self {
        tracing::error!("Official database operation failed");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "Service unavailable")
    }
}

pub async fn router(
    pool: PgPool,
    sender: Arc<dyn EmailSender>,
    origin: String,
) -> Result<Router, sqlx_core::migrate::MigrateError> {
    router_with_oauth(pool, sender, origin, OAuthProviders::default()).await
}

pub async fn router_with_oauth(
    pool: PgPool,
    sender: Arc<dyn EmailSender>,
    origin: String,
    oauth: OAuthProviders,
) -> Result<Router, sqlx_core::migrate::MigrateError> {
    router_with_relay(pool, sender, origin, oauth, Relay::default()).await
}

/// Supply the relay handle used by installation-access changes to revoke live readers.
pub async fn router_with_relay(
    pool: PgPool,
    sender: Arc<dyn EmailSender>,
    origin: String,
    oauth: OAuthProviders,
    relay: Relay,
) -> Result<Router, sqlx_core::migrate::MigrateError> {
    router_with_network(
        pool,
        sender,
        origin,
        oauth,
        relay,
        TrustedProxies::default(),
    )
    .await
}

/// Configure proxy trust explicitly; library/test callers default to no trusted peers.
pub async fn router_with_network(
    pool: PgPool,
    sender: Arc<dyn EmailSender>,
    origin: String,
    oauth: OAuthProviders,
    relay: Relay,
    trusted_proxies: TrustedProxies,
) -> Result<Router, sqlx_core::migrate::MigrateError> {
    sqlx_macros::migrate!("./migrations").run(&pool).await?;

    let installer = installer::router(origin.clone());
    let service = Service {
        pool,
        sender,
        origin,
        oauth,
        relay,
    };
    Ok(Router::new()
        .route("/api/account/email-code", post(request_code))
        .route("/api/account/verify", post(verify_code))
        .route("/api/account/session", get(session))
        .route("/api/account/logout", post(logout))
        .route(
            "/api/account/passkeys/register/start",
            post(passkeys::register_start),
        )
        .route(
            "/api/account/passkeys/register/finish",
            post(passkeys::register_finish),
        )
        .route(
            "/api/account/passkeys/login/start",
            post(passkeys::login_start),
        )
        .route(
            "/api/account/passkeys/login/finish",
            post(passkeys::login_finish),
        )
        .route("/api/account/options", get(oauth::options))
        .route("/api/account/oauth/{provider}/start", post(oauth::start))
        .route(
            "/api/account/oauth/{provider}/callback",
            get(oauth::callback),
        )
        .route("/api/account/invitations", get(sharing::pending))
        .route(
            "/api/account/invitations/{invitation}/accept",
            post(sharing::accept),
        )
        .route(
            "/api/installations/{installation}/sharing",
            get(sharing::list),
        )
        .route(
            "/api/installations/{installation}/sharing/invitations",
            post(sharing::invite),
        )
        .route(
            "/api/installations/{installation}/sharing/invitations/{invitation}",
            axum::routing::delete(sharing::cancel),
        )
        .route(
            "/api/installations/{installation}/sharing/members/{member}",
            axum::routing::delete(sharing::remove),
        )
        .route(
            "/api/installations/{installation}/sharing/membership",
            axum::routing::delete(sharing::leave),
        )
        .route("/api/account/methods", get(methods::list))
        .route("/api/account/methods/remove", post(methods::remove))
        .route("/api/installations", get(installations::status))
        .route(
            "/api/installations/claim-code",
            post(installations::claim_code),
        )
        .route(
            "/api/installations/{installation}",
            axum::routing::patch(installations::rename),
        )
        .route(
            "/api/installations/device-claim/preview",
            post(installations::preview_device),
        )
        .route(
            "/api/installations/device-claim",
            post(installations::approve_device),
        )
        .route(
            "/api/installations/{installation}/detach",
            post(installations::detach),
        )
        .route(
            "/api/installations/{installation}/api/{*path}",
            any(relay::forward),
        )
        .route("/api/mcp/oauth/preview", post(mcp::preview))
        .route("/api/mcp/oauth/consent", post(mcp::consent))
        .route(
            "/api/installations/{installation}/tokens",
            get(mcp::list).post(mcp::personal),
        )
        .route(
            "/api/installations/{installation}/tokens/{grant}",
            axum::routing::delete(mcp::revoke),
        )
        .layer(middleware::from_fn_with_state(
            service.clone(),
            browser_security,
        ))
        .merge(
            Router::new()
                .merge(
                    Router::new()
                        .route("/mcp", any(mcp::handle))
                        .route(
                            "/api/public/installations/{installation}/artifacts/{token}",
                            get(relay::public_artifact),
                        )
                        .route("/oauth/register", post(mcp::register))
                        .route("/oauth/authorize", get(mcp::authorize))
                        .route("/oauth/token", post(mcp::exchange))
                        .route("/oauth/revoke", post(mcp::revoke_token))
                        .route(
                            "/.well-known/oauth-protected-resource",
                            get(mcp::resource_metadata),
                        )
                        .route(
                            "/.well-known/oauth-protected-resource/mcp",
                            get(mcp::resource_metadata),
                        )
                        .route(
                            "/.well-known/oauth-authorization-server",
                            get(mcp::server_metadata),
                        )
                        .layer(middleware::from_fn(mcp::public_security)),
                )
                .route("/api/relay/claim", post(installations::claim))
                .route(
                    "/api/relay/device-claim/start",
                    post(installations::start_device),
                )
                .route(
                    "/api/relay/device-claim/poll",
                    post(installations::poll_device),
                )
                .route("/api/relay/{installation}/connect", get(relay::upgrade)),
        )
        .with_state(service)
        .merge(installer)
        .layer(middleware::from_fn_with_state(
            trusted_proxies,
            network::client_peer,
        )))
}

fn random_token() -> String {
    let mut bytes = [0; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[derive(Deserialize)]
struct EmailRequest {
    email: String,
}

fn normalized_email(input: &str) -> Result<String, ApiError> {
    let email = input.trim().to_lowercase();
    if email.len() > 254
        || email.chars().any(char::is_control)
        || email_address::EmailAddress::parse_with_options(
            &email,
            email_address::Options {
                allow_display_text: false,
                ..Default::default()
            },
        )
        .is_err()
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Enter a valid email address",
        ));
    }

    Ok(email)
}

async fn request_code(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<EmailRequest>,
) -> Result<Response, ApiError> {
    consume_limit(&service.pool, &format!("delivery:{}", peer.ip()), 10).await?;

    let email = normalized_email(&input.email)?;

    consume_limit(&service.pool, &format!("email:{}", digest(&email)), 1).await?;

    let challenge = random_token();
    let code = format!("{:08}", rand::rng().random_range(0..100_000_000_u32));
    let code_digest = digest(&format!("{challenge}:{code}"));

    let mut transaction = service.pool.begin().await?;
    // Serialize delivery for an address across all official processes. A later
    // request must not invalidate the proof already in the recipient's mailbox.
    query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(&email)
        .execute(&mut *transaction)
        .await?;
    let pending: Option<(String,)> = query_as(
        "SELECT challenge FROM email_codes WHERE email = $1 AND expires_at > now() LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(&mut *transaction)
    .await?;
    if pending.is_some() {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "A code is already pending. Use it or wait for it to expire.",
        ));
    }

    consume_limit_on(
        &mut transaction,
        &format!("email-hour:{}", digest(&email)),
        6,
        3600,
    )
    .await?;
    consume_limit_on(
        &mut transaction,
        &format!("email-day:{}", digest(&email)),
        20,
        86400,
    )
    .await?;

    query("DELETE FROM email_codes WHERE email = $1")
        .bind(&email)
        .execute(&mut *transaction)
        .await?;
    query("INSERT INTO email_codes (challenge, email, code_digest, expires_at) VALUES ($1, $2, $3, now() + interval '10 minutes')")
        .bind(&challenge).bind(&email).bind(code_digest).execute(&mut *transaction).await?;
    transaction.commit().await?;

    if service.sender.send_code(&email, &code).await.is_err() {
        query("DELETE FROM email_codes WHERE challenge = $1")
            .bind(&challenge)
            .execute(&service.pool)
            .await?;
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Email delivery unavailable. Please try again later.",
        ));
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "challenge": challenge })),
    )
        .into_response())
}

#[derive(Deserialize)]
struct Verification {
    challenge: String,
    code: String,
}

async fn verify_code(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<Verification>,
) -> Result<Response, ApiError> {
    consume_limit(&service.pool, &format!("verification:{}", peer.ip()), 30).await?;

    let mut transaction = service.pool.begin().await?;
    let row: Option<(String, String, bool, i32)> = query_as("SELECT email, code_digest, expires_at > now(), attempts FROM email_codes WHERE challenge = $1 FOR UPDATE")
        .bind(&input.challenge).fetch_optional(&mut *transaction).await?;

    let invalid = || ApiError(StatusCode::UNAUTHORIZED, "Invalid or expired code");
    let Some((email, expected, unexpired, attempts)) = row else {
        return Err(invalid());
    };

    let supplied = digest(&format!("{}:{}", input.challenge, input.code));
    if !unexpired || attempts >= 5 || !bool::from(expected.as_bytes().ct_eq(supplied.as_bytes())) {
        query("UPDATE email_codes SET attempts = attempts + 1 WHERE challenge = $1")
            .bind(&input.challenge)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        return Err(invalid());
    }

    query("DELETE FROM email_codes WHERE challenge = $1")
        .bind(&input.challenge)
        .execute(&mut *transaction)
        .await?;

    let (account_id,): (String,) = query_as("INSERT INTO leo_accounts (id, email) VALUES ($1, $2) ON CONFLICT (email) DO UPDATE SET email = EXCLUDED.email RETURNING id")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&email).fetch_one(&mut *transaction).await?;

    let removed: Option<(bool,)> =
        query_as("SELECT removed FROM sign_in_methods WHERE account_id = $1 AND kind = 'email'")
            .bind(&account_id)
            .fetch_optional(&mut *transaction)
            .await?;
    if removed == Some((true,)) {
        let linked = methods::authenticated_on(&mut transaction, &headers, true).await?;
        if linked.0 != account_id {
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "Sign in with another method to re-enable email",
            ));
        }
    }

    query("INSERT INTO sign_in_methods (id, account_id, kind, subject, label) VALUES ($1, $2, 'email', $3, $3) ON CONFLICT (kind, subject) DO UPDATE SET removed = false")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&account_id).bind(&email).execute(&mut *transaction).await?;

    let response = create_session(&service, &mut transaction, &account_id, &email).await?;
    transaction.commit().await?;
    Ok(response)
}

async fn create_session(
    service: &Service,
    connection: &mut sqlx_postgres::PgConnection,
    account_id: &str,
    email: &str,
) -> Result<Response, ApiError> {
    let token = random_token();
    let csrf = random_token();
    query("INSERT INTO web_sessions (digest, account_id, csrf, expires_at) VALUES ($1, $2, $3, now() + interval '7 days')")
        .bind(digest(&token)).bind(account_id).bind(&csrf).execute(&mut *connection).await?;

    let secure = if service.origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    let cookie =
        format!("leo_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=604800{secure}");
    Ok((
        [(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap())],
        Json(json!({
            "authenticated": true,
            "account": { "id": account_id, "email": email },
            "csrf": csrf,
            "installations": installations::list(&mut *connection, account_id, &service.relay).await?,
        })),
    )
        .into_response())
}

async fn session(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let token = session_token(&headers);
    let row: Option<(String, String, String)> = query_as("SELECT a.id, a.email, s.csrf FROM web_sessions s JOIN leo_accounts a ON a.id = s.account_id WHERE s.digest = $1 AND s.expires_at > now()")
        .bind(digest(token)).fetch_optional(&service.pool).await?;
    Ok(Json(match row {
        Some((id, email, csrf)) => json!({
            "authenticated": true,
            "account": { "id": id, "email": email },
            "csrf": csrf,
            "installations": installations::list(&service.pool, &id, &service.relay).await?,
        }),
        None => json!({
            "authenticated": false,
            "account": null,
            "csrf": null,
            "installations": [],
        }),
    }))
}

async fn browser_security(
    State(service): State<Service>,
    request: Request,
    next: Next,
) -> Response {
    let allowed_origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        == Some(service.origin.as_str());
    let mut response = if request.method() != Method::GET && !allowed_origin {
        ApiError(StatusCode::FORBIDDEN, "Invalid origin").into_response()
    } else {
        next.run(request).await
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn session_token(headers: &HeaderMap) -> &str {
    cookie_token(headers, "leo_session")
}

fn cookie_token<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
        .unwrap_or("")
}

async fn logout(State(service): State<Service>, headers: HeaderMap) -> Result<Response, ApiError> {
    let token = session_token(&headers);
    let supplied = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    let mut transaction = service.pool.begin().await?;
    let row: Option<(String,)> = query_as(
        "SELECT csrf FROM web_sessions WHERE digest = $1 AND expires_at > now() FOR UPDATE",
    )
    .bind(digest(token))
    .fetch_optional(&mut *transaction)
    .await?;

    let Some((csrf,)) = row else {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Session expired. Please sign in again.",
        ));
    };

    if !bool::from(csrf.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(ApiError(StatusCode::FORBIDDEN, "Invalid CSRF token"));
    }

    query("DELETE FROM web_sessions WHERE digest = $1")
        .bind(digest(token))
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    let secure = if service.origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            format!("leo_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}"),
        )],
    )
        .into_response())
}

async fn consume_limit(pool: &PgPool, key: &str, maximum: i32) -> Result<(), ApiError> {
    let mut connection = pool.acquire().await?;
    consume_limit_on(&mut connection, key, maximum, 60).await
}

async fn consume_limit_on(
    connection: &mut sqlx_postgres::PgConnection,
    key: &str,
    maximum: i32,
    seconds: i32,
) -> Result<(), ApiError> {
    let (requests,): (i32,) = query_as("INSERT INTO account_rate_limits (key, requests, resets_at) VALUES ($1, 1, now() + $3::integer * interval '1 second') ON CONFLICT (key) DO UPDATE SET requests = CASE WHEN account_rate_limits.resets_at <= now() THEN 1 ELSE LEAST(account_rate_limits.requests + 1, $2 + 1) END, resets_at = CASE WHEN account_rate_limits.resets_at <= now() THEN now() + $3::integer * interval '1 second' ELSE account_rate_limits.resets_at END RETURNING requests")
        .bind(key).bind(maximum).bind(seconds).fetch_one(connection).await?;
    if requests > maximum {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts. Please wait for the delivery or request limit to reset.",
        ));
    }

    Ok(())
}

/// Expiration is enforced by every read; reclamation happens outside public requests.
pub async fn cleanup_expired(pool: &PgPool) -> Result<(), sqlx_core::error::Error> {
    let mut transaction = pool.begin().await?;
    for statement in [
        "DELETE FROM email_codes WHERE expires_at <= now()",
        "DELETE FROM web_sessions WHERE expires_at <= now()",
        "DELETE FROM account_rate_limits WHERE resets_at < now() - interval '1 day'",
        "DELETE FROM sign_in_challenges WHERE expires_at <= now()",
        "DELETE FROM installation_claim_codes WHERE expires_at <= now()",
        "DELETE FROM installation_device_claims WHERE expires_at <= now()",
        "DELETE FROM installation_invitations WHERE expires_at <= now()",
    ] {
        query(statement).execute(&mut *transaction).await?;
    }
    transaction.commit().await
}
