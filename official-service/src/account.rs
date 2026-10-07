use super::*;
use axum::extract::Path;

pub(super) async fn sessions(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (account, _) = methods::authenticated(&service, &headers, false).await?;
    let rows: Vec<(String, String, String, bool, String)> = query_as(
        "SELECT id, to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), digest = $2, device FROM web_sessions WHERE account_id = $1 AND expires_at > now() ORDER BY created_at DESC, id",
    )
    .bind(account)
    .bind(digest(session_token(&headers)))
    .fetch_all(&service.pool)
    .await?;
    let sessions: Vec<Value> = rows
        .into_iter()
        .map(|(id, created_at, expires_at, current, device)| {
            json!({
                "id": id,
                "createdAt": created_at,
                "expiresAt": expires_at,
                "current": current,
                "device": device,
            })
        })
        .collect();
    Ok(Json(json!({ "sessions": sessions })))
}

pub(super) async fn revoke_session(
    State(service): State<Service>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    revoke(&service, &headers, Some(&id)).await
}

pub(super) async fn revoke_others(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    revoke(&service, &headers, None).await
}

async fn revoke(
    service: &Service,
    headers: &HeaderMap,
    target: Option<&str>,
) -> Result<Response, ApiError> {
    let (account, _) = methods::authenticated(service, headers, true).await?;
    let current = digest(session_token(headers));
    let (revokes_current,): (bool,) = query_as(
        "SELECT EXISTS (SELECT 1 FROM web_sessions WHERE account_id = $1 AND id = $2 AND digest = $3)",
    )
    .bind(&account)
    .bind(target)
    .bind(&current)
    .fetch_one(&service.pool)
    .await?;
    if !revokes_current {
        confirmed_session(service, headers).await?;
    }
    consume_limit(&service.pool, &format!("session-revoke:{account}"), 30).await?;

    let mut transaction = service.pool.begin().await?;
    // Serialize competing session revocations, then revalidate the caller.
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR NO KEY UPDATE")
        .bind(&account)
        .execute(&mut *transaction)
        .await?;
    methods::authenticated_on(&mut transaction, headers, true).await?;
    if !revokes_current {
        require_recent_proof(&mut transaction, headers).await?;
    }

    let removed: Vec<(String,)> = query_as(
        "DELETE FROM web_sessions WHERE account_id = $1 AND (id = $2 OR ($2 IS NULL AND digest <> $3)) RETURNING digest",
    ).bind(&account).bind(target).bind(&current).fetch_all(&mut *transaction).await?;
    if target.is_some() && removed.is_empty() {
        return Err(ApiError::Http(StatusCode::NOT_FOUND, "Session not found"));
    }
    if !removed.is_empty() {
        audit::record(
            &mut transaction,
            audit::Event {
                actor_id: &account,
                installation_id: None,
                action: audit::Action::SessionRevoked,
                target_id: target,
            },
        )
        .await?;
    }
    transaction.commit().await?;

    let mut revoked_current = false;
    for (revoked,) in removed {
        service.relay.revoke_session(&revoked);
        revoked_current |= revoked == current;
    }

    if revoked_current {
        Ok(clear_session_cookie(service))
    } else {
        Ok(StatusCode::NO_CONTENT.into_response())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Deletion {
    email: String,
}

pub(super) async fn delete(
    State(service): State<Service>,
    headers: HeaderMap,
    Json(input): Json<Deletion>,
) -> Result<Response, ApiError> {
    let (account, email) = confirmed_session(&service, &headers).await?;
    consume_limit(&service.pool, &format!("account-delete:{account}"), 5).await?;

    if input.email.trim() != email {
        return Err(ApiError::Http(
            StatusCode::BAD_REQUEST,
            "Enter your account email to confirm deletion",
        ));
    }

    let mut attempt = 0;
    let deleted = loop {
        attempt += 1;
        match delete_access(&service, &headers, &account, &email).await {
            Err(error) if error.is_deadlock() && attempt < 3 => continue,
            result => break result?,
        }
    };

    for (installation, owner) in deleted.installations {
        service
            .relay
            .revoke_access(&installation, if owner { None } else { Some(&account) });
    }
    for (session,) in deleted.sessions {
        service.relay.revoke_session(&session);
    }

    Ok(clear_session_cookie(&service))
}

struct DeletedAccess {
    installations: Vec<(String, bool)>,
    sessions: Vec<(String,)>,
}

// Retry only the whole transaction, after rollback. Recheck the caller on every
// attempt; notify the relay only once the successful transaction commits.
async fn delete_access(
    service: &Service,
    headers: &HeaderMap,
    account: &str,
    email: &str,
) -> Result<DeletedAccess, ApiError> {
    let mut transaction = service.pool.begin().await?;
    // Block new account credentials while collecting every affected access.
    // Email operations take the address lock before either code or account rows.
    lock_email(&mut transaction, email).await?;
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR UPDATE")
        .bind(account)
        .execute(&mut *transaction)
        .await?;
    methods::authenticated_on(&mut transaction, headers, true).await?;

    let access: Vec<(String, bool)> = query_as(
        "SELECT i.id, COALESCE(i.owner_id = $1, false) FROM installations i WHERE i.owner_id = $1 OR EXISTS (SELECT 1 FROM installation_members m WHERE m.installation_id = i.id AND m.account_id = $1) ORDER BY i.id FOR UPDATE",
    ).bind(account).fetch_all(&mut *transaction).await?;
    // Row/advisory locks can wait: check the caller and proof at wall-clock time
    // after all access locks, before any irreversible account mutation.
    confirmed_session_on(&mut transaction, headers).await?;

    for (installation, owner) in &access {
        if *owner {
            installations::detach_on(&mut transaction, installation, account).await?;
        } else {
            audit::record(
                &mut transaction,
                audit::Event {
                    actor_id: account,
                    installation_id: Some(installation),
                    action: audit::Action::MemberLeft,
                    target_id: Some(account),
                },
            )
            .await?;
        }
    }
    let sessions: Vec<(String,)> =
        query_as("SELECT digest FROM web_sessions WHERE account_id = $1")
            .bind(account)
            .fetch_all(&mut *transaction)
            .await?;

    // Remove pending email proofs too: a code issued before deletion cannot
    // silently recreate the deleted identity in another browser.
    query("DELETE FROM email_codes WHERE email = $1")
        .bind(email)
        .execute(&mut *transaction)
        .await?;
    query("DELETE FROM installation_invitations WHERE email = $1")
        .bind(email)
        .execute(&mut *transaction)
        .await?;
    audit::record(
        &mut transaction,
        audit::Event {
            actor_id: account,
            installation_id: None,
            action: audit::Action::AccountDeleted,
            target_id: None,
        },
    )
    .await?;
    query("UPDATE account_audit SET account_id = NULL WHERE account_id = $1")
        .bind(account)
        .execute(&mut *transaction)
        .await?;

    query("DELETE FROM leo_accounts WHERE id = $1")
        .bind(account)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    Ok(DeletedAccess {
        installations: access,
        sessions,
    })
}

pub(super) async fn require_recent_proof(
    connection: &mut sqlx_postgres::PgConnection,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let (recent,): (bool,) = query_as("SELECT EXISTS (SELECT 1 FROM web_sessions WHERE digest = $1 AND expires_at > clock_timestamp() AND last_proof_at > clock_timestamp() - interval '5 minutes' AND last_proof_at <= clock_timestamp())")
        .bind(digest(session_token(headers))).fetch_one(connection).await?;
    if !recent {
        return Err(ApiError::Http(
            StatusCode::FORBIDDEN,
            "Confirm your identity with an email code or passkey before continuing",
        ));
    }
    Ok(())
}

pub(super) async fn confirmed_session(
    service: &Service,
    headers: &HeaderMap,
) -> Result<(String, String), ApiError> {
    let mut connection = service.pool.acquire().await?;
    confirmed_session_on(&mut connection, headers).await
}

pub(super) async fn confirmed_session_on(
    connection: &mut sqlx_postgres::PgConnection,
    headers: &HeaderMap,
) -> Result<(String, String), ApiError> {
    let account = methods::authenticated_on(connection, headers, true).await?;
    require_recent_proof(connection, headers).await?;
    Ok(account)
}

/// Call under the account lock after a successful email/passkey proof. Preserve
/// this session's bearer, CSRF, deadline and other devices' authentication age.
pub(super) async fn confirm_identity(
    connection: &mut sqlx_postgres::PgConnection,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    methods::authenticated_on(connection, headers, true).await?;
    query("UPDATE web_sessions SET last_proof_at = clock_timestamp() WHERE digest = $1")
        .bind(digest(session_token(headers)))
        .execute(connection)
        .await?;
    Ok(())
}
