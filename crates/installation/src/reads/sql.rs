//! Per-account read starts, read marks and set-aside outcomes. Account identities
//! come from Beacon; items keep the identity spelling they were read with.
use crate::{error::Result, store::Db};
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;

pub(crate) const SCHEMA: &str = include_str!("schema.sql");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadKind {
    Chat,
    Mission,
}

impl ReadKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Mission => "mission",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "chat" => Ok(Self::Chat),
            "mission" => Ok(Self::Mission),
            _ => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                "Unknown read kind".into(),
            )),
        }
    }
}

/// A blocked outcome set aside, by the finish of the run that reported it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredDismissal {
    pub finished_at: i64,
    pub until: Option<i64>,
}

impl Db<'_> {
    pub fn read_start(&self, account: &str) -> Result<Option<i64>> {
        Ok(self
            .0
            .prepare_cached("SELECT started_at FROM account_read_starts WHERE account_id=?")?
            .query_row([account], |row| row.get(0))
            .optional()?)
    }

    /// Keeps the first start: later calls never move it.
    pub fn start_reading(&self, account: &str, at: i64) -> Result<()> {
        self.0
            .prepare_cached("INSERT INTO account_read_starts VALUES(?,?) ON CONFLICT DO NOTHING")?
            .execute(params![account, at])?;
        Ok(())
    }

    pub fn read_marks(&self, account: &str) -> Result<HashMap<(ReadKind, String), i64>> {
        let mut statement = self.0.prepare_cached(
            "SELECT kind,item_id,finished_at FROM account_reads WHERE account_id=?",
        )?;
        let marks = statement
            .query_map([account], |row| {
                Ok((
                    (ReadKind::parse(row.get_ref(0)?.as_str()?)?, row.get(1)?),
                    row.get(2)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(marks)
    }

    /// An older observation never lowers the mark.
    pub fn mark_read(
        &self,
        account: &str,
        kind: ReadKind,
        item: &str,
        finished_at: i64,
    ) -> Result<()> {
        self.0
            .prepare_cached(
                "INSERT INTO account_reads VALUES(?,?,?,?) ON CONFLICT DO UPDATE
                 SET finished_at=max(finished_at,excluded.finished_at)",
            )?
            .execute(params![account, kind.as_str(), item, finished_at])?;
        Ok(())
    }

    pub fn dismissals(&self, account: &str) -> Result<HashMap<String, StoredDismissal>> {
        let mut statement = self.0.prepare_cached(
            "SELECT task_id,finished_at,snoozed_until FROM account_dismissals WHERE account_id=?",
        )?;
        let dismissals = statement
            .query_map([account], |row| {
                Ok((
                    row.get(0)?,
                    StoredDismissal {
                        finished_at: row.get(1)?,
                        until: row.get(2)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(dismissals)
    }

    pub fn set_dismissal(
        &self,
        account: &str,
        task: &str,
        dismissal: Option<StoredDismissal>,
    ) -> Result<()> {
        match dismissal {
            Some(dismissal) => self
                .0
                .prepare_cached(
                    "INSERT INTO account_dismissals VALUES(?,?,?,?) ON CONFLICT DO UPDATE
                     SET finished_at=excluded.finished_at,snoozed_until=excluded.snoozed_until",
                )?
                .execute(params![
                    account,
                    task,
                    dismissal.finished_at,
                    dismissal.until
                ])?,
            None => self
                .0
                .prepare_cached("DELETE FROM account_dismissals WHERE account_id=? AND task_id=?")?
                .execute(params![account, task])?,
        };
        Ok(())
    }
}
