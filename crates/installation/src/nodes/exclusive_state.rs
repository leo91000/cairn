//! Exclusive reservation state shares the SQLite admission transaction.
use crate::{
    error::{Error, Result},
    store::{Db, Store},
};
use cairn_contracts::{AcquireExclusiveInput, ExclusiveState, RunId};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Reservation {
    pub id: String,
    pub node_id: String,
    pub owner_run_id: String,
    pub owner_attempt_id: String,
    pub member_attempt_ids: Vec<String>,
    pub message_id: Option<String>,
    pub reason: String,
    pub input: AcquireExclusiveInput,
    pub created_at: i64,
    pub order: u64,
    pub wait_until: i64,
    pub state: ExclusiveState,
    pub epoch: Option<u64>,
    pub expires_at: Option<i64>,
    pub requested_expires_at: Option<i64>,
    pub acquisition_started_at: Option<i64>,
    pub acquired_at: Option<i64>,
    pub release_requested_at: Option<i64>,
    #[serde(default)]
    pub paused_run_ids: Vec<String>,
    pub error: Option<String>,
}

pub(crate) const SCHEMA: &str = "
CREATE TABLE exclusive_reservations(
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id),
    owner_run_id TEXT NOT NULL,
    live_owner_run_id TEXT REFERENCES runs(id) ON DELETE SET NULL CHECK(live_owner_run_id IS NULL OR live_owner_run_id=owner_run_id),
    owner_attempt_id TEXT NOT NULL,
    live_owner_attempt_id TEXT REFERENCES attempts(id) ON DELETE SET NULL CHECK(live_owner_attempt_id IS NULL OR live_owner_attempt_id=owner_attempt_id),
    message_id TEXT,
    reason TEXT NOT NULL CHECK(length(CAST(trim(reason) AS BLOB))>=1),
    duration_seconds INTEGER NOT NULL CHECK(duration_seconds BETWEEN 1 AND 900),
    wait_seconds INTEGER NOT NULL CHECK(wait_seconds BETWEEN 0 AND 3600),
    acquisition_timeout_seconds INTEGER NOT NULL CHECK(acquisition_timeout_seconds BETWEEN 1 AND 3600),
    minimum_memory_mi_b INTEGER NOT NULL CHECK(minimum_memory_mi_b BETWEEN 0 AND 1073741824),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    queue_order INTEGER NOT NULL UNIQUE CHECK(queue_order BETWEEN 1 AND 9007199254740991),
    wait_until INTEGER NOT NULL CHECK(wait_until BETWEEN 0 AND 9007199254740991),
    state TEXT NOT NULL CHECK(state IN ('queued','acquiring','acquired','releasing','released','failed','expired')),
    epoch INTEGER CHECK(epoch BETWEEN 1 AND 9007199254740991),
    expires_at INTEGER CHECK(expires_at BETWEEN 0 AND 9007199254740991),
    requested_expires_at INTEGER CHECK(requested_expires_at BETWEEN 0 AND 9007199254740991),
    acquisition_started_at INTEGER CHECK(acquisition_started_at BETWEEN 0 AND 9007199254740991),
    acquired_at INTEGER CHECK(acquired_at BETWEEN 0 AND 9007199254740991),
    release_requested_at INTEGER CHECK(release_requested_at BETWEEN 0 AND 9007199254740991),
    error TEXT
);
CREATE INDEX exclusive_live_owner_run ON exclusive_reservations(live_owner_run_id) WHERE live_owner_run_id IS NOT NULL;
CREATE INDEX exclusive_live_owner_attempt ON exclusive_reservations(live_owner_attempt_id) WHERE live_owner_attempt_id IS NOT NULL;
CREATE INDEX exclusive_pending ON exclusive_reservations(node_id,queue_order) WHERE state IN ('queued','acquiring','acquired','releasing');
CREATE INDEX exclusive_owner ON exclusive_reservations(owner_run_id,created_at DESC);
CREATE TABLE exclusive_members(
    reservation_id TEXT NOT NULL REFERENCES exclusive_reservations(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 15),
    run_id TEXT NOT NULL,
    live_run_id TEXT REFERENCES runs(id) ON DELETE SET NULL CHECK(live_run_id IS NULL OR live_run_id=run_id),
    attempt_id TEXT NOT NULL,
    live_attempt_id TEXT REFERENCES attempts(id) ON DELETE SET NULL CHECK(live_attempt_id IS NULL OR live_attempt_id=attempt_id),
    PRIMARY KEY(reservation_id,position), UNIQUE(reservation_id,run_id)
) WITHOUT ROWID;
CREATE INDEX exclusive_members_live_run ON exclusive_members(live_run_id) WHERE live_run_id IS NOT NULL;
CREATE INDEX exclusive_members_live_attempt ON exclusive_members(live_attempt_id) WHERE live_attempt_id IS NOT NULL;
CREATE TABLE exclusive_paused_runs(
    reservation_id TEXT NOT NULL REFERENCES exclusive_reservations(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position>=0),
    run_id TEXT NOT NULL,
    live_run_id TEXT REFERENCES runs(id) ON DELETE SET NULL CHECK(live_run_id IS NULL OR live_run_id=run_id),
    PRIMARY KEY(reservation_id,position)
) WITHOUT ROWID;
CREATE INDEX exclusive_paused_live_run ON exclusive_paused_runs(live_run_id) WHERE live_run_id IS NOT NULL;
CREATE TABLE exclusive_sequence(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    queue_order INTEGER NOT NULL CHECK(queue_order BETWEEN 0 AND 9007199254740991),
    epoch INTEGER NOT NULL CHECK(epoch BETWEEN 0 AND 9007199254740991)
);
INSERT INTO exclusive_sequence VALUES(1,0,0);
";

const COLUMNS: &str = "id,node_id,owner_run_id,owner_attempt_id,message_id,reason,duration_seconds,wait_seconds,acquisition_timeout_seconds,minimum_memory_mi_b,created_at,queue_order,wait_until,state,epoch,expires_at,requested_expires_at,acquisition_started_at,acquired_at,release_requested_at,error";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Reservation> {
    let state: String = row.get(13)?;
    let state = match state.as_str() {
        "queued" => ExclusiveState::Queued,
        "acquiring" => ExclusiveState::Acquiring,
        "acquired" => ExclusiveState::Acquired,
        "releasing" => ExclusiveState::Releasing,
        "released" => ExclusiveState::Released,
        "failed" => ExclusiveState::Failed,
        "expired" => ExclusiveState::Expired,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(Reservation {
        id: row.get(0)?,
        node_id: row.get(1)?,
        owner_run_id: row.get(2)?,
        owner_attempt_id: row.get(3)?,
        message_id: row.get(4)?,
        reason: row.get(5)?,
        input: AcquireExclusiveInput {
            reason: row.get(5)?,
            duration_seconds: row.get::<_, i64>(6)? as u64,
            wait_seconds: row.get::<_, i64>(7)? as u64,
            acquisition_timeout_seconds: row.get::<_, i64>(8)? as u64,
            minimum_available_memory_mi_b: row.get::<_, i64>(9)? as u64,
            member_run_ids: vec![],
        },
        created_at: row.get(10)?,
        order: row.get::<_, i64>(11)? as u64,
        wait_until: row.get(12)?,
        state,
        epoch: row.get::<_, Option<i64>>(14)?.map(|n| n as u64),
        expires_at: row.get(15)?,
        requested_expires_at: row.get(16)?,
        acquisition_started_at: row.get(17)?,
        acquired_at: row.get(18)?,
        release_requested_at: row.get(19)?,
        error: row.get(20)?,
        member_attempt_ids: vec![],
        paused_run_ids: vec![],
    })
}

impl Db<'_> {
    fn hydrate_reservations(&self, mut records: Vec<Reservation>) -> Result<Vec<Reservation>> {
        let indexes: std::collections::HashMap<String, usize> = records
            .iter()
            .enumerate()
            .map(|(i, r)| (r.id.clone(), i))
            .collect();
        // Bound SQLite parameters and hydrate each ordered relation once per batch,
        // including a point read without scanning unrelated reservation history.
        let ids = records.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        for ids in ids.chunks(500) {
            let placeholders = vec!["?"; ids.len()].join(",");
            let query = format!(
                "SELECT reservation_id,run_id,attempt_id FROM exclusive_members WHERE reservation_id IN ({placeholders}) ORDER BY reservation_id,position"
            );
            for member in self.0.prepare_cached(&query)?.query_map(
                rusqlite::params_from_iter(ids),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )? {
                let (reservation, run, attempt) = member?;
                let record = &mut records[indexes[&reservation]];
                record
                    .input
                    .member_run_ids
                    .push(RunId(uuid::Uuid::parse_str(&run).map_err(|_| {
                        Error::bad("Invalid stored reservation member.")
                    })?));
                record.member_attempt_ids.push(attempt);
            }
            let query = format!(
                "SELECT reservation_id,run_id FROM exclusive_paused_runs WHERE reservation_id IN ({placeholders}) ORDER BY reservation_id,position"
            );
            for paused in self
                .0
                .prepare_cached(&query)?
                .query_map(rusqlite::params_from_iter(ids), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
            {
                let (reservation, run) = paused?;
                records[indexes[&reservation]].paused_run_ids.push(run);
            }
        }
        Ok(records)
    }

    pub fn exclusive_reservation(&self, id: &str) -> Result<Option<Reservation>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).exclusive_reservation(id);
        }

        let record = self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM exclusive_reservations WHERE id=?"
            ))?
            .query_row([id], row)
            .optional()?;
        Ok(self
            .hydrate_reservations(record.into_iter().collect())?
            .pop())
    }

    pub fn exclusive_reservations(&self) -> Result<Vec<Reservation>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).exclusive_reservations();
        }

        let records = self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM exclusive_reservations ORDER BY queue_order"
            ))?
            .query_map([], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        self.hydrate_reservations(records)
    }

    pub fn pending_exclusive_reservations(&self) -> Result<Vec<Reservation>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).pending_exclusive_reservations();
        }

        let records = self.0.prepare_cached(&format!("SELECT {COLUMNS} FROM exclusive_reservations WHERE state IN ('queued','acquiring','acquired','releasing') ORDER BY queue_order"))?.query_map([],row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        self.hydrate_reservations(records)
    }

    pub(crate) fn pending_exclusive_reservations_on(&self, node: &str) -> Result<Vec<Reservation>> {
        let records=self.0.prepare_cached(&format!("SELECT {COLUMNS} FROM exclusive_reservations WHERE node_id=? AND state IN ('queued','acquiring','acquired','releasing') ORDER BY queue_order"))?.query_map([node],row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        self.hydrate_reservations(records)
    }

    pub fn owned_exclusive_reservations(&self, run: &str) -> Result<Vec<Reservation>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).owned_exclusive_reservations(run);
        }
        let records=self.0.prepare_cached(&format!("SELECT {COLUMNS} FROM exclusive_reservations WHERE owner_run_id=? ORDER BY CASE WHEN state IN ('queued','acquiring','acquired','releasing') THEN 0 ELSE 1 END,created_at DESC LIMIT 20"))?.query_map([run],row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        self.hydrate_reservations(records)
    }

    pub fn save_exclusive_reservation(&self, record: &Reservation) -> Result<()> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            Db(&tx).save_exclusive_reservation(record)?;
            tx.commit()?;
            return Ok(());
        }
        self.0
            .execute_batch("SAVEPOINT exclusive_reservation_write")?;
        let before = self.0.total_changes();
        let result = self.write_exclusive_reservation(record);
        match result {
            Ok(()) => {
                self.0
                    .execute_batch("RELEASE exclusive_reservation_write")?;
                Ok(())
            }
            Err(error) => {
                self.0.execute_batch(
                    "ROLLBACK TO exclusive_reservation_write; RELEASE exclusive_reservation_write",
                )?;
                let discarded = self.0.total_changes() - before;
                if discarded>0 && self.0.query_row("SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name='cairn_discarded_changes')",[],|row|row.get::<_,bool>(0))? {
                    self.0.query_row("SELECT cairn_discarded_changes(?)",[i64::try_from(discarded).unwrap_or(i64::MAX)],|_|Ok(()))?;
                }
                Err(error)
            }
        }
    }

    fn write_exclusive_reservation(&self, record: &Reservation) -> Result<()> {
        record.input.validate_stored().map_err(Error::bad)?;
        if record.reason != record.input.reason
            || record.member_attempt_ids.len() != record.input.member_run_ids.len()
        {
            return Err(Error::bad("Invalid exclusive reservation membership."));
        }
        self.0.prepare_cached("INSERT INTO exclusive_reservations(id,node_id,owner_run_id,owner_attempt_id,message_id,reason,duration_seconds,wait_seconds,acquisition_timeout_seconds,minimum_memory_mi_b,created_at,queue_order,wait_until,state,epoch,expires_at,requested_expires_at,acquisition_started_at,acquired_at,release_requested_at,error,live_owner_run_id,live_owner_attempt_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,(SELECT id FROM runs WHERE id=?3),(SELECT id FROM attempts WHERE id=?4)) ON CONFLICT(id) DO UPDATE SET state=excluded.state,epoch=excluded.epoch,expires_at=excluded.expires_at,requested_expires_at=excluded.requested_expires_at,acquisition_started_at=excluded.acquisition_started_at,acquired_at=excluded.acquired_at,release_requested_at=excluded.release_requested_at,error=excluded.error")?.execute(params![
            record.id,record.node_id,record.owner_run_id,record.owner_attempt_id,record.message_id,record.reason,record.input.duration_seconds as i64,record.input.wait_seconds as i64,record.input.acquisition_timeout_seconds as i64,record.input.minimum_available_memory_mi_b as i64,record.created_at,record.order as i64,record.wait_until,record.state.as_str(),record.epoch.map(|n| n as i64),record.expires_at,record.requested_expires_at,record.acquisition_started_at,record.acquired_at,record.release_requested_at,record.error
        ])?;
        self.0.execute(
            "DELETE FROM exclusive_members WHERE reservation_id=?",
            [&record.id],
        )?;
        for (position, (run, attempt)) in record
            .input
            .member_run_ids
            .iter()
            .zip(&record.member_attempt_ids)
            .enumerate()
        {
            self.0.execute(
                "INSERT INTO exclusive_members(reservation_id,position,run_id,attempt_id,live_run_id,live_attempt_id) VALUES(?,?,?,?,(SELECT id FROM runs WHERE id=?3),(SELECT id FROM attempts WHERE id=?4))",
                params![record.id, position as i64, run.to_string(), attempt],
            )?;
        }
        self.0.execute(
            "DELETE FROM exclusive_paused_runs WHERE reservation_id=?",
            [&record.id],
        )?;
        for (position, run) in record.paused_run_ids.iter().enumerate() {
            self.0.execute(
                "INSERT INTO exclusive_paused_runs(reservation_id,position,run_id,live_run_id) VALUES(?,?,?,(SELECT id FROM runs WHERE id=?3))",
                params![record.id, position as i64, run],
            )?;
        }
        Ok(())
    }

    pub fn next_exclusive_order(&self) -> Result<u64> {
        Ok(self.0.query_row("UPDATE exclusive_sequence SET queue_order=queue_order+1 WHERE singleton=1 RETURNING queue_order",[],|row| row.get::<_,i64>(0))? as u64)
    }

    pub fn next_exclusive_epoch(&self, observed: u64) -> Result<u64> {
        let observed = i64::try_from(observed)
            .map_err(|_| Error::conflict("Exclusive generation exhausted."))?;
        Ok(self.0.query_row(
            "UPDATE exclusive_sequence SET epoch=max(epoch,?)+1 WHERE singleton=1 RETURNING epoch",
            [observed],
            |row| row.get::<_, i64>(0),
        )? as u64)
    }
}

impl Store {
    pub async fn exclusive_reservation(&self, id: &str) -> Result<Option<Reservation>> {
        let id = id.to_owned();
        self.read(move |db| db.exclusive_reservation(&id)).await
    }
}
