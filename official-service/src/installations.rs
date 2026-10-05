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
    relay: &super::relay::Relay,
) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(String, String, bool)> =
        query_as("SELECT i.id, i.name, i.owner_id = $1 FROM installations i LEFT JOIN installation_members m ON m.installation_id = i.id AND m.account_id = $1 WHERE i.owner_id IS NOT NULL AND (i.owner_id = $1 OR m.account_id = $1) ORDER BY i.created_at, i.id")
            .bind(account)
            .fetch_all(executor)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, owner)| {
            json!({
                "id": id,
                "name": name,
                "role": if owner { "owner" } else { "member" },
                "online": relay.online(&id),
            })
        })
        .collect())
}

pub(super) async fn status(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, ApiError> {
    let account = account(&service, &headers, &Method::GET).await?;
    Ok(Json(list(&service.pool, &account, &service.relay).await?))
}

pub(super) async fn claim_code(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let account = account(&service, &headers, &Method::POST).await?;
    consume_limit(&service.pool, &format!("claim-code:{account}"), 10).await?;

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
    consume_limit(&service.pool, &format!("installation-rename:{owner}"), 10).await?;
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
    let name = claim_name(&input.name, input.protocol)?;

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
        Json(json!({
            "installationId": installation,
            "token": token,
        })),
    ))
}

/// Forget permanently revokes machine proofs and removes only the official record.
pub(super) async fn forget(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let owner = account(&service, &headers, &Method::DELETE).await?;
    consume_limit(&service.pool, &format!("installation-forget:{owner}"), 10).await?;
    let mut transaction = service.pool.begin().await?;
    let forgotten = query("DELETE FROM installations WHERE id = $1 AND owner_id = $2")
        .bind(&installation)
        .bind(owner)
        .execute(&mut *transaction)
        .await?;
    if forgotten.rows_affected() == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "Installation not found"));
    }
    // Device claims deliberately have no FK: fresh requests do not reserve an installation row.
    query("DELETE FROM installation_device_claims WHERE installation_id = $1")
        .bind(&installation)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    service.relay.revoke_access(&installation, None);
    Ok(StatusCode::NO_CONTENT)
}

/// Detach revokes access without removing either installation data or its record.
pub(super) async fn detach(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let owner = account(&service, &headers, &Method::POST).await?;
    let mut transaction = service.pool.begin().await?;
    let detached =
        query("UPDATE installations SET owner_id = NULL WHERE id = $1 AND owner_id = $2")
            .bind(&installation)
            .bind(owner)
            .execute(&mut *transaction)
            .await?;
    if detached.rows_affected() == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "Installation not found"));
    }

    super::sharing::clear(&mut transaction, &installation).await?;

    query("DELETE FROM mcp_grants WHERE installation_id = $1")
        .bind(&installation)
        .execute(&mut *transaction)
        .await?;
    query("DELETE FROM mcp_codes WHERE installation_id = $1")
        .bind(&installation)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    service.relay.revoke_access(&installation, None);
    Ok(StatusCode::NO_CONTENT)
}

fn claim_name(name: &str, protocol: u16) -> Result<&str, ApiError> {
    if !leo_relay_protocol::SUPPORTED_VERSIONS.contains(&protocol) {
        return Err(ApiError(StatusCode::CONFLICT, "Unsupported relay protocol"));
    }

    installation_name(name)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MachineIdentity {
    installation_id: String,
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TokenRotation {
    token: String,
}

/// The machine persists its replacement before calling, so lost responses can be retried.
pub(super) async fn rotate_token(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(installation): Path<String>,
    headers: HeaderMap,
    Json(input): Json<TokenRotation>,
) -> Result<StatusCode, ApiError> {
    consume_limit(&service.pool, &format!("token-rotation:{}", peer.ip()), 30).await?;
    let previous = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if input.token.len() != 64
        || !input.token.bytes().all(|byte| byte.is_ascii_hexdigit())
        || previous == input.token
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Supply a new installation credential",
        ));
    }

    let replacement = digest(&input.token);
    let mut transaction = service.pool.begin().await?;
    let current: Option<(String,)> = query_as(
        "SELECT token_digest FROM installations WHERE id = $1 AND owner_id IS NOT NULL AND (token_digest = $2 OR token_digest = $3) FOR UPDATE",
    ).bind(&installation).bind(digest(previous)).bind(&replacement)
        .fetch_optional(&mut *transaction).await?;
    let Some((current,)) = current else {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Invalid installation identity",
        ));
    };
    let changed = current != replacement;
    if changed {
        query("UPDATE installations SET token_digest = $1, recovery_digest = NULL WHERE id = $2")
            .bind(&replacement)
            .bind(&installation)
            .execute(&mut *transaction)
            .await?;
        query("DELETE FROM installation_device_claims WHERE installation_id = $1")
            .bind(&installation)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    if changed {
        service.relay.revoke_access(&installation, None);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct DeviceStart {
    name: String,
    protocol: u16,
    identity: Option<MachineIdentity>,
}

/// Only possession of the private machine token can reclaim an existing record.
pub(super) async fn start_device(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<DeviceStart>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    consume_limit(&service.pool, &format!("device-start:{}", peer.ip()), 30).await?;
    let requested_name = claim_name(&input.name, input.protocol)?;
    let mut transaction = service.pool.begin().await?;
    let recovering = input.identity.is_some();
    let (installation, name) = if let Some(identity) = input.identity {
        let machine_digest = digest(&identity.token);
        let row: Option<(Option<String>, String)> = query_as(
            "SELECT owner_id, name FROM installations WHERE id = $1 AND (token_digest = $2 OR recovery_digest = $2) FOR UPDATE",
        )
        .bind(&identity.installation_id)
        .bind(&machine_digest)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((owner, name)) = row else {
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "Invalid installation identity",
            ));
        };
        if owner.is_some() {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Detach the installation before claiming it again",
            ));
        }
        // Keep the actual file's proof across rotation and interrupted delivery.
        // An owned installation still rejects recovery before this update.
        query("UPDATE installations SET recovery_digest = $1 WHERE id = $2")
            .bind(machine_digest)
            .bind(&identity.installation_id)
            .execute(&mut *transaction)
            .await?;
        (identity.installation_id, name)
    } else {
        (uuid::Uuid::new_v4().to_string(), requested_name.to_owned())
    };

    query("DELETE FROM installation_device_claims WHERE installation_id = $1")
        .bind(&installation)
        .execute(&mut *transaction)
        .await?;
    let device = random_token();
    let code = random_token()[..12].to_uppercase();
    query("INSERT INTO installation_device_claims (device_digest, user_digest, installation_id, installation_name, recovering, expires_at) VALUES ($1, $2, $3, $4, $5, now() + interval '10 minutes')")
        .bind(digest(&device)).bind(digest(&code)).bind(&installation).bind(&name).bind(recovering).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "deviceCode": device,
            "userCode": format!("{}-{}-{}", &code[..4], &code[4..8], &code[8..]),
            "verificationUri": format!("{}/claim", service.origin),
            "expiresIn": 600,
            "interval": 2,
            "fingerprint": digest(&installation),
            "name": name,
        })),
    ))
}

#[derive(Deserialize)]
pub(super) struct DeviceApproval {
    code: String,
    confirmation: Option<String>,
}

pub(super) async fn preview_device(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<DeviceApproval>,
) -> Result<Json<Value>, ApiError> {
    let account = account(&service, &headers, &Method::POST).await?;
    consume_limit(&service.pool, &format!("device-review:{account}"), 10).await?;
    consume_limit(
        &service.pool,
        &format!("device-review-ip:{}", peer.ip()),
        30,
    )
    .await?;
    let code = input.code.trim().replace('-', "").to_uppercase();
    let confirmation = random_token();
    let reviewed: Option<(String, String)> = query_as(
        "UPDATE installation_device_claims AS claim SET reviewed_by = $1, confirmation_digest = $2 WHERE user_digest = $3 AND approved_by IS NULL AND expires_at > now() AND (NOT recovering OR EXISTS (SELECT 1 FROM installations WHERE id = claim.installation_id AND owner_id IS NULL)) RETURNING installation_id, installation_name",
    )
    .bind(account)
    .bind(digest(&confirmation))
    .bind(digest(&code))
    .fetch_optional(&service.pool)
    .await?;
    let Some((installation, name)) = reviewed else {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Invalid, expired or already approved claim code",
        ));
    };

    Ok(Json(json!({
        "name": name,
        "fingerprint": digest(&installation),
        "confirmation": confirmation,
    })))
}

pub(super) async fn approve_device(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<DeviceApproval>,
) -> Result<Json<Value>, ApiError> {
    let account = account(&service, &headers, &Method::POST).await?;
    consume_limit(&service.pool, &format!("device-approval:{account}"), 10).await?;
    consume_limit(
        &service.pool,
        &format!("device-approval-ip:{}", peer.ip()),
        30,
    )
    .await?;
    let code = input.code.trim().replace('-', "").to_uppercase();
    let confirmation = input.confirmation.ok_or(ApiError(
        StatusCode::BAD_REQUEST,
        "Review the installation before confirming its claim",
    ))?;
    let approved: Option<(String,)> = query_as("UPDATE installation_device_claims SET approved_by = $1 WHERE user_digest = $2 AND reviewed_by = $1 AND confirmation_digest = $3 AND approved_by IS NULL AND expires_at > now() RETURNING installation_id")
        .bind(account).bind(digest(&code)).bind(digest(&confirmation)).fetch_optional(&service.pool).await?;
    let Some((installation,)) = approved else {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Invalid, expired or already approved claim code",
        ));
    };
    Ok(Json(json!({ "installationId": installation })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DevicePoll {
    device_code: String,
}

pub(super) async fn poll_device(
    State(service): State<Service>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<DevicePoll>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    consume_limit(&service.pool, &format!("device-poll:{}", peer.ip()), 120).await?;
    let device_digest = digest(&input.device_code);
    let mut transaction = service.pool.begin().await?;
    let row: Option<(String, bool)> = query_as("SELECT installation_id, recovering FROM installation_device_claims WHERE device_digest = $1 AND expires_at > now()")
        .bind(&device_digest).fetch_optional(&mut *transaction).await?;
    let invalid = || ApiError(StatusCode::UNAUTHORIZED, "Invalid or expired device claim");
    let Some((installation, recovering)) = row else {
        return Err(invalid());
    };
    // Existing records are always locked before their challenge, as in start.
    // Fresh claims have no installation row until approval is collected.
    if recovering {
        let row: Option<(Option<String>,)> =
            query_as("SELECT owner_id FROM installations WHERE id = $1 FOR UPDATE")
                .bind(&installation)
                .fetch_optional(&mut *transaction)
                .await?;
        if !matches!(row, Some((None,))) {
            return Err(invalid());
        }
    }
    let row: Option<(Option<String>, String)> = query_as("SELECT approved_by, installation_name FROM installation_device_claims WHERE device_digest = $1 AND expires_at > now() FOR UPDATE")
        .bind(&device_digest).fetch_optional(&mut *transaction).await?;
    let Some((approved, name)) = row else {
        return Err(invalid());
    };
    let Some(owner) = approved else {
        return Ok((StatusCode::ACCEPTED, Json(json!({ "pending": true }))));
    };
    let token = random_token();
    if recovering {
        super::sharing::clear(&mut transaction, &installation).await?;
        query("UPDATE installations SET owner_id = $1, token_digest = $2 WHERE id = $3")
            .bind(owner)
            .bind(digest(&token))
            .bind(&installation)
            .execute(&mut *transaction)
            .await?;
    } else {
        query(
            "INSERT INTO installations (id, owner_id, name, token_digest) VALUES ($1, $2, $3, $4)",
        )
        .bind(&installation)
        .bind(owner)
        .bind(name)
        .bind(digest(&token))
        .execute(&mut *transaction)
        .await?;
    }
    query("DELETE FROM installation_device_claims WHERE device_digest = $1")
        .bind(device_digest)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "installationId": installation,
            "token": token,
        })),
    ))
}

pub(super) async fn role(
    service: &Service,
    installation: &str,
    account: &str,
) -> Result<leo_relay_protocol::Role, ApiError> {
    let access: Option<(bool,)> = query_as("SELECT i.owner_id = $2 FROM installations i LEFT JOIN installation_members m ON m.installation_id = i.id AND m.account_id = $2 WHERE i.id = $1 AND i.owner_id IS NOT NULL AND (i.owner_id = $2 OR m.account_id = $2)")
        .bind(installation).bind(account).fetch_optional(&service.pool).await?;
    match access {
        Some((true,)) => Ok(leo_relay_protocol::Role::Owner),
        Some((false,)) => Ok(leo_relay_protocol::Role::Member),
        None => Err(ApiError(StatusCode::NOT_FOUND, "Installation not found")),
    }
}
