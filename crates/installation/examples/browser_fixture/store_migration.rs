//! Operator-only rehearsal on an offline DB copy. Never opens a Service or Vault.
use cairn_installation::{
    error::{Error, Result},
    store::Store,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::Path;

fn failed() -> Error {
    Error::bad("Store-only migration rehearsal failed; private copy retained.")
}

pub fn upgrade_copy(directory: &Path) -> Result<Value> {
    let file = directory.join("manager.db");
    // Refuse a typo rather than allowing Store::open to create an empty database.
    let connection = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| failed())?;
    let before: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| failed())?;
    drop(connection);
    let store = Store::open(directory).map_err(|_| failed())?;
    let connection = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| failed())?;
    let after: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| failed())?;
    let quick_check: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|_| failed())?;
    let foreign_key_violation = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(|_| failed())?
        .exists([])
        .map_err(|_| failed())?;
    if quick_check != "ok" || foreign_key_violation {
        return Err(failed());
    }
    drop(connection);
    drop(store);
    Ok(json!({
        "versionBefore": before,
        "versionAfter": after,
        "quickCheck": "ok",
        "foreignKeyCheck": "ok",
    }))
}
