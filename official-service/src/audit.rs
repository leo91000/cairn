use super::*;
use sqlx_postgres::PgConnection;

type AuditRow = (
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
);

/// Record access metadata in the same transaction as the successful mutation.
/// The current owner sees member activity; actor IDs survive account deletion
/// for the operator's bounded incident history, without retaining their email.
pub(super) async fn record(
    connection: &mut PgConnection,
    actor: &str,
    installation: Option<&str>,
    action: &'static str,
    target: Option<&str>,
) -> Result<(), ApiError> {
    query("INSERT INTO account_audit (account_id, actor_id, installation_id, action, target_id) SELECT COALESCE(i.owner_id, a.id), $1, $2, $3, $4 FROM (VALUES (1)) AS event(n) LEFT JOIN installations i ON i.id = $2 LEFT JOIN leo_accounts a ON a.id = $1")
        .bind(actor).bind(installation).bind(action).bind(target).execute(connection).await?;
    Ok(())
}

pub(super) async fn list(
    State(service): State<Service>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (account, _) = methods::authenticated(&service, &headers, false).await?;
    let rows: Vec<AuditRow> = query_as(
        "SELECT id::text, actor_id, installation_id, action, target_id, to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') FROM account_audit WHERE (account_id = $1 OR actor_id = $1) AND created_at > now() - interval '90 days' ORDER BY id DESC LIMIT 100",
    ).bind(account).fetch_all(&service.pool).await?;
    let events: Vec<Value> = rows
        .into_iter()
        .map(|(id, actor, installation, action, target, created_at)| {
            json!({
                "id": id,
                "actorId": actor,
                "installationId": installation,
                "action": action,
                "targetId": target,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "events": events })))
}
