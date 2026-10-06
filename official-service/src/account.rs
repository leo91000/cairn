use super::*;
use axum::extract::Path;

pub(super) async fn sessions(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (account, _) = methods::authenticated(&service, &headers, false).await?;
    let rows: Vec<(String, String, String, bool)> = query_as(
        "SELECT id, to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), digest = $2 FROM web_sessions WHERE account_id = $1 AND expires_at > now() ORDER BY created_at DESC, id",
    )
    .bind(account)
    .bind(digest(session_token(&headers)))
    .fetch_all(&service.pool)
    .await?;
    let sessions: Vec<Value> = rows
        .into_iter()
        .map(|(id, created_at, expires_at, current)| {
            json!({
                "id": id,
                "createdAt": created_at,
                "expiresAt": expires_at,
                "current": current,
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
    let (account, _) = methods::authenticated(&service, &headers, true).await?;
    consume_limit(&service.pool, &format!("session-revoke:{account}"), 30).await?;
    let mut transaction = service.pool.begin().await?;
    // Serialize competing session revocations, then revalidate the caller.
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR UPDATE")
        .bind(&account)
        .execute(&mut *transaction)
        .await?;
    methods::authenticated_on(&mut transaction, &headers, true).await?;
    let removed: Option<(String,)> =
        query_as("DELETE FROM web_sessions WHERE id = $1 AND account_id = $2 RETURNING digest")
            .bind(id)
            .bind(account)
            .fetch_optional(&mut *transaction)
            .await?;
    let Some((revoked,)) = removed else {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found"));
    };
    transaction.commit().await?;
    service.relay.revoke_session(&revoked);
    if revoked == digest(session_token(&headers)) {
        Ok(clear_session_cookie(&service))
    } else {
        Ok(StatusCode::NO_CONTENT.into_response())
    }
}
