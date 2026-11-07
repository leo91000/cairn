//! Placement decisions retained with a run, independently of the A11 disk boundary.
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::{ExclusiveReservation, NativeModeRequest, NodeResources};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunPlacement {
    pub node_id: Option<String>,
    pub node_state: Option<String>,
    pub pinned_node_id: Option<String>,
    pub preferred_node_id: Option<String>,
    pub target_node_id: Option<String>,
    pub required_tags: Option<Vec<String>>,
    pub resources: Option<NodeResources>,
    pub native_mode: Option<NativeModeRequest>,
    pub native_mode_interruption: Option<String>,
    pub execution_backend: Option<String>,
    pub move_request: Option<MoveRequest>,
    pub move_reservation: Option<String>,
    pub movement_error: Option<String>,
    pub suspended_by_exclusive: Option<ExclusiveReservation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveRequest {
    pub reservation: String,
    pub node_id: String,
    pub resources: NodeResources,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_at: Option<i64>,
    #[serde(default)]
    pub automatic: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PlacementChanges {
    pub node_id: Option<Option<String>>,
    pub node_state: Option<Option<String>>,
    pub pinned_node_id: Option<Option<String>>,
    pub preferred_node_id: Option<Option<String>>,
    pub target_node_id: Option<Option<String>>,
    pub required_tags: Option<Option<Vec<String>>>,
    pub resources: Option<Option<NodeResources>>,
    pub native_mode: Option<Option<NativeModeRequest>>,
    pub native_mode_interruption: Option<Option<String>>,
    pub execution_backend: Option<Option<String>>,
    pub move_request: Option<Option<MoveRequest>>,
    pub move_reservation: Option<Option<String>>,
    pub movement_error: Option<Option<String>>,
    pub suspended_by_exclusive: Option<Option<ExclusiveReservation>>,
}

impl RunPlacement {
    pub fn replacement(self) -> PlacementChanges {
        PlacementChanges {
            node_id: Some(self.node_id),
            node_state: Some(self.node_state),
            pinned_node_id: Some(self.pinned_node_id),
            preferred_node_id: Some(self.preferred_node_id),
            target_node_id: Some(self.target_node_id),
            required_tags: Some(self.required_tags),
            resources: Some(self.resources),
            native_mode: Some(self.native_mode),
            native_mode_interruption: Some(self.native_mode_interruption),
            execution_backend: Some(self.execution_backend),
            move_request: Some(self.move_request),
            move_reservation: Some(self.move_reservation),
            movement_error: Some(self.movement_error),
            suspended_by_exclusive: Some(self.suspended_by_exclusive),
        }
    }
}

impl PlacementChanges {
    pub fn apply(self, value: &mut RunPlacement) {
        if let Some(next) = self.node_id {
            value.node_id = next;
        }
        if let Some(next) = self.node_state {
            value.node_state = next;
        }
        if let Some(next) = self.pinned_node_id {
            value.pinned_node_id = next;
        }
        if let Some(next) = self.preferred_node_id {
            value.preferred_node_id = next;
        }
        if let Some(next) = self.target_node_id {
            value.target_node_id = next;
        }
        if let Some(next) = self.required_tags {
            value.required_tags = next;
        }
        if let Some(next) = self.resources {
            value.resources = next;
        }
        if let Some(next) = self.native_mode {
            value.native_mode = next;
        }
        if let Some(next) = self.native_mode_interruption {
            value.native_mode_interruption = next;
        }
        if let Some(next) = self.execution_backend {
            value.execution_backend = next;
        }
        if let Some(next) = self.move_request {
            value.move_request = next;
        }
        if let Some(next) = self.move_reservation {
            value.move_reservation = next;
        }
        if let Some(next) = self.movement_error {
            value.movement_error = next;
        }
        if let Some(next) = self.suspended_by_exclusive {
            value.suspended_by_exclusive = next;
        }
    }
}

pub const FIELDS: &[&str] = &[
    "nodeId",
    "nodeState",
    "pinnedNodeId",
    "preferredNodeId",
    "targetNodeId",
    "requiredTags",
    "resources",
    "nativeMode",
    "nativeModeInterruption",
    "executionBackend",
    "moveRequest",
    "moveReservation",
    "movementError",
    "suspendedByExclusive",
];

pub(crate) const COLUMNS: &str = "placement_node_id,placement_node_state,placement_pinned_node_id,placement_preferred_node_id,placement_target_node_id,placement_required_tags,placement_resources,placement_native_mode,placement_native_mode_interruption,placement_execution_backend,placement_move_request,placement_move_reservation,placement_movement_error,placement_suspended_by_exclusive";

pub(crate) const ALTER: &str = "
ALTER TABLE runs ADD COLUMN placement_node_id TEXT;
ALTER TABLE runs ADD COLUMN placement_node_state TEXT CHECK(placement_node_state IN ('pausing','saving','restoring','resuming','updating','waiting-for-node'));
ALTER TABLE runs ADD COLUMN placement_pinned_node_id TEXT;
ALTER TABLE runs ADD COLUMN placement_preferred_node_id TEXT;
ALTER TABLE runs ADD COLUMN placement_target_node_id TEXT;
ALTER TABLE runs ADD COLUMN placement_required_tags TEXT CHECK(placement_required_tags IS NULL OR (json_valid(placement_required_tags) AND json_type(placement_required_tags)='array'));
ALTER TABLE runs ADD COLUMN placement_resources TEXT CHECK(placement_resources IS NULL OR json_valid(placement_resources));
ALTER TABLE runs ADD COLUMN placement_native_mode TEXT CHECK(placement_native_mode IS NULL OR json_valid(placement_native_mode));
ALTER TABLE runs ADD COLUMN placement_native_mode_interruption TEXT;
ALTER TABLE runs ADD COLUMN placement_execution_backend TEXT CHECK(placement_execution_backend IN ('native','firecracker'));
ALTER TABLE runs ADD COLUMN placement_move_request TEXT CHECK(placement_move_request IS NULL OR json_valid(placement_move_request));
ALTER TABLE runs ADD COLUMN placement_move_reservation TEXT REFERENCES attempts(id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE runs ADD COLUMN placement_movement_error TEXT;
ALTER TABLE runs ADD COLUMN placement_suspended_by_exclusive TEXT CHECK(placement_suspended_by_exclusive IS NULL OR json_valid(placement_suspended_by_exclusive));
CREATE INDEX runs_placement_node ON runs(placement_node_id);
CREATE INDEX runs_placement_move_reservation ON runs(placement_move_reservation) WHERE placement_move_reservation IS NOT NULL;
";

fn json<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<T>> {
    let value: Option<String> = row.get(index)?;
    value
        .map(|s| {
            serde_json::from_str(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .transpose()
}

pub(crate) fn row(row: &rusqlite::Row<'_>, start: usize) -> rusqlite::Result<RunPlacement> {
    Ok(RunPlacement {
        node_id: row.get(start)?,
        node_state: row.get(start + 1)?,
        pinned_node_id: row.get(start + 2)?,
        preferred_node_id: row.get(start + 3)?,
        target_node_id: row.get(start + 4)?,
        required_tags: json(row, start + 5)?,
        resources: json(row, start + 6)?,
        native_mode: json(row, start + 7)?,
        native_mode_interruption: row.get(start + 8)?,
        execution_backend: row.get(start + 9)?,
        move_request: json(row, start + 10)?,
        move_reservation: row.get(start + 11)?,
        movement_error: row.get(start + 12)?,
        suspended_by_exclusive: json(row, start + 13)?,
    })
}

impl Db<'_> {
    pub fn update_run_placement(
        &self,
        id: &str,
        changes: PlacementChanges,
    ) -> Result<serde_json::Value> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let result = Db(&tx).update_run_placement(id, changes)?;
            tx.commit()?;
            return Ok(result);
        }
        self.change_run_placement(id, changes)?;
        crate::error::required(self.run(id)?, "Run not found")
    }

    pub(crate) fn change_run_placement(&self, id: &str, changes: PlacementChanges) -> Result<()> {
        let previous = self.run_placement(id)?;
        let mut placement = previous.clone();
        changes.apply(&mut placement);
        if placement != previous {
            self.save_run_placement(id, &placement)?;
        }
        Ok(())
    }

    pub fn run_placement(&self, id: &str) -> Result<RunPlacement> {
        Ok(self
            .0
            .prepare_cached(&format!("SELECT {COLUMNS} FROM runs WHERE id=?"))?
            .query_row([id], |r| row(r, 0))?)
    }

    pub fn save_run_placement(&self, id: &str, value: &RunPlacement) -> Result<()> {
        if let Some(resources) = &value.resources {
            resources.validate_allocation().map_err(Error::bad)?;
        }
        self.0.prepare_cached("UPDATE runs SET placement_node_id=?,placement_node_state=?,placement_pinned_node_id=?,placement_preferred_node_id=?,placement_target_node_id=?,placement_required_tags=?,placement_resources=?,placement_native_mode=?,placement_native_mode_interruption=?,placement_execution_backend=?,placement_move_request=?,placement_move_reservation=?,placement_movement_error=?,placement_suspended_by_exclusive=? WHERE id=?")?.execute(params![value.node_id,value.node_state,value.pinned_node_id,value.preferred_node_id,value.target_node_id,value.required_tags.as_ref().map(serde_json::to_string).transpose()?,value.resources.as_ref().map(serde_json::to_string).transpose()?,value.native_mode.as_ref().map(serde_json::to_string).transpose()?,value.native_mode_interruption,value.execution_backend,value.move_request.as_ref().map(serde_json::to_string).transpose()?,value.move_reservation,value.movement_error,value.suspended_by_exclusive.as_ref().map(serde_json::to_string).transpose()?,id])?;
        Ok(())
    }
}

impl crate::store::Store {
    pub async fn update_run_placement(
        &self,
        id: &str,
        changes: PlacementChanges,
    ) -> Result<serde_json::Value> {
        let id = id.to_owned();
        self.transaction(move |db| db.update_run_placement(&id, changes))
            .await
    }
}
