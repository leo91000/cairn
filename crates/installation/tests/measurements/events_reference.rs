//! Frozen event writer/read mapping at f5aebef5 (A6 base). Measurement only.
use cairn_installation::{config::now, error::Result};
use rusqlite::{Connection, params};
use serde_json::Value;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: i64,
    run_id: String,
    created_at: i64,
    #[serde(rename = "type")]
    kind: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<Box<serde_json::value::RawValue>>,
}

pub struct Reference<'a>(pub &'a Connection);

impl Reference<'_> {
    pub fn event(&self, run: &str, kind: &str, text: &str, payload: Option<&Value>) -> Result<()> {
        let text = text.chars().take(16000).collect::<String>();
        self.0
            .prepare_cached(
                "INSERT INTO events(run_id,created_at,type,text,payload) VALUES(?,?,?,?,?)",
            )?
            .execute(params![
                run,
                now(),
                kind,
                text,
                payload.map(Value::to_string)
            ])?;
        // Prune in batches instead of executing a delete for every streamed delta.
        if self.0.last_insert_rowid() % 256 == 0 {
            self.0
                .prepare_cached(
                    "DELETE FROM events
                     WHERE run_id=?1
                       AND id<(SELECT COALESCE(MAX(id),0)-10000 FROM events WHERE run_id=?1)
                       AND run_id IN (SELECT id FROM runs WHERE trigger!='chat')",
                )?
                .execute([run])?;
        }
        Ok(())
    }

    pub fn event_batch(
        &self,
        run: &str,
        after: i64,
        limit: i64,
        max_bytes: usize,
    ) -> Result<Vec<Event>> {
        let sql = "SELECT id,run_id,created_at,type,text,payload FROM events WHERE run_id=? \
            AND id>? ORDER BY id LIMIT ?";
        let mut stmt = self.0.prepare_cached(sql)?;
        let rows = stmt.query_map(params![run, after, limit], event_row)?;
        within_budget(rows, limit, max_bytes)
    }

    pub fn events_before(&self, run: &str, before: i64) -> Result<(Vec<Event>, bool)> {
        self.events_between(run, 0, before)
    }

    /// The newest page of events between two IDs (both exclusive), in order, without
    /// text snapshots a later snapshot of the same message replaced within its turn.
    /// Also reports whether earlier such events remain above `after`.
    pub fn events_between(&self, run: &str, after: i64, before: i64) -> Result<(Vec<Event>, bool)> {
        let sql = "SELECT e.id,e.run_id,e.created_at,e.type,e.text,e.payload FROM events e
WHERE e.run_id=? AND e.id>? AND e.id<? AND (
 json_extract(e.payload,'$.item.type') IS NOT 'agent_message'
 OR COALESCE(json_extract(e.payload,'$.item.id'),'')=''
 OR NOT EXISTS (
  SELECT 1 FROM events n WHERE n.run_id=e.run_id
   AND json_extract(n.payload,'$.item.type')='agent_message'
   AND json_extract(n.payload,'$.item.id')=json_extract(e.payload,'$.item.id')
   AND n.id>e.id AND n.id<COALESCE((SELECT t.id FROM events t WHERE t.run_id=e.run_id AND t.type='turn.started' AND t.id>e.id ORDER BY t.id LIMIT 1),9223372036854775807)
 )) ORDER BY e.id DESC LIMIT ?";
        let mut stmt = self.0.prepare_cached(sql)?;
        let mut rows = stmt.query_map(
            params![run, after, before, HISTORY_PAGE_EVENTS + 1],
            event_row,
        )?;
        let mut events = within_budget(&mut rows, HISTORY_PAGE_EVENTS, HISTORY_PAGE_BYTES)?;
        let more = rows.next().transpose()?.is_some();
        events.reverse();
        Ok((events, more))
    }
}

/// Events per page of history, and their approximate byte budget.
const HISTORY_PAGE_EVENTS: i64 = 100;
const HISTORY_PAGE_BYTES: usize = 256 * 1024;

type EventRow = (i64, String, i64, String, String, Option<String>);

fn event_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
    ))
}

/// Stops reading rows at the byte budget, rather than allocating an entire page
/// of large historical outputs. Stored payloads are already JSON: their bytes are
/// kept as they are instead of being parsed into intermediate object nodes.
fn within_budget(
    rows: impl Iterator<Item = rusqlite::Result<EventRow>>,
    limit: i64,
    max_bytes: usize,
) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    let mut bytes = 0;
    for row in rows {
        let (id, run, created, kind, text, payload) = row?;
        bytes += text.len() + payload.as_ref().map_or(0, String::len) + 256;
        events.push(Event {
            id,
            run_id: run,
            created_at: created,
            kind,
            text,
            payload: payload
                .map(serde_json::value::RawValue::from_string)
                .transpose()?,
        });
        if bytes >= max_bytes || events.len() >= limit as usize {
            break;
        }
    }
    Ok(events)
}
