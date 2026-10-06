use super::*;

pub(super) async fn authenticated(
    service: &Service,
    headers: &HeaderMap,
    mutation: bool,
) -> Result<(String, String), ApiError> {
    let mut connection = service.pool.acquire().await?;
    authenticated_on(&mut connection, headers, mutation).await
}

pub(super) async fn authenticated_on(
    connection: &mut sqlx_postgres::PgConnection,
    headers: &HeaderMap,
    mutation: bool,
) -> Result<(String, String), ApiError> {
    let row: Option<(String, String, String)> = query_as("SELECT a.id, a.email, s.csrf FROM web_sessions s JOIN leo_accounts a ON a.id = s.account_id WHERE s.digest = $1 AND s.expires_at > clock_timestamp()")
        .bind(digest(session_token(headers))).fetch_optional(connection).await?;
    let Some((id, email, csrf)) = row else {
        return Err(ApiError::Http(
            StatusCode::UNAUTHORIZED,
            "Session expired. Please sign in again.",
        ));
    };

    let supplied = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if mutation && !bool::from(csrf.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(ApiError::Http(StatusCode::FORBIDDEN, "Invalid CSRF token"));
    }

    Ok((id, email))
}

pub(super) async fn list(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (id, _) = authenticated(&service, &headers, false).await?;

    let rows: Vec<(String, String, String)> = query_as(
        "SELECT id, kind, label FROM sign_in_methods WHERE account_id = $1 AND NOT removed ORDER BY kind, id",
    )
    .bind(id)
    .fetch_all(&service.pool)
    .await?;

    let methods: Vec<Value> = rows
        .into_iter()
        .map(|(id, kind, label)| {
            json!({
                "id": id,
                "kind": kind,
                "label": label,
            })
        })
        .collect();

    Ok(Json(json!({ "methods": methods })))
}

#[derive(Deserialize)]
pub(super) struct Removal {
    id: String,
}

pub(super) async fn remove(
    State(service): State<Service>,
    headers: HeaderMap,
    Json(input): Json<Removal>,
) -> Result<StatusCode, ApiError> {
    let (account_id, _) = authenticated(&service, &headers, true).await?;
    {
        let mut connection = service.pool.acquire().await?;
        account::require_recent_proof(&mut connection, &headers).await?;
    }

    let mut transaction = service.pool.begin().await?;
    // Serialize all changes to this account's methods, including concurrent removals.
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR UPDATE")
        .bind(&account_id)
        .execute(&mut *transaction)
        .await?;
    authenticated_on(&mut transaction, &headers, true).await?;
    account::require_recent_proof(&mut transaction, &headers).await?;

    let rows: Vec<(String,)> =
        query_as("SELECT id FROM sign_in_methods WHERE account_id = $1 AND NOT removed")
            .bind(&account_id)
            .fetch_all(&mut *transaction)
            .await?;
    if !rows.iter().any(|(id,)| id == &input.id) {
        return Err(ApiError::Http(
            StatusCode::NOT_FOUND,
            "Sign-in method not found",
        ));
    }
    if rows.len() == 1 {
        return Err(ApiError::Http(
            StatusCode::CONFLICT,
            "Keep at least one sign-in method",
        ));
    }

    query("UPDATE sign_in_methods SET removed = true, credential = NULL WHERE account_id = $1 AND id = $2")
        .bind(account_id)
        .bind(input.id)
        .execute(&mut *transaction)
        .await?;

    transaction.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
