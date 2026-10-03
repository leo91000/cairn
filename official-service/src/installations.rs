use super::{ApiError, Service, consume_limit, digest, random_token, session_token};
use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, Method, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx_core::{query::query, query_as::query_as};
use sqlx_postgres::PgPool;
use std::net::SocketAddr;
use subtle::ConstantTimeEq;

pub(super) async fn account(
    service: &Service,
    headers: &HeaderMap,
    method: &Method,
) -> Result<String, ApiError> {
    let row: Option<(String, String)> = query_as(
        "SELECT account_id, csrf FROM web_sessions WHERE digest = $1 AND expires_at > now()",
    )
    .bind(digest(session_token(headers)))
    .fetch_optional(&service.pool)
    .await?;
    let Some((id, csrf)) = row else {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "Please sign in."));
    };

    if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        let supplied = headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !bool::from(csrf.as_bytes().ct_eq(supplied.as_bytes())) {
            return Err(ApiError(StatusCode::FORBIDDEN, "Invalid CSRF token"));
        }
    }

    Ok(id)
}

pub(super) async fn list(pool: &PgPool, account: &str) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(String, String)> =
        query_as("SELECT id, name FROM installations WHERE owner_id = $1 ORDER BY created_at, id")
            .bind(account)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| {
            json!({
                "id": id,
                "name": name,
                "role": "owner",
            })
        })
        .collect())
}

pub(super) async fn claim_code(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let account = account(&service, &headers, &Method::POST).await?;
    consume_limit(&service.pool, &format!("claim-code:{account}"), 10).await?;
    query("DELETE FROM installation_claim_codes WHERE expires_at <= now()")
        .execute(&service.pool)
        .await?;

    let code = random_token();
    query("INSERT INTO installation_claim_codes (digest, account_id, expires_at) VALUES ($1, $2, now() + interval '10 minutes')")
        .bind(digest(&code)).bind(account).execute(&service.pool).await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "code": code, "expiresIn": 600 })),
    ))
}

#[derive(Deserialize)]
pub(super) struct Claim {
    code: String,
    name: String,
    protocol: u16,
}

pub(super) async fn claim(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Claim>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    consume_limit(&service.pool, &format!("claim:{}", peer.ip()), 30).await?;
    if input.protocol != leo_relay_protocol::PROTOCOL_VERSION {
        return Err(ApiError(StatusCode::CONFLICT, "Unsupported relay protocol"));
    }

    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Choose an installation name (1–100 characters)",
        ));
    }

    let mut transaction = service.pool.begin().await?;
    let owner: Option<(String,)> = query_as("DELETE FROM installation_claim_codes WHERE digest = $1 AND expires_at > now() RETURNING account_id")
        .bind(digest(&input.code)).fetch_optional(&mut *transaction).await?;
    let Some((owner,)) = owner else {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Invalid or expired claim code",
        ));
    };

    let installation = uuid::Uuid::new_v4().to_string();
    let token = random_token();
    query("INSERT INTO installations (id, owner_id, name, token_digest) VALUES ($1, $2, $3, $4)")
        .bind(&installation)
        .bind(owner)
        .bind(name)
        .bind(digest(&token))
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "installationId": installation, "token": token })),
    ))
}
