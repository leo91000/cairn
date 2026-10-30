//! The v17 KV layout of personal read state is accepted only by this atomic startup migration.
use super::sql::{ReadKind, StoredDismissal};
use crate::{
    error::{Error, Result},
    store::Db,
};
use rusqlite::{Connection, TransactionBehavior};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DismissalV17 {
    finished_at: i64,
    until: Option<i64>,
}

fn invalid() -> Error {
    Error::bad("Invalid previous read state; original database retained.")
}

/// `{account}:{chat|task}:{id}`, as v17 keyed marks and dismissals.
fn item(rest: &str) -> Result<(&str, ReadKind, &str)> {
    let (position, kind) = [(":chat:", ReadKind::Chat), (":task:", ReadKind::Mission)]
        .into_iter()
        .filter_map(|(marker, kind)| rest.find(marker).map(|position| (position, kind)))
        .min_by_key(|(position, _)| *position)
        .ok_or_else(invalid)?;
    let account = &rest[..position];
    let id = &rest[position + 6..];
    if account.is_empty() || uuid::Uuid::parse_str(id).is_err() {
        return Err(invalid());
    }
    Ok((account, kind, id))
}

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if matches!(version, 18 | 19) {
        return Ok(());
    }
    if version != 17 {
        return Err(Error::bad(
            "Unexpected installation database version during read state upgrade.",
        ));
    }
    tx.execute_batch(super::sql::SCHEMA)?;
    let rows = tx
        .prepare(
            "SELECT key,data,expires FROM kv
             WHERE key LIKE 'read-since:%' OR key LIKE 'read:%' OR key LIKE 'dismissed:%'
             ORDER BY key",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let db = Db(&tx);
    for (key, data, expires) in rows {
        if expires.is_some() {
            return Err(invalid());
        }
        if let Some(account) = key.strip_prefix("read-since:") {
            if account.is_empty() {
                return Err(invalid());
            }
            db.start_reading(account, serde_json::from_str(&data)?)?;
        } else if let Some(rest) = key.strip_prefix("read:") {
            let (account, kind, id) = item(rest)?;
            db.mark_read(account, kind, id, serde_json::from_str(&data)?)?;
        } else if let Some(rest) = key.strip_prefix("dismissed:") {
            let (account, kind, id) = item(rest)?;
            let previous: DismissalV17 = serde_json::from_str(&data)?;
            if kind != ReadKind::Mission {
                return Err(invalid());
            }
            db.set_dismissal(
                account,
                id,
                Some(StoredDismissal {
                    finished_at: previous.finished_at,
                    until: previous.until,
                }),
            )?;
        } else {
            return Err(invalid());
        }
    }
    tx.execute_batch("CREATE TABLE kv_v18(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%' AND key NOT LIKE 'chat-coding-agent:%' AND key NOT LIKE 'chat-coding-agent-request:%' AND key NOT LIKE 'chat-question:%' AND key NOT LIKE 'chat-attachment:%' AND key!='conversation-expiration' AND key NOT LIKE 'read-since:%' AND key NOT LIKE 'read:%' AND key NOT LIKE 'dismissed:%'),data TEXT NOT NULL,expires INTEGER);
INSERT INTO kv_v18 SELECT * FROM kv WHERE key NOT LIKE 'read-since:%' AND key NOT LIKE 'read:%' AND key NOT LIKE 'dismissed:%';
DROP TABLE kv;
ALTER TABLE kv_v18 RENAME TO kv;")?;
    if tx.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err(Error::bad(
            "Invalid references after the read state upgrade; original database retained.",
        ));
    }
    tx.pragma_update(None, "user_version", 18)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::item;
    use crate::reads::sql::ReadKind;

    #[test]
    fn previous_keys_split_at_their_item() {
        let id = "25d7ac0e-c91e-4a7e-b1e1-ab177dc6e02d";
        assert_eq!(
            item(&format!("acct:chat:{id}")).unwrap(),
            ("acct", ReadKind::Chat, id)
        );
        assert_eq!(
            item(&format!("acct:task:{}", id.to_uppercase())).unwrap().1,
            ReadKind::Mission
        );
        for invalid in [
            format!(":chat:{id}"),
            "acct:chat:not-a-uuid".to_owned(),
            format!("acct:agent:{id}"),
        ] {
            assert!(item(&invalid).is_err(), "{invalid}");
        }
    }
}
