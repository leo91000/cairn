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

pub(super) enum Action {
    InstallationClaimed,
    InstallationForgotten,
    InstallationDetached,
    InvitationCreated,
    InvitationDeliveryFailed,
    InvitationAccepted,
    InvitationCancelled,
    MemberRemoved,
    MemberLeft,
    SessionRevoked,
    AccountDeleted,
}

impl Action {
    fn as_str(&self) -> &'static str {
        match self {
            Self::InstallationClaimed => "installation.claimed",
            Self::InstallationForgotten => "installation.forgotten",
            Self::InstallationDetached => "installation.detached",
            Self::InvitationCreated => "invitation.created",
            Self::InvitationDeliveryFailed => "invitation.delivery_failed",
            Self::InvitationAccepted => "invitation.accepted",
            Self::InvitationCancelled => "invitation.cancelled",
            Self::MemberRemoved => "member.removed",
            Self::MemberLeft => "member.left",
            Self::SessionRevoked => "session.revoked",
            Self::AccountDeleted => "account.deleted",
        }
    }
}

pub(super) struct Event<'a> {
    pub actor_id: &'a str,
    pub installation_id: Option<&'a str>,
    pub action: Action,
    pub target_id: Option<&'a str>,
}

/// Record access metadata in the same transaction as the successful mutation.
/// The current owner sees member activity; actor IDs survive account deletion
/// for the operator's bounded incident history, without retaining their email.
pub(super) async fn record(
    connection: &mut PgConnection,
    event: Event<'_>,
) -> Result<(), ApiError> {
    query("INSERT INTO account_audit (account_id, actor_id, installation_id, action, target_id) SELECT COALESCE(i.owner_id, a.id), $1, $2, $3, $4 FROM (VALUES (1)) AS event(n) LEFT JOIN installations i ON i.id = $2 LEFT JOIN leo_accounts a ON a.id = $1")
        .bind(event.actor_id).bind(event.installation_id).bind(event.action.as_str()).bind(event.target_id).execute(connection).await?;
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
