use super::{
    ApiError, EmailRequest, Service, consume_limit, installations, methods, normalized_email,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode},
};
use serde_json::{Value, json};
use sqlx_core::transaction::Transaction;
use sqlx_core::{query::query, query_as::query_as};
use sqlx_postgres::{PgConnection, Postgres};

/// The machine may check scheduling authors, but browser access stays authoritative here.
pub(super) async fn task_authors(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
) -> Result<
    (
        [(axum::http::header::HeaderName, &'static str); 1],
        Json<cairn_protocol::TaskAuthorPolicy>,
    ),
    ApiError,
> {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    let mut transaction = service.pool.begin().await?;
    let owner: Option<(String,)> = query_as(
        "SELECT owner_id FROM installations WHERE id = $1 AND token_digest = $2 AND owner_id IS NOT NULL FOR SHARE",
    ).bind(&installation).bind(super::digest(token)).fetch_optional(&mut *transaction).await?;
    let Some((owner,)) = owner else {
        return Err(ApiError::Http(
            StatusCode::UNAUTHORIZED,
            "Invalid installation identity",
        ));
    };
    let members: Vec<(String, String)> = query_as(
        "SELECT account_id, access_id FROM installation_members WHERE installation_id = $1 ORDER BY account_id",
    ).bind(&installation).fetch_all(&mut *transaction).await?;
    let policy = cairn_protocol::TaskAuthorPolicy {
        owner: cairn_protocol::TaskAuthorGrant {
            access_id: format!("owner:{installation}:{owner}"),
            account_id: owner,
        },
        members: members
            .into_iter()
            .map(
                |(account_id, access_id)| cairn_protocol::TaskAuthorGrant {
                    account_id,
                    access_id,
                },
            )
            .collect(),
    };
    transaction.commit().await?;
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(policy),
    ))
}

async fn owner_transaction<'a>(
    service: &'a Service,
    installation: &str,
    account: &str,
) -> Result<Transaction<'a, Postgres>, ApiError> {
    let mut transaction = service.pool.begin().await?;
    let owner: Option<(String,)> =
        query_as("SELECT id FROM installations WHERE id = $1 AND owner_id = $2 FOR UPDATE")
            .bind(installation)
            .bind(account)
            .fetch_optional(&mut *transaction)
            .await?;
    if owner.is_none() {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Installation not found",
        ));
    }

    Ok(transaction)
}

async fn invitation_list(
    connection: &mut PgConnection,
    installation: &str,
) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(String, String)> = query_as("SELECT id, email FROM installation_invitations WHERE installation_id = $1 AND expires_at > now() ORDER BY created_at, id")
        .bind(installation).fetch_all(connection).await?;
    Ok(rows
        .into_iter()
        .map(|(id, email)| json!({ "id": id, "email": email }))
        .collect())
}

pub(super) async fn list(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let account = installations::account(&service, &headers, &Method::GET).await?;
    let mut transaction = owner_transaction(&service, &installation, &account).await?;
    let rows: Vec<(String, String)> = query_as("SELECT a.id, a.email FROM installation_members m JOIN cairn_accounts a ON a.id = m.account_id WHERE m.installation_id = $1 ORDER BY m.joined_at, a.id")
        .bind(&installation).fetch_all(&mut *transaction).await?;
    let members: Vec<Value> = rows
        .into_iter()
        .map(|(id, email)| json!({ "id": id, "email": email }))
        .collect();
    let invitations = invitation_list(&mut transaction, &installation).await?;
    Ok(Json(
        json!({ "members": members, "invitations": invitations }),
    ))
}

pub(super) async fn invite(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
    Json(input): Json<EmailRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (account, _) = super::account::confirmed_session(&service, &headers).await?;
    consume_limit(&service.pool, &format!("invitation:{account}"), 10).await?;
    let email = normalized_email(&input.email)?;
    let mut transaction = service.pool.begin().await?;
    installations::lock_confirmed_owner(&mut transaction, &headers, &installation, &account)
        .await?;
    let existing: Option<(String,)> = query_as("SELECT a.id FROM cairn_accounts a WHERE a.email = $1 AND (a.id = $2 OR EXISTS (SELECT 1 FROM installation_members WHERE installation_id = $3 AND account_id = a.id))")
        .bind(&email).bind(&account).bind(&installation).fetch_optional(&mut *transaction).await?;
    if existing.is_some() {
        return Err(ApiError::Http(
            StatusCode::CONFLICT,
            "This account already has access",
        ));
    }

    query(
        "DELETE FROM installation_invitations WHERE installation_id = $1 AND expires_at <= now()",
    )
    .bind(&installation)
    .execute(&mut *transaction)
    .await?;

    let id = uuid::Uuid::new_v4().to_string();
    let inserted = query("INSERT INTO installation_invitations (id, installation_id, email) VALUES ($1, $2, $3) ON CONFLICT (installation_id, email) DO NOTHING")
        .bind(&id).bind(&installation).bind(&email).execute(&mut *transaction).await?;
    if inserted.rows_affected() == 0 {
        return Err(ApiError::Http(
            StatusCode::CONFLICT,
            "An invitation is already pending",
        ));
    }

    super::consume_limit_on(
        &mut transaction,
        &format!("invitation-day:{account}"),
        20,
        86_400,
    )
    .await?;

    let (name,): (String,) = query_as("SELECT name FROM installations WHERE id = $1")
        .bind(&installation)
        .fetch_one(&mut *transaction)
        .await?;
    super::audit::record(
        &mut transaction,
        super::audit::Event {
            actor_id: &account,
            installation_id: Some(&installation),
            action: super::audit::Action::InvitationCreated,
            target_id: Some(&id),
        },
    )
    .await?;
    transaction.commit().await?;

    // Mail clients can linkify plain text. Keep the useful name while removing
    // URL/email punctuation supplied by the owner from Cairn's outbound email.
    let email_name: String = name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, ' ' | '-' | '_') {
                character
            } else {
                ' '
            }
        })
        .collect();
    let email_name = email_name.split_whitespace().collect::<Vec<_>>().join(" ");
    let email_name = if email_name.is_empty() {
        "Shared installation"
    } else {
        &email_name
    };

    let url = format!("{}/?invitations=1", service.origin);
    if service
        .sender
        .send_invitation(&email, email_name, &url)
        .await
        .is_err()
    {
        let mut transaction = service.pool.begin().await?;
        query("DELETE FROM installation_invitations WHERE id = $1")
            .bind(&id)
            .execute(&mut *transaction)
            .await?;
        super::audit::record(
            &mut transaction,
            super::audit::Event {
                actor_id: &account,
                installation_id: Some(&installation),
                action: super::audit::Action::InvitationDeliveryFailed,
                target_id: Some(&id),
            },
        )
        .await?;
        transaction.commit().await?;
        return Err(ApiError::Http(
            StatusCode::SERVICE_UNAVAILABLE,
            "Invitation email delivery unavailable. Please try again.",
        ));
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "email": email })),
    ))
}

pub(super) async fn pending(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, ApiError> {
    let (_, email) = methods::authenticated(&service, &headers, false).await?;
    let rows: Vec<(String, String, String, String)> = query_as("SELECT v.id, i.id, i.name, a.email FROM installation_invitations v JOIN installations i ON i.id = v.installation_id JOIN cairn_accounts a ON a.id = i.owner_id WHERE v.email = $1 AND v.expires_at > now() ORDER BY v.created_at, v.id")
        .bind(email).fetch_all(&service.pool).await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, installation_id, name, owner)| {
                json!({
                    "id": id,
                    "installationId": installation_id,
                    "installationName": name,
                    "ownerEmail": owner,
                })
            })
            .collect(),
    ))
}

pub(super) async fn accept(
    State(service): State<Service>,
    Path(invitation): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let (account, email) = methods::authenticated(&service, &headers, true).await?;
    consume_limit(&service.pool, &format!("sharing:{account}"), 30).await?;
    let mut transaction = service.pool.begin().await?;
    let target: Option<(String,)> = query_as(
        "SELECT installation_id FROM installation_invitations WHERE id = $1 AND email = $2",
    )
    .bind(&invitation)
    .bind(&email)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((installation,)) = target else {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Invitation not found or expired",
        ));
    };
    // Serialize acceptance with cancellation, removal and departure on this installation.
    let exists: Option<(String,)> =
        query_as("SELECT id FROM installations WHERE id = $1 AND owner_id IS NOT NULL FOR UPDATE")
            .bind(&installation)
            .fetch_optional(&mut *transaction)
            .await?;
    if exists.is_none() {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Installation not found",
        ));
    }
    let consumed = query(
        "DELETE FROM installation_invitations WHERE id = $1 AND email = $2 AND expires_at > now()",
    )
    .bind(&invitation)
    .bind(email)
    .execute(&mut *transaction)
    .await?;
    if consumed.rows_affected() == 0 {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Invitation not found or expired",
        ));
    }
    query("INSERT INTO installation_members (installation_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(&installation).bind(&account).execute(&mut *transaction).await?;
    super::audit::record(
        &mut transaction,
        super::audit::Event {
            actor_id: &account,
            installation_id: Some(&installation),
            action: super::audit::Action::InvitationAccepted,
            target_id: Some(&invitation),
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_membership(
    service: &Service,
    mut transaction: Transaction<'_, Postgres>,
    installation: &str,
    account: &str,
) -> Result<StatusCode, ApiError> {
    let removed =
        query("DELETE FROM installation_members WHERE installation_id = $1 AND account_id = $2")
            .bind(installation)
            .bind(account)
            .execute(&mut *transaction)
            .await?;
    if removed.rows_affected() == 0 {
        return Err(ApiError::Http(StatusCode::NOT_FOUND, "Member not found"));
    }

    transaction.commit().await?;
    service.relay.revoke_access(installation, Some(account));
    super::relay::refresh_task_authors(service, installation).await;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn remove(
    State(service): State<Service>,
    Path((installation, member)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let (account, _) = super::account::confirmed_session(&service, &headers).await?;
    consume_limit(&service.pool, &format!("sharing:{account}"), 30).await?;
    let mut transaction = service.pool.begin().await?;
    installations::lock_confirmed_owner(&mut transaction, &headers, &installation, &account)
        .await?;
    super::audit::record(
        &mut transaction,
        super::audit::Event {
            actor_id: &account,
            installation_id: Some(&installation),
            action: super::audit::Action::MemberRemoved,
            target_id: Some(&member),
        },
    )
    .await?;
    remove_membership(&service, transaction, &installation, &member).await
}

pub(super) async fn leave(
    State(service): State<Service>,
    Path(installation): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let account = installations::account(&service, &headers, &Method::DELETE).await?;
    consume_limit(&service.pool, &format!("sharing:{account}"), 30).await?;
    let mut transaction = service.pool.begin().await?;
    let exists: Option<(String,)> =
        query_as("SELECT id FROM installations WHERE id = $1 AND owner_id IS NOT NULL FOR UPDATE")
            .bind(&installation)
            .fetch_optional(&mut *transaction)
            .await?;
    if exists.is_none() {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Installation not found",
        ));
    }

    super::audit::record(
        &mut transaction,
        super::audit::Event {
            actor_id: &account,
            installation_id: Some(&installation),
            action: super::audit::Action::MemberLeft,
            target_id: Some(&account),
        },
    )
    .await?;
    remove_membership(&service, transaction, &installation, &account).await
}

pub(super) async fn cancel(
    State(service): State<Service>,
    Path((installation, invitation)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let account = installations::account(&service, &headers, &Method::DELETE).await?;
    consume_limit(&service.pool, &format!("sharing:{account}"), 30).await?;
    let mut transaction = owner_transaction(&service, &installation, &account).await?;
    let cancelled =
        query("DELETE FROM installation_invitations WHERE installation_id = $1 AND id = $2")
            .bind(&installation)
            .bind(&invitation)
            .execute(&mut *transaction)
            .await?;
    if cancelled.rows_affected() == 0 {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Invitation not found",
        ));
    }

    super::audit::record(
        &mut transaction,
        super::audit::Event {
            actor_id: &account,
            installation_id: Some(&installation),
            action: super::audit::Action::InvitationCancelled,
            target_id: Some(&invitation),
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sharing belongs to an ownership period, never to a reclaimed machine.
pub(super) async fn clear(
    connection: &mut PgConnection,
    installation: &str,
) -> Result<(), ApiError> {
    query("DELETE FROM installation_members WHERE installation_id = $1")
        .bind(installation)
        .execute(&mut *connection)
        .await?;
    query("DELETE FROM installation_invitations WHERE installation_id = $1")
        .bind(installation)
        .execute(connection)
        .await?;
    Ok(())
}
