//! Per-account read starts, read marks and snoozed conversations. Account identities
//! come from Beacon; items keep the identity spelling they were read with.
use crate::{error::Result, store::Db};
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;

pub(crate) const SCHEMA: &str = include_str!("schema.sql");

/// A conversation snoozed until a deadline, tied to the activity timestamp observed when snoozed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredSnooze {
    pub finished_at: i64,
    pub until: i64,
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

    pub fn read_marks(&self, account: &str) -> Result<HashMap<String, i64>> {
        let mut statement = self
            .0
            .prepare_cached("SELECT chat_id,finished_at FROM account_reads WHERE account_id=?")?;
        let marks = statement
            .query_map([account], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(marks)
    }

    /// An older observation never lowers the mark.
    pub fn mark_read(&self, account: &str, chat_id: &str, finished_at: i64) -> Result<()> {
        self.0
            .prepare_cached(
                "INSERT INTO account_reads VALUES(?,?,?) ON CONFLICT DO UPDATE
                 SET finished_at=max(finished_at,excluded.finished_at)",
            )?
            .execute(params![account, chat_id, finished_at])?;
        Ok(())
    }

    pub fn acknowledge_snooze(
        &self,
        account: &str,
        chat_id: &str,
        finished_at: i64,
        observed_until: Option<i64>,
    ) -> Result<()> {
        self.0
            .prepare_cached(
                "DELETE FROM account_snoozes WHERE account_id=?1 AND chat_id=?2
                 AND finished_at<=?3 AND (snoozed_until<=?3 OR snoozed_until=?4)",
            )?
            .execute(params![account, chat_id, finished_at, observed_until])?;
        Ok(())
    }

    pub fn restore_snooze(&self, account: &str, chat_id: &str, at: i64) -> Result<()> {
        self.0
            .prepare_cached(
                "UPDATE account_snoozes SET snoozed_until=max(finished_at+1,?3)
                 WHERE account_id=?1 AND chat_id=?2 AND snoozed_until>?3",
            )?
            .execute(params![account, chat_id, at])?;
        Ok(())
    }

    pub fn snoozes(&self, account: &str) -> Result<HashMap<String, StoredSnooze>> {
        let mut statement = self.0.prepare_cached(
            "SELECT chat_id,finished_at,snoozed_until FROM account_snoozes WHERE account_id=?",
        )?;
        let snoozes = statement
            .query_map([account], |row| {
                Ok((
                    row.get(0)?,
                    StoredSnooze {
                        finished_at: row.get(1)?,
                        until: row.get(2)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(snoozes)
    }

    pub fn snoozed_until(
        &self,
        account: &str,
        chat_id: &str,
        activity_at: i64,
    ) -> Result<Option<i64>> {
        Ok(self
            .0
            .prepare_cached(
                "SELECT snoozed_until FROM account_snoozes WHERE account_id=? AND chat_id=? AND finished_at>=?",
            )?
            .query_row(params![account, chat_id, activity_at], |row| row.get(0))
            .optional()?)
    }

    pub fn set_snooze(
        &self,
        account: &str,
        chat_id: &str,
        snooze: Option<StoredSnooze>,
    ) -> Result<()> {
        match snooze {
            Some(snooze) => {
                self.0
                    .prepare_cached(
                        "INSERT INTO account_snoozes VALUES(?,?,?,?) ON CONFLICT DO UPDATE
                         SET finished_at=excluded.finished_at,snoozed_until=excluded.snoozed_until
                         WHERE excluded.finished_at >= account_snoozes.finished_at",
                    )?
                    .execute(params![account, chat_id, snooze.finished_at, snooze.until])?;
            }
            None => {
                self.0
                    .prepare_cached("DELETE FROM account_snoozes WHERE account_id=? AND chat_id=?")?
                    .execute(params![account, chat_id])?;
            }
        }
        Ok(())
    }
}
