//! Moves JSON wire projections off the mutable run table without rewriting rows.
use crate::error::{Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

#[cfg(test)]
pub(crate) mod tests;

pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (19..=crate::store::DATABASE_VERSION).contains(&version) {
        transaction.commit()?;
        return Ok(());
    }
    if version != 18 {
        return Err(Error::bad(
            "Unexpected installation database version during run projection upgrade.",
        ));
    }

    for name in ["data", "summary_data"] {
        let hidden: Option<i64> = transaction
            .query_row(
                "SELECT hidden FROM pragma_table_xinfo('runs') WHERE name=?",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        if hidden != Some(2) {
            return Err(Error::bad(
                "Unexpected run projection schema during installation upgrade.",
            ));
        }
    }

    transaction.execute_batch(
        "ALTER TABLE runs DROP COLUMN data;
ALTER TABLE runs DROP COLUMN summary_data;",
    )?;
    transaction.execute_batch(include_str!("projections/v19.sql"))?;
    transaction.pragma_update(None, "user_version", 19)?;
    transaction.commit()?;
    Ok(())
}
