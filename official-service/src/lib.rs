use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
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
use std::{borrow::Cow, sync::Arc};
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
    let migrations = Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            1,
            "leo accounts".into(),
            MigrationType::Simple,
            include_str!("../migrations/0001_leo_accounts.sql").into(),
            false,
        )]),
        ..Migrator::DEFAULT
    };
    migrations.run(&pool).await?;

    let service = Service {
        pool,
        sender,
        origin,
    };
    Ok(Router::new()
        .route("/api/account/email-code", post(request_code))
        .route("/api/account/verify", post(verify_code))
        .route("/api/account/session", get(session))
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

async fn request_code(
    State(service): State<Service>,
    Json(input): Json<EmailRequest>,
) -> Result<Response, ApiError> {
    let email = input.email.trim().to_lowercase();
    let challenge = random_token();
    let code = format!("{:08}", rand::rng().random_range(0..100_000_000_u32));
    let code_digest = digest(&format!("{challenge}:{code}"));
    query("INSERT INTO email_codes (challenge, email, code_digest, expires_at) VALUES ($1, $2, $3, now() + interval '10 minutes')")
        .bind(&challenge).bind(&email).bind(code_digest).execute(&service.pool).await?;

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
    Json(input): Json<Verification>,
) -> Result<Response, ApiError> {
    let mut transaction = service.pool.begin().await?;
    let row: Option<(String, String, bool)> = query_as("SELECT email, code_digest, expires_at > now() FROM email_codes WHERE challenge = $1 FOR UPDATE")
        .bind(&input.challenge).fetch_optional(&mut *transaction).await?;
    let invalid = || ApiError(StatusCode::UNAUTHORIZED, "Invalid or expired code");
    let Some((email, expected, unexpired)) = row else {
        return Err(invalid());
    };
    let supplied = digest(&format!("{}:{}", input.challenge, input.code));
    if !unexpired || !bool::from(expected.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(invalid());
    }

    query("DELETE FROM email_codes WHERE challenge = $1")
        .bind(&input.challenge)
        .execute(&mut *transaction)
        .await?;
    let (account_id,): (String,) = query_as("INSERT INTO leo_accounts (id, email) VALUES ($1, $2) ON CONFLICT (email) DO UPDATE SET email = EXCLUDED.email RETURNING id")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&email).fetch_one(&mut *transaction).await?;
    let token = random_token();
    let csrf = random_token();
    query("INSERT INTO web_sessions (digest, account_id, csrf, expires_at) VALUES ($1, $2, $3, now() + interval '7 days')")
        .bind(digest(&token)).bind(&account_id).bind(&csrf).execute(&mut *transaction).await?;
    transaction.commit().await?;

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
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let token = cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == "leo_session").then_some(value))
        .unwrap_or("");
    let row: Option<(String, String, String)> = query_as("SELECT a.id, a.email, s.csrf FROM web_sessions s JOIN leo_accounts a ON a.id = s.account_id WHERE s.digest = $1 AND s.expires_at > now()")
        .bind(digest(token)).fetch_optional(&service.pool).await?;
    Ok(Json(match row {
        Some((id, email, csrf)) => json!({
            "authenticated": true,
            "account": { "id": id, "email": email },
            "csrf": csrf,
            "installations": [],
        }),
        None => json!({
            "authenticated": false,
            "account": null,
            "csrf": null,
            "installations": [],
        }),
    }))
}
