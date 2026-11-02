//! Durable execution ownership. Attempt identity is the existing fencing token;
//! an expired lease never releases ownership until recovery positively fences it.
use crate::{
    error::Result,
    store::{Db, Store},
};
use cairn_contracts::{NativeModeRequest, NodeResources};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AttemptRole {
    #[default]
    Execution,
    Destination,
}

impl AttemptRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Execution => "execution",
            Self::Destination => "destination",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Attempt {
    pub id: String,
    #[serde(default)]
    pub role: AttemptRole,
    pub run_id: String,
    pub node_id: String,
    pub resources: NodeResources,
    pub runtime_id: String,
    pub created_at: i64,
    pub lease_expires_at: Option<i64>,
    #[serde(default)]
    pub released: bool,
    pub native_mode: Option<NativeModeRequest>,
    #[serde(default)]
    pub additional_disk_mi_b: u64,
    #[serde(default)]
    pub disk_materialized: bool,
    pub released_at: Option<i64>,
    #[serde(default)]
    pub lease_required: bool,
    pub lease_duration_ms: Option<u64>,
}

pub(crate) const SCHEMA: &str = "
CREATE TABLE placements(
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)>0),
    run_id TEXT NOT NULL,
    -- Keep historical ownership when its conversation is purged, like the old records.
    live_run_id TEXT REFERENCES runs(id) ON DELETE SET NULL CHECK(live_run_id IS NULL OR live_run_id=run_id),
    node_id TEXT NOT NULL CHECK(length(node_id)>0),
    -- The virtual development runner is usable before its first inventory observation.
    inventory_node_id TEXT GENERATED ALWAYS AS (nullif(node_id,'00000000-0000-4000-8000-000000000002')) VIRTUAL REFERENCES nodes(id),
    role TEXT NOT NULL CHECK(role IN ('execution','destination')),
    cpu INTEGER NOT NULL CHECK(cpu BETWEEN 1 AND 4096),
    memory_mi_b INTEGER NOT NULL CHECK(memory_mi_b BETWEEN 128 AND 1073741824),
    disk_mi_b INTEGER NOT NULL CHECK(disk_mi_b BETWEEN 0 AND 1099511627776),
    runtime_id TEXT NOT NULL,
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE INDEX placements_run ON placements(run_id);
CREATE INDEX placements_live_run ON placements(live_run_id) WHERE live_run_id IS NOT NULL;
CREATE INDEX placements_node ON placements(node_id);
CREATE TABLE attempts(
    id TEXT PRIMARY KEY NOT NULL REFERENCES placements(id) ON DELETE CASCADE,
    lease_expires_at INTEGER CHECK(lease_expires_at BETWEEN 0 AND 9007199254740991),
    released INTEGER NOT NULL CHECK(released IN (0,1)),
    native_mode TEXT CHECK(native_mode IN ('protected','administrator')),
    native_generation TEXT,
    additional_disk_mi_b INTEGER NOT NULL CHECK(additional_disk_mi_b BETWEEN 0 AND 1099511627776),
    disk_materialized INTEGER NOT NULL CHECK(disk_materialized IN (0,1)),
    released_at INTEGER CHECK(released_at BETWEEN 0 AND 9007199254740991),
    lease_required INTEGER NOT NULL CHECK(lease_required IN (0,1)),
    lease_duration_ms INTEGER CHECK(lease_duration_ms BETWEEN 0 AND 9007199254740991),
    CHECK(native_mode IS NOT NULL OR native_generation IS NULL)
) WITHOUT ROWID;
CREATE INDEX attempts_released ON attempts(released_at);
";

const COLUMNS: &str = "p.id,p.role,p.run_id,p.node_id,p.cpu,p.memory_mi_b,p.disk_mi_b,p.runtime_id,p.created_at,a.lease_expires_at,a.released,a.native_mode,a.native_generation,a.additional_disk_mi_b,a.disk_materialized,a.released_at,a.lease_required,a.lease_duration_ms";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
    let role: String = row.get(1)?;
    let mode: Option<String> = row.get(11)?;
    let generation: Option<String> = row.get(12)?;
    Ok(Attempt {
        id: row.get(0)?,
        role: match role.as_str() {
            "execution" => AttemptRole::Execution,
            "destination" => AttemptRole::Destination,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
        run_id: row.get(2)?,
        node_id: row.get(3)?,
        resources: NodeResources {
            cpu: row.get(4)?,
            memory_mi_b: row.get::<_, i64>(5)? as u64,
            disk_mi_b: row.get::<_, i64>(6)? as u64,
        },
        runtime_id: row.get(7)?,
        created_at: row.get(8)?,
        lease_expires_at: row.get(9)?,
        released: row.get(10)?,
        native_mode: mode.map(|mode| NativeModeRequest {
            mode: if mode == "administrator" {
                cairn_contracts::NativeMode::Administrator
            } else {
                cairn_contracts::NativeMode::Protected
            },
            generation,
        }),
        additional_disk_mi_b: row.get::<_, i64>(13)? as u64,
        disk_materialized: row.get(14)?,
        released_at: row.get(15)?,
        lease_required: row.get(16)?,
        lease_duration_ms: row.get::<_, Option<i64>>(17)?.map(|value| value as u64),
    })
}

impl Db<'_> {
    pub fn attempt(&self, id: &str) -> Result<Option<Attempt>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM placements p JOIN attempts a ON a.id=p.id WHERE p.id=?"
            ))?
            .query_row([id], row)
            .optional()?)
    }

    pub fn attempts(&self) -> Result<Vec<Attempt>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM placements p JOIN attempts a ON a.id=p.id ORDER BY p.created_at DESC,p.id"
            ))?
            .query_map([], row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn run_attempts(&self, run: &str) -> Result<Vec<Attempt>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM placements p JOIN attempts a ON a.id=p.id WHERE p.run_id=?"
            ))?
            .query_map([run], row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_attempt(&self, attempt: &Attempt) -> Result<()> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            Db(&tx).save_attempt(attempt)?;
            tx.commit()?;
            return Ok(());
        }
        self.0.execute_batch("SAVEPOINT attempt_write")?;
        let before = self.0.total_changes();
        let result = self.write_attempt(attempt);
        match result {
            Ok(()) => {
                self.0.execute_batch("RELEASE attempt_write")?;
                Ok(())
            }
            Err(error) => {
                self.0
                    .execute_batch("ROLLBACK TO attempt_write; RELEASE attempt_write")?;
                let discarded = self.0.total_changes() - before;
                if discarded>0 && self.0.query_row("SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name='cairn_discarded_changes')",[],|row|row.get::<_,bool>(0))? {
                    self.0.query_row("SELECT cairn_discarded_changes(?)",[i64::try_from(discarded).unwrap_or(i64::MAX)],|_|Ok(()))?;
                }
                Err(error)
            }
        }
    }

    fn write_attempt(&self, attempt: &Attempt) -> Result<()> {
        attempt
            .resources
            .validate_allocation()
            .map_err(crate::error::Error::bad)?;
        // Fence identity reuse even when a caller catches this error inside its transaction.
        let existing = self.attempt(&attempt.id)?;
        if existing.as_ref().is_some_and(|previous| {
            previous.run_id != attempt.run_id
                || previous.node_id != attempt.node_id
                || previous.runtime_id != attempt.runtime_id
                || previous.role != attempt.role
        }) {
            return Err(crate::error::Error::conflict(
                "Attempt identity is already bound.",
            ));
        }
        self.0.prepare_cached("INSERT INTO placements(id,run_id,node_id,role,cpu,memory_mi_b,disk_mi_b,runtime_id,created_at,live_run_id) VALUES(?,?,?,?,?,?,?,?,?,(SELECT id FROM runs WHERE id=?2)) ON CONFLICT(id) DO UPDATE SET live_run_id=excluded.live_run_id,cpu=excluded.cpu,memory_mi_b=excluded.memory_mi_b,disk_mi_b=excluded.disk_mi_b")?.execute(params![
            attempt.id, attempt.run_id, attempt.node_id, attempt.role.as_str(), attempt.resources.cpu, attempt.resources.memory_mi_b as i64, attempt.resources.disk_mi_b as i64, attempt.runtime_id, attempt.created_at
        ])?;
        self.0.prepare_cached("INSERT INTO attempts(id,lease_expires_at,released,native_mode,native_generation,additional_disk_mi_b,disk_materialized,released_at,lease_required,lease_duration_ms) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET lease_expires_at=excluded.lease_expires_at,released=excluded.released,native_mode=excluded.native_mode,native_generation=excluded.native_generation,additional_disk_mi_b=excluded.additional_disk_mi_b,disk_materialized=excluded.disk_materialized,released_at=excluded.released_at,lease_required=excluded.lease_required,lease_duration_ms=excluded.lease_duration_ms")?.execute(params![
            attempt.id,attempt.lease_expires_at,attempt.released,
            attempt.native_mode.as_ref().map(|mode| if mode.mode == cairn_contracts::NativeMode::Administrator { "administrator" } else { "protected" }),
            attempt.native_mode.as_ref().and_then(|mode| mode.generation.as_deref()),attempt.additional_disk_mi_b as i64,attempt.disk_materialized,attempt.released_at,attempt.lease_required,attempt.lease_duration_ms.map(|n| n as i64)
        ])?;
        Ok(())
    }
}

impl Store {
    pub async fn attempt(&self, id: &str) -> Result<Option<Attempt>> {
        let id = id.to_owned();
        self.read(move |db| db.attempt(&id)).await
    }

    pub async fn attempts(&self) -> Result<Vec<Attempt>> {
        self.read(|db| db.attempts()).await
    }
}
