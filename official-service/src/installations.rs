use super::{ApiError, Service, consume_limit, digest, methods, random_token};
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, Method, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx_core::{query::query, query_as::query_as};
use sqlx_postgres::PgExecutor;
use std::net::SocketAddr;

pub(super) async fn account(
    service: &Service,
    headers: &HeaderMap,
    method: &Method,
) -> Result<String, ApiError> {
    let mutation = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let (id, _) = methods::authenticated(service, headers, mutation).await?;

    Ok(id)
}

pub(super) async fn list<'e>(
    executor: impl PgExecutor<'e>,
    account: &str,
) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(String, String)> =
        query_as("SELECT id, name FROM installations WHERE owner_id = $1 ORDER BY created_at, id")
            .bind(account)
            .fetch_all(executor)
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

#[derive(Deserialize)]
pub(super) struct Rename {
    name: String,
}

fn installation_name(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Choose an installation name (1–100 characters)",
        ));
    }

    Ok(name)
}

pub(super) async fn rename(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Rename>,
) -> Result<Json<Value>, ApiError> {
    let owner = account(&service, &headers, &Method::PATCH).await?;
    let name = installation_name(&input.name)?;
    let updated: Option<(String, String)> = query_as(
        "UPDATE installations SET name = $1 WHERE id = $2 AND owner_id = $3 RETURNING id, name",
    )
    .bind(name)
    .bind(installation)
    .bind(owner)
    .fetch_optional(&service.pool)
    .await?;
    let Some((id, name)) = updated else {
        return Err(ApiError(StatusCode::NOT_FOUND, "Installation not found"));
    };

    Ok(Json(json!({
        "id": id,
        "name": name,
        "role": "owner",
    })))
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

    let name = installation_name(&input.name)?;

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
