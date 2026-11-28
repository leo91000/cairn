use super::*;

/// What one action of a triggering did.
#[derive(Clone, Copy)]
pub(super) struct StepRecord<'a> {
    pub(super) status: &'a str,
    pub(super) input: Option<&'a Json>,
    pub(super) result: Option<&'a Json>,
    pub(super) error: Option<&'a str>,
}

/// An action whose input is evaluated, persisted before it runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Pending {
    pub(super) node_id: String,
    pub(super) position: u32,
    pub(super) attempts: u32,
    pub(super) input: Json,
}

#[derive(Clone, Debug)]
pub(super) struct Triggering {
    pub(super) id: String,
    pub(super) rule_id: String,
    pub(super) version: u32,
    pub(super) event: Json,
    pub(super) status: String,
    pub(super) cursor: Cursor,
    pub(super) pending: Option<Pending>,
    pub(super) in_flight: bool,
    pub(super) actions: u32,
    pub(super) tool_calls: u32,
    pub(super) counted_at: Option<i64>,
    pub(super) verify: bool,
    pub(super) verify_since: Option<i64>,
    pub(super) created_at: i64,
}

pub(super) const COLUMNS: &str = "id,rule_id,version,event,status,cursor,pending,in_flight,actions,tool_calls,counted_at,verify,verify_since,created_at";

pub(super) fn triggering(row: &rusqlite::Row<'_>) -> rusqlite::Result<Triggering> {
    let decode = |index: usize, error: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    let event: String = row.get(3)?;
    let cursor: String = row.get(5)?;
    let pending: Option<String> = row.get(6)?;
    Ok(Triggering {
        id: row.get(0)?,
        rule_id: row.get(1)?,
        version: row.get(2)?,
        event: serde_json::from_str(&event).map_err(|e| decode(3, e))?,
        status: row.get(4)?,
        cursor: serde_json::from_str(&cursor).map_err(|e| decode(5, e))?,
        pending: pending
            .map(|pending| serde_json::from_str(&pending))
            .transpose()
            .map_err(|e| decode(6, e))?,
        in_flight: row.get(7)?,
        actions: row.get(8)?,
        tool_calls: row.get(9)?,
        counted_at: row.get(10)?,
        verify: row.get(11)?,
        verify_since: row.get(12)?,
        created_at: row.get(13)?,
    })
}

impl Db<'_> {
    pub(super) fn insert_rule_triggering(
        &self,
        rule: &str,
        version: u32,
        event: &str,
        recorded: &str,
        at: i64,
    ) -> Result<Option<String>> {
        let triggering = id();
        let written = self.0.prepare_cached(
            "INSERT OR IGNORE INTO hook_rule_triggerings( \
             id,rule_id,version,event_id,event,status,cursor,pending,in_flight, \
             actions,tool_calls,counted_at,verify,verify_since,next_at,lease_until, \
             error,created_at,updated_at,finished_at,recorded_bytes) \
             VALUES(?1,?2,?3,?4,?5,'running',?6,NULL,0,0,0,NULL,0,NULL,NULL,NULL,NULL,?7,?7,NULL,?8)",
        )?.execute(params![
            triggering,
            rule,
            version,
            event,
            recorded,
            serde_json::to_string(&Cursor::default())?,
            at,
            (recorded.len() + 256) as i64,
        ])?;
        Ok((written > 0).then_some(triggering))
    }

    pub(super) fn hook_triggering(&self, id: &str) -> Result<Option<Triggering>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM hook_rule_triggerings WHERE id=?"
            ))?
            .query_row([id], triggering)
            .optional()?)
    }

    pub(super) fn save_hook_triggering(
        &self,
        t: &Triggering,
        status: &str,
        next_at: Option<i64>,
        error: Option<&str>,
    ) -> Result<()> {
        let terminal = matches!(status, "completed" | "failed" | "stopped");
        if terminal && t.actions == 0 {
            let removed = self
                .0
                .execute("DELETE FROM hook_rule_triggerings WHERE id=?", [&t.id])?;
            if removed > 0 {
                self.count_quiet(&t.rule_id)?;
                self.prune_hook_triggerings(&t.rule_id)?;
            }
            return Ok(());
        }
        let finished = terminal.then(now);
        let cursor = if terminal {
            Cursor::default()
        } else {
            t.cursor.clone()
        };
        let pending = (!terminal).then_some(t.pending.as_ref()).flatten();
        let cursor = serde_json::to_string(&cursor)?;
        if !terminal && cursor.len() > MAX_CURSOR_BYTES {
            return Err(Error::bad(
                "This triggering's control state exceeds 512 KiB.",
            ));
        }
        self.0
            .prepare_cached("UPDATE hook_rule_triggerings SET status=?2,cursor=?3,pending=?4,in_flight=?5,actions=?6,tool_calls=?7,counted_at=?8,verify=?9,verify_since=?10,next_at=?11,error=?12,updated_at=?13,finished_at=?14,lease_until=CASE WHEN ?2='running' THEN ?13+?15 ELSE NULL END WHERE id=?1")?
            .execute(params![
                t.id,
                status,
                cursor,
                pending.map(serde_json::to_string).transpose()?,
                !terminal && t.in_flight,
                t.actions,
                t.tool_calls,
                t.counted_at,
                t.verify,
                t.verify_since,
                next_at,
                error.map(|error| error.chars().take(4000).collect::<String>()),
                now(),
                finished,
                LEASE,
            ])?;
        if terminal {
            self.0.execute(
                "UPDATE hook_rule_steps SET status='failed',error=?2,finished_at=?3 \
                 WHERE triggering_id=?1 AND status IN ('running','waiting','retrying')",
                params![
                    t.id,
                    error.unwrap_or("The triggering ended before this step completed."),
                    finished
                ],
            )?;
            self.prune_hook_triggerings(&t.rule_id)?;
        }
        Ok(())
    }

    pub(super) fn hook_rule_values(&self, rule: &str) -> Result<Map<String, Json>> {
        let mut values = Map::new();
        let mut statement = self.0.prepare_cached(
            "SELECT name,value FROM hook_rule_values WHERE rule_id=? ORDER BY name",
        )?;
        for row in statement.query_map([rule], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (name, value) = row?;
            values.insert(name, serde_json::from_str(&value)?);
        }
        Ok(values)
    }

    pub(super) fn completed_step_values(&self, triggering: &str) -> Result<Map<String, Json>> {
        let mut values = Map::new();
        let mut statement = self.0.prepare_cached(
            "SELECT r.node_id,r.result FROM hook_rule_steps r WHERE r.triggering_id=?1 \
             AND r.status='completed' AND r.result IS NOT NULL AND r.position=( \
                 SELECT max(n.position) FROM hook_rule_steps n WHERE n.triggering_id=r.triggering_id \
                 AND n.node_id=r.node_id AND n.status='completed')",
        )?;
        for row in statement.query_map([triggering], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (node, value) = row?;
            values.insert(node, serde_json::from_str(&value)?);
        }
        Ok(values)
    }

    pub(super) fn record_step(
        &self,
        triggering: &str,
        position: u32,
        node: &RuleNode,
        attempts: u32,
        record: StepRecord<'_>,
    ) -> Result<()> {
        let StepRecord {
            status,
            input,
            result,
            error,
        } = record;
        let (label, kind) = match node {
            RuleNode::Action { label, action, .. } => (label.clone(), action_kind(action)),
            _ => (String::new(), "block"),
        };
        let input = input.map(Json::to_string);
        let result = result.map(Json::to_string);
        let error = error.map(|error| error.chars().take(4000).collect::<String>());
        let previous = self.0.query_row(
            "SELECT coalesce(length(CAST(input AS BLOB)),0),128+length(CAST(node_id AS BLOB)) \
             +length(CAST(label AS BLOB))+length(CAST(kind AS BLOB)) \
             +coalesce(length(CAST(input AS BLOB)),0)+coalesce(length(CAST(result AS BLOB)),0) \
             +coalesce(length(CAST(error AS BLOB)),0) FROM hook_rule_steps WHERE triggering_id=?1 AND position=?2",
            params![triggering, position], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        ).optional()?;
        let input_bytes = previous.map_or_else(
            || input.as_ref().map_or(0, String::len),
            |(bytes, _)| bytes as usize,
        );
        let bytes = 128
            + node.id().len()
            + label.len()
            + kind.len()
            + input_bytes
            + result.as_ref().map_or(0, String::len)
            + error.as_ref().map_or(0, String::len);
        let updated = self.0.execute(
            "UPDATE hook_rule_triggerings SET recorded_bytes=recorded_bytes-?2+?3 \
             WHERE id=?1 AND recorded_bytes-?2+?3<=?4",
            params![
                triggering,
                previous.map_or(0, |(_, bytes)| bytes),
                bytes as i64,
                (MAX_RECORDED_BYTES - ERROR_RESERVE) as i64
            ],
        )?;
        if updated == 0 {
            return Err(Error::bad(
                "This triggering exceeded its 1 MiB recorded-data budget. The last action is not retried.",
            ));
        }
        let at = now();
        let finished = matches!(status, "completed" | "failed").then_some(at);
        self.0
            .prepare_cached("INSERT INTO hook_rule_steps VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(triggering_id,position) DO UPDATE SET status=excluded.status,result=excluded.result,error=excluded.error,attempts=excluded.attempts,finished_at=excluded.finished_at")?
            .execute(params![
                triggering,
                position,
                node.id(),
                label,
                kind,
                status,
                input,
                result,
                error,
                attempts,
                at,
                finished,
            ])?;
        Ok(())
    }

    pub(super) fn count_quiet(&self, rule: &str) -> Result<()> {
        self.0
            .prepare_cached("INSERT INTO hook_rule_quiet_hours(rule_id,hour,count) VALUES(?1,?2,1) ON CONFLICT(rule_id,hour) DO UPDATE SET count=count+1")?
            .execute(params![rule, now() / 3_600_000])?;
        Ok(())
    }

    /// Capacity refusal is a per-rule decision, with no payload retained and no
    /// external resolution. Its receipt prevents recounting if another listener defers.
    pub(super) fn refuse_admission(
        &self,
        rule: &str,
        event: &str,
        incoming_bytes: usize,
    ) -> Result<bool> {
        let (count, bytes): (i64, i64) = self.0.query_row(
            "SELECT ((SELECT count(*) FROM hook_rule_triggerings WHERE rule_id=?1 AND status IN ('running','waiting','retrying')) + \
             (SELECT count(*) FROM hook_rule_admissions a WHERE a.rule_id=?1 AND a.event_id<>?2 \
                 AND NOT EXISTS(SELECT 1 FROM hook_rule_receipts r WHERE r.rule_id=a.rule_id AND r.event_id=a.event_id))), \
             ((SELECT coalesce(sum(event_bytes),0) FROM hook_rule_triggerings WHERE rule_id=?1 AND status IN ('running','waiting','retrying')) + \
              (SELECT coalesce(sum(event_bytes),0) FROM hook_rule_admissions a WHERE a.rule_id=?1 AND a.event_id<>?2 \
                 AND NOT EXISTS(SELECT 1 FROM hook_rule_receipts r WHERE r.rule_id=a.rule_id AND r.event_id=a.event_id)))",
            params![rule, event], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if count < MAX_PENDING && bytes as usize + incoming_bytes <= MAX_PENDING_EVENT_BYTES {
            return Ok(false);
        }
        self.record_admission_refusal(rule, event)?;
        Ok(true)
    }

    /// A receipt makes capacity/deadline refusals observable exactly once.
    pub(super) fn record_admission_refusal(&self, rule: &str, event: &str) -> Result<()> {
        let recorded = self.0.execute(
            "INSERT OR IGNORE INTO hook_rule_receipts(rule_id,event_id) VALUES(?1,?2)",
            params![rule, event],
        )?;
        if recorded > 0 {
            self.0.execute(
                "INSERT INTO hook_rule_quiet_hours(rule_id,hour,count,not_admitted) VALUES(?1,?2,0,1) \
                 ON CONFLICT(rule_id,hour) DO UPDATE SET not_admitted=not_admitted+1", params![rule, now()/3_600_000],
            )?;
        }
        Ok(())
    }

    /// Keeps the newest finished triggerings of a rule and a week of quiet hours.
    pub(super) fn prune_hook_triggerings(&self, rule: &str) -> Result<()> {
        self.0.prepare_cached("DELETE FROM hook_rule_triggerings WHERE rule_id=?1 AND status IN ('completed','failed','stopped') AND id NOT IN (SELECT id FROM hook_rule_triggerings WHERE rule_id=?1 ORDER BY created_at DESC,id DESC LIMIT ?2)")?
            .execute(params![rule, KEPT])?;
        self.0
            .prepare_cached("DELETE FROM hook_rule_quiet_hours WHERE rule_id=? AND hour<?")?
            .execute(params![rule, now() / 3_600_000 - 24 * 7])?;
        Ok(())
    }
}
