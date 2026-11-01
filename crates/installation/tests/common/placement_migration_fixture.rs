//! Previous-version tables for migration tests; no runtime format fallback.
#![allow(dead_code)]

use rusqlite::Connection;

pub fn previous_layout(db: &Connection) {
    db.execute_batch("PRAGMA foreign_keys=OFF;
DROP VIEW IF EXISTS run_views;
DROP TABLE IF EXISTS timeline_entries;
DROP TABLE IF EXISTS timeline_state;
DROP TABLE IF EXISTS run_progress;
DROP TABLE exclusive_members; DROP TABLE exclusive_paused_runs; DROP TABLE exclusive_reservations; DROP TABLE exclusive_sequence;
DROP TABLE attempts; DROP TABLE placements;
DROP TABLE runs;").unwrap();
    db.execute_batch(include_str!("../fixtures/v12-runs.sql"))
        .unwrap();
}

/// Compare both wire projections across historical and current layouts. A10
/// adds explicit absent placement fields; only those null-versus-absent values
/// are normalized, while every other full/summary value is retained.
pub fn run_views(db: &Connection) -> Vec<serde_json::Value> {
    let current: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='view' AND name='run_views')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let query = if current {
        "SELECT data FROM run_views ORDER BY id"
    } else {
        "SELECT data FROM runs ORDER BY id"
    };
    let query = query.replace("SELECT data", "SELECT data,summary_data");
    db.prepare(&query)
        .unwrap()
        .query_map([], |row| {
            Ok([row.get::<_, String>(0)?, row.get::<_, String>(1)?])
        })
        .unwrap()
        .map(|row| {
            let projections = row.unwrap().map(|data| {
                let mut run: serde_json::Value = serde_json::from_str(&data).unwrap();
                for field in cairn_installation::nodes::run_placement::FIELDS {
                    if run[*field].is_null() {
                        run.as_object_mut().unwrap().remove(*field);
                    }
                }
                run
            });
            serde_json::json!(projections)
        })
        .collect()
}

/// Placement changes its columns and boundary projection. Other physical run
/// values, including frozen execution data, must keep their exact SQL encoding.
pub fn retained_run_columns(db: &Connection) -> (Vec<String>, Vec<Vec<rusqlite::types::Value>>) {
    let columns: Vec<String> = db
        .prepare("PRAGMA table_xinfo('runs')")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(6)?))
        })
        .unwrap()
        .map(|column| column.unwrap())
        .filter(|(_, hidden)| *hidden != 2)
        .map(|(name, _)| name)
        .filter(|column| column != "boundary" && !column.starts_with("placement_"))
        .collect();
    let selection = columns
        .iter()
        .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",");
    let rows = db
        .prepare(&format!("SELECT {selection} FROM runs ORDER BY id"))
        .unwrap()
        .query_map([], |row| {
            (0..columns.len())
                .map(|index| row.get(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    (columns, rows)
}
