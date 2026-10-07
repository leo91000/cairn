// The upstream migration macro expands through ::sqlx. Keep the Postgres-only
// crates: the full facade also resolves SQLite and conflicts with rusqlite.
extern crate sqlx_core as sqlx;

mod account;
mod audit;
mod fcm;
mod installations;
pub mod installer;
mod mcp;
mod methods;
mod network;
mod notifications;
mod oauth;
mod passkeys;
mod relay;
mod sharing;

pub use fcm::{AccountPushSender, FcmPushSender};
pub use network::TrustedProxies;
pub use notifications::{PushError, PushSender, PushSubscription, WebPushSender};
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
    push: Option<Arc<dyn PushSender>>,
    push_receipts: Arc<notifications::DeliveryReceipts>,
    push_pool: PgPool,
}

enum ApiError {
    Http(StatusCode, &'static str),
    Database(sqlx_core::error::Error),
}

impl ApiError {
    fn is_deadlock(&self) -> bool {
        match self {
            Self::Database(error) => error
                .as_database_error()
                .is_some_and(|database| database.code().as_deref() == Some("40P01")),
            Self::Http(..) => false,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Http(status, message) => (status, message),
            Self::Database(_) => {
                // Keep database details and bound parameters out of responses/logs.
                tracing::error!("Official database operation failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "Service unavailable")
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<sqlx_core::error::Error> for ApiError {
    fn from(error: sqlx_core::error::Error) -> Self {
        Self::Database(error)
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
    router_with_network_and_push(pool, sender, origin, oauth, relay, trusted_proxies, None).await
}

pub async fn router_with_push(
    pool: PgPool,
    sender: Arc<dyn EmailSender>,
    origin: String,
    oauth: OAuthProviders,
    relay: Relay,
    push: Option<Arc<dyn PushSender>>,
) -> Result<Router, sqlx_core::migrate::MigrateError> {
    router_with_network_and_push(
        pool,
        sender,
        origin,
        oauth,
        relay,
        TrustedProxies::default(),
        push,
    )
    .await
}

pub async fn router_with_network_and_push(
    pool: PgPool,
    sender: Arc<dyn EmailSender>,
    origin: String,
    oauth: OAuthProviders,
    relay: Relay,
    trusted_proxies: TrustedProxies,
    push: Option<Arc<dyn PushSender>>,
) -> Result<Router, sqlx_core::migrate::MigrateError> {
    sqlx_macros::migrate!("./migrations").run(&pool).await?;

    // Provider waits retain sharing locks but never borrow the account/API pool.
    let push_pool = sqlx_postgres::PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(std::time::Duration::from_secs(1))
        .connect_lazy_with((*pool.connect_options()).clone());
    let installer = installer::router(origin.clone());
    let service = Service {
        pool,
        sender,
        origin,
        oauth,
        relay,
        push,
        push_receipts: Arc::default(),
        push_pool,
    };
    Ok(Router::new()
        .route("/.well-known/assetlinks.json", get(passkeys::assetlinks))
        .route("/api/account/email-code", post(request_code))
        .route("/api/account/verify", post(verify_code))
        .route("/api/account/reauth/email", post(reauthenticate_email))
        .route("/api/account/session", get(session))
        .route("/api/account/logout", post(logout))
        .route("/api/account/sessions", get(account::sessions))
        .route("/api/account/delete", post(account::delete))
        .route("/api/account/audit", get(audit::list))
        .route(
            "/api/account/sessions/revoke-others",
            post(account::revoke_others),
        )
        .route(
            "/api/account/sessions/{session}",
            axum::routing::delete(account::revoke_session),
        )
        .route(
            "/api/account/notifications",
            get(notifications::configuration),
        )
        .route(
            "/api/account/notifications/android",
            post(notifications::subscribe_android).get(notifications::android_configuration),
        )
        .route(
            "/api/account/notifications/subscriptions",
            post(notifications::subscribe),
        )
        .route(
            "/api/account/notifications/subscriptions/{id}",
            get(notifications::registered).delete(notifications::unsubscribe),
        )
        .route(
            "/api/account/oauth/{name}/native/browser",
            get(oauth::native_browser),
        )
        .route(
            "/api/account/oauth/{name}/native/finish",
            post(oauth::native_finish),
        )
        .route(
            "/api/account/oauth/{name}/native/confirm",
            post(oauth::native_confirm),
        )
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
        .route(
            "/api/account/passkeys/reauth/start",
            post(passkeys::reauth_start),
        )
        .route(
            "/api/account/passkeys/reauth/finish",
            post(passkeys::reauth_finish),
        )
        .route("/api/account/options", get(oauth::options))
        .route("/api/account/oauth/{provider}/start", post(oauth::start))
        .route(
            "/api/account/oauth/{provider}/callback",
            get(oauth::callback).post(oauth::google_credential),
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
            axum::routing::patch(installations::rename).delete(installations::forget),
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
                    "/api/relay/{installation}/rotate-token",
                    post(installations::rotate_token),
                )
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

const EMAIL_CHALLENGE_ATTEMPT_LIMIT: i32 = 5;
const EMAIL_CODE_ATTEMPT_LIMIT: i32 = 50;

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
        return Err(ApiError::Http(
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

    let challenge = random_token();
    let code = format!("{:08}", rand::rng().random_range(0..100_000_000_u32));
    let code_digest = digest(&format!("{challenge}:{code}"));

    let mut transaction = service.pool.begin().await?;
    // Serialize delivery for an address across all official processes. A later
    // request must not invalidate the proof already in the recipient's mailbox.
    lock_email(&mut transaction, &email).await?;
    let pending: Option<(String,)> = query_as(
        "SELECT challenge FROM email_codes WHERE email = $1 AND expires_at > clock_timestamp() AND attempts < $2 LIMIT 1 FOR UPDATE",
    )
    .bind(&email)
    .bind(EMAIL_CODE_ATTEMPT_LIMIT)
    .fetch_optional(&mut *transaction)
    .await?;
    if let Some((code_challenge,)) = pending {
        // Give the mailbox owner a usable proof even if a third party requested
        // the code first. Each challenge gets its own attempts and keeps the
        // original deadline.
        query("INSERT INTO email_code_challenges (challenge, code_challenge) VALUES ($1, $2)")
            .bind(&challenge)
            .bind(&code_challenge)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({ "challenge": challenge })),
        )
            .into_response());
    }

    consume_limit_on(
        &mut transaction,
        &format!("email:{}", digest(&email)),
        1,
        60,
    )
    .await?;
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
    query("INSERT INTO email_code_challenges (challenge, code_challenge) VALUES ($1, $1)")
        .bind(&challenge)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    if service.sender.send_code(&email, &code).await.is_err() {
        query("DELETE FROM email_codes WHERE challenge = $1")
            .bind(&challenge)
            .execute(&service.pool)
            .await?;
        return Err(ApiError::Http(
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

/// Serialize proof delivery, verification and deletion before locking codes or
/// accounts. The address is normalized by delivery; verification reads it from
/// the stored challenge, then re-reads the proof after acquiring this lock.
async fn lock_email(
    connection: &mut sqlx_postgres::PgConnection,
    email: &str,
) -> Result<(), ApiError> {
    query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(email)
        .execute(connection)
        .await?;
    Ok(())
}

async fn verify_code(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<Verification>,
) -> Result<Response, ApiError> {
    verify_email(&service, peer, &headers, input, ProofPurpose::SignIn).await
}

async fn reauthenticate_email(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<Verification>,
) -> Result<Response, ApiError> {
    methods::authenticated(&service, &headers, true).await?;
    verify_email(
        &service,
        peer,
        &headers,
        input,
        ProofPurpose::ConfirmSession,
    )
    .await
}

async fn verify_email(
    service: &Service,
    peer: SocketAddr,
    headers: &HeaderMap,
    input: Verification,
    purpose: ProofPurpose,
) -> Result<Response, ApiError> {
    consume_limit(&service.pool, &format!("verification:{}", peer.ip()), 30).await?;

    let invalid = || ApiError::Http(StatusCode::UNAUTHORIZED, "Invalid or expired code");
    let mut transaction = service.pool.begin().await?;
    let address: Option<(String,)> = query_as("SELECT e.email FROM email_codes e JOIN email_code_challenges c ON c.code_challenge = e.challenge WHERE c.challenge = $1")
        .bind(&input.challenge).fetch_optional(&mut *transaction).await?;
    let Some((address,)) = address else {
        return Err(invalid());
    };
    lock_email(&mut transaction, &address).await?;

    let row: Option<(String, String, String, bool, i32, i32)> = query_as("SELECT e.challenge, e.email, e.code_digest, e.expires_at > clock_timestamp(), e.attempts, c.attempts FROM email_codes e JOIN email_code_challenges c ON c.code_challenge = e.challenge WHERE c.challenge = $1 FOR UPDATE OF e, c")
        .bind(&input.challenge).fetch_optional(&mut *transaction).await?;

    let Some((code_challenge, email, expected, unexpired, code_attempts, challenge_attempts)) = row
    else {
        return Err(invalid());
    };

    if !unexpired
        || challenge_attempts >= EMAIL_CHALLENGE_ATTEMPT_LIMIT
        || code_attempts >= EMAIL_CODE_ATTEMPT_LIMIT
    {
        return Err(invalid());
    }

    let supplied = digest(&format!("{code_challenge}:{}", input.code));
    if !bool::from(expected.as_bytes().ct_eq(supplied.as_bytes())) {
        // Locked code and challenge rows bound guesses across concurrent clients.
        // An already exhausted challenge never burns someone else's budget.
        query("UPDATE email_code_challenges SET attempts = attempts + 1 WHERE challenge = $1")
            .bind(&input.challenge)
            .execute(&mut *transaction)
            .await?;
        query("UPDATE email_codes SET attempts = attempts + 1 WHERE challenge = $1")
            .bind(&code_challenge)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        return Err(invalid());
    }

    if purpose == ProofPurpose::ConfirmSession {
        let (account, current_email) =
            methods::authenticated_on(&mut transaction, headers, true).await?;
        if current_email != email {
            return Err(invalid());
        }
        query("SELECT id FROM leo_accounts WHERE id = $1 FOR UPDATE")
            .bind(&account)
            .execute(&mut *transaction)
            .await?;
        account::confirm_identity(&mut transaction, headers).await?;
        query("DELETE FROM email_codes WHERE challenge = $1")
            .bind(&code_challenge)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    query("DELETE FROM email_codes WHERE challenge = $1")
        .bind(&code_challenge)
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
        let linked = methods::authenticated_on(&mut transaction, headers, true).await?;
        if linked.0 != account_id {
            return Err(ApiError::Http(
                StatusCode::UNAUTHORIZED,
                "Sign in with another method to re-enable email",
            ));
        }
    }

    query("INSERT INTO sign_in_methods (id, account_id, kind, subject, label) VALUES ($1, $2, 'email', $3, $3) ON CONFLICT (kind, subject) DO UPDATE SET removed = false")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&account_id).bind(&email).execute(&mut *transaction).await?;

    let response = create_session(
        service,
        &mut transaction,
        &account_id,
        &email,
        headers,
        SessionProof::Email,
    )
    .await?;
    transaction.commit().await?;
    Ok(response)
}

/// Whether a valid independent proof creates a session or confirms the caller.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProofPurpose {
    SignIn,
    ConfirmSession,
}

enum SessionProof {
    Email,
    Passkey,
    OAuth,
}

async fn create_session(
    service: &Service,
    connection: &mut sqlx_postgres::PgConnection,
    account_id: &str,
    email: &str,
    headers: &HeaderMap,
    proof: SessionProof,
) -> Result<Response, ApiError> {
    let token = random_token();
    let csrf = random_token();
    let device: String = headers
        .get(header::USER_AGENT)
        .and_then(|value| std::str::from_utf8(value.as_bytes()).ok())
        .unwrap_or("Unknown device")
        .chars()
        .filter(|character| {
            let bidi_control = matches!(character,
                '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
            !character.is_control() && !bidi_control
        })
        .take(256)
        .collect();
    let recent_proof = matches!(proof, SessionProof::Email | SessionProof::Passkey);
    query("INSERT INTO web_sessions (digest, account_id, csrf, expires_at, device, last_proof_at) VALUES ($1, $2, $3, now() + interval '7 days', $4, CASE WHEN $5 THEN clock_timestamp() END)")
        .bind(digest(&token)).bind(account_id).bind(&csrf).bind(device).bind(recent_proof).execute(&mut *connection).await?;

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
        ApiError::Http(StatusCode::FORBIDDEN, "Invalid origin").into_response()
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
        return Err(ApiError::Http(
            StatusCode::UNAUTHORIZED,
            "Session expired. Please sign in again.",
        ));
    };

    if !bool::from(csrf.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(ApiError::Http(StatusCode::FORBIDDEN, "Invalid CSRF token"));
    }

    query("DELETE FROM web_sessions WHERE digest = $1")
        .bind(digest(token))
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    service.relay.revoke_session(&digest(token));

    Ok(clear_session_cookie(&service))
}

fn clear_session_cookie(service: &Service) -> Response {
    let secure = if service.origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            format!("leo_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}"),
        )],
    )
        .into_response()
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
        return Err(ApiError::Http(
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
        "DELETE FROM account_audit WHERE created_at <= now() - interval '90 days'",
        "DELETE FROM email_codes WHERE expires_at <= now()",
        "DELETE FROM web_sessions WHERE expires_at <= now()",
        "DELETE FROM account_rate_limits WHERE resets_at < now() - interval '1 day'",
        "DELETE FROM sign_in_challenges WHERE expires_at <= now()",
        "DELETE FROM native_oauth_handovers WHERE expires_at <= now()",
        "DELETE FROM installation_claim_codes WHERE expires_at <= now()",
        "DELETE FROM installation_device_claims WHERE expires_at <= now()",
        "DELETE FROM installation_invitations WHERE expires_at <= now()",
    ] {
        query(statement).execute(&mut *transaction).await?;
    }
    transaction.commit().await
}
