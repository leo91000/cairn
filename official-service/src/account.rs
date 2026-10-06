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
    consume_limit(&service.pool, &format!("session-revoke:{account}"), 30).await?;
    let mut transaction = service.pool.begin().await?;
    // Serialize competing session revocations, then revalidate the caller.
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR UPDATE")
        .bind(&account)
        .execute(&mut *transaction)
        .await?;
    methods::authenticated_on(&mut transaction, headers, true).await?;
    let current = digest(session_token(headers));
    let removed: Vec<(String,)> = query_as(
        "DELETE FROM web_sessions WHERE account_id = $1 AND (id = $2 OR ($2 IS NULL AND digest <> $3)) RETURNING digest",
    ).bind(&account).bind(target).bind(&current).fetch_all(&mut *transaction).await?;
    if target.is_some() && removed.is_empty() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found"));
    }
    if !removed.is_empty() {
        audit::record(&mut transaction, &account, None, "session.revoked", target).await?;
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
    let (account, email) = methods::authenticated(&service, &headers, true).await?;
    consume_limit(&service.pool, &format!("account-delete:{account}"), 5).await?;
    if input.email.trim() != email {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Enter your account email to confirm deletion",
        ));
    }
    let mut transaction = service.pool.begin().await?;
    // Block new account credentials while collecting every affected access.
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR UPDATE")
        .bind(&account)
        .execute(&mut *transaction)
        .await?;
    methods::authenticated_on(&mut transaction, &headers, true).await?;
    let access: Vec<(String, bool)> = query_as(
        "SELECT i.id, COALESCE(i.owner_id = $1, false) FROM installations i WHERE i.owner_id = $1 OR EXISTS (SELECT 1 FROM installation_members m WHERE m.installation_id = i.id AND m.account_id = $1) ORDER BY i.id FOR UPDATE",
    ).bind(&account).fetch_all(&mut *transaction).await?;
    for (installation, owner) in &access {
        if *owner {
            installations::detach_on(&mut transaction, installation, &account).await?;
        } else {
            audit::record(
                &mut transaction,
                &account,
                Some(installation),
                "member.left",
                Some(&account),
            )
            .await?;
        }
    }
    let sessions: Vec<(String,)> =
        query_as("SELECT digest FROM web_sessions WHERE account_id = $1")
            .bind(&account)
            .fetch_all(&mut *transaction)
            .await?;
    // Remove pending email proofs too: a code issued before deletion cannot
    // silently recreate the deleted identity in another browser.
    query("DELETE FROM email_codes WHERE email = $1")
        .bind(&email)
        .execute(&mut *transaction)
        .await?;
    query("DELETE FROM installation_invitations WHERE email = $1")
        .bind(&email)
        .execute(&mut *transaction)
        .await?;
    audit::record(&mut transaction, &account, None, "account.deleted", None).await?;
    query("DELETE FROM leo_accounts WHERE id = $1")
        .bind(&account)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    for (installation, owner) in access {
        service
            .relay
            .revoke_access(&installation, if owner { None } else { Some(&account) });
    }
    for (session,) in sessions {
        service.relay.revoke_session(&session);
    }
    Ok(clear_session_cookie(&service))
}
