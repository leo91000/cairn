//! Frozen version-12 point-reader mapping (4af96bbe), only for measurement.
use cairn_contracts::{Retry, RunState};
use cairn_installation::run_status::RunStatus;
use serde_json::Value;

const STATE_COLUMNS: &str = "id,task_id,project_id,status,trigger,created_at,started_at,finished_at,summary,error,session_id,workspace,workspace_cleaned_at,recovery_pending,resume_available,resume_count,cancel_requested_at,account_id,account_name,account_wait_reason,account_required,capacity_wait_until,retry_attempt,retry_limit,retry_next_at,retry_started_at,retry_deadline_at,retry_cause,requested_by";

fn state_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunState> {
    Ok(RunState {
        id: row.get(0)?,
        task_id: row.get(1)?,
        project_id: row.get(2)?,
        status: RunStatus::parse(&row.get::<_, String>(3)?)
            .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
        trigger: row.get(4)?,
        created_at: row.get(5)?,
        started_at: row.get(6)?,
        finished_at: row.get(7)?,
        summary: row.get(8)?,
        error: row.get(9)?,
        session_id: row.get(10)?,
        workspace: row.get(11)?,
        workspace_cleaned_at: row.get(12)?,
        recovery_pending: row.get(13)?,
        resume_available: row.get(14)?,
        resume_count: row.get(15)?,
        cancel_requested_at: row.get(16)?,
        account_id: row.get(17)?,
        account_name: row.get(18)?,
        account_wait_reason: row.get(19)?,
        account_required: row
            .get::<_, Option<String>>(20)?
            .map(|s| serde_json::from_value(Value::String(s)))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    20,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        capacity_wait_until: row.get(21)?,
        requested_by: row
            .get::<_, Option<String>>(28)?
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    28,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        retry: match row.get::<_, Option<u32>>(22)? {
            None => None,
            Some(attempt) => Some(Retry {
                attempt: u64::from(attempt),
                limit: u64::from(row.get::<_, u32>(23)?),
                next_attempt_at: row.get(24)?,
                started_at: row.get(25)?,
                deadline_at: row.get(26)?,
                cause: serde_json::from_str(&row.get::<_, String>(27)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        27,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            }),
        },
    })
}

pub(super) fn read(db: &rusqlite::Connection, id: &str) -> Value {
    db.prepare_cached(&format!(
        "SELECT {STATE_COLUMNS},snapshot,boundary FROM runs WHERE id=?"
    ))
    .unwrap()
    .query_row([id], |row| {
        let state = state_row(row)?;
        let snapshot: Option<String> = row.get(29)?;
        let boundary: String = row.get(30)?;
        let mut value: Value = serde_json::from_str(&boundary).unwrap();
        let Value::Object(fields) = serde_json::to_value(state).unwrap() else {
            unreachable!("RunState serializes to an object")
        };
        value.as_object_mut().unwrap().extend(fields);
        value["snapshot"] = snapshot.map_or(Value::Null, |s| serde_json::from_str(&s).unwrap());
        Ok(value)
    })
    .unwrap()
}
