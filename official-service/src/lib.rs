mod installations;
mod methods;
mod oauth;
mod passkeys;

pub use oauth::{OAuthProvider, OAuthProviders};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::Request,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rand::{Rng, RngCore};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx_core::{
    migrate::{Migration, MigrationType, Migrator},
    query::query,
    query_as::query_as,
};
use sqlx_postgres::PgPool;
use std::{borrow::Cow, net::SocketAddr, sync::Arc};
use subtle::ConstantTimeEq;

#[async_trait]
pub trait EmailSender: Send + Sync {
    /// Deliver the code without retaining or logging it.
    async fn send_code(&self, email: &str, code: &str) -> Result<(), String>;
}

#[derive(Clone)]
struct Service {
    pool: PgPool,
    sender: Arc<dyn EmailSender>,
    origin: String,
    oauth: OAuthProviders,
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
    let migrations = Migrator {
        migrations: Cow::Owned(vec![
            Migration::new(
                1,
                "leo accounts".into(),
                MigrationType::Simple,
                include_str!("../migrations/0001_leo_accounts.sql").into(),
                false,
            ),
            Migration::new(
                2,
                "account rate limits".into(),
                MigrationType::Simple,
                include_str!("../migrations/0002_account_rate_limits.sql").into(),
                false,
            ),
            Migration::new(
                3,
                "installation claims".into(),
                MigrationType::Simple,
                include_str!("../migrations/0003_installations.sql").into(),
                false,
            ),
            Migration::new(
                202610030052,
                "sign in methods".into(),
                MigrationType::Simple,
                include_str!("../migrations/202610030052_sign_in_methods.sql").into(),
                false,
            ),
            Migration::new(
                202610030152,
                "removed sign in methods".into(),
                MigrationType::Simple,
                include_str!("../migrations/202610030152_removed_methods.sql").into(),

                false,
            ),
        ]),
        ..Migrator::DEFAULT
    };
    migrations.run(&pool).await?;

    let service = Service {
        pool,
        sender,
        origin,
        oauth,
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
        .route("/api/account/methods", get(methods::list))
        .route("/api/account/methods/remove", post(methods::remove))
        .route(
            "/api/installations/claim-code",
            post(installations::claim_code),
        )

        .layer(middleware::from_fn_with_state(
            service.clone(),
            browser_security,
        ))
        .merge(Router::new().route("/api/relay/claim", post(installations::claim)))
        .with_state(service))
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

    query("DELETE FROM email_codes WHERE expires_at <= now()")
        .execute(&service.pool)
        .await?;
    query("DELETE FROM web_sessions WHERE expires_at <= now()")
        .execute(&service.pool)
        .await?;
    query("DELETE FROM account_rate_limits WHERE resets_at < now() - interval '1 day'")
        .execute(&service.pool)
        .await?;

    let challenge = random_token();
    let code = format!("{:08}", rand::rng().random_range(0..100_000_000_u32));
    let code_digest = digest(&format!("{challenge}:{code}"));

    let mut transaction = service.pool.begin().await?;
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
        .bind(digest(&token)).bind(account_id).bind(&csrf).execute(connection).await?;

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
            "installations": [],
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
            "installations": installations::list(&service.pool, &id).await?,
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
    let (requests,): (i32,) = query_as("INSERT INTO account_rate_limits (key, requests, resets_at) VALUES ($1, 1, now() + interval '1 minute') ON CONFLICT (key) DO UPDATE SET requests = CASE WHEN account_rate_limits.resets_at <= now() THEN 1 ELSE LEAST(account_rate_limits.requests + 1, $2 + 1) END, resets_at = CASE WHEN account_rate_limits.resets_at <= now() THEN now() + interval '1 minute' ELSE account_rate_limits.resets_at END RETURNING requests")
        .bind(key).bind(maximum).fetch_one(pool).await?;
    if requests > maximum {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts. Please wait a minute.",
        ));
    }

    Ok(())
}
