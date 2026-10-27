//! A9 node inventory. The SQLite actor remains the only writer. No historical reader.
use crate::{
    config::now,
    error::{Error, Result},
    store::{Db, Store},
};
use cairn_contracts::*;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

pub(crate) const SCHEMA: &str = "
CREATE TABLE nodes(
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
    revision TEXT NOT NULL CHECK(length(revision)>0),
    local INTEGER NOT NULL CHECK(local IN (0,1)),
    accepting INTEGER NOT NULL CHECK(accepting IN (0,1)),
    revoked INTEGER NOT NULL CHECK(revoked IN (0,1)),
    cap_os TEXT NOT NULL,
    cap_arch TEXT NOT NULL,
    backend TEXT NOT NULL CHECK(backend IN ('firecracker','native')),
    kvm INTEGER NOT NULL CHECK(kvm IN (0,1)),
    ublk INTEGER NOT NULL CHECK(ublk IN (0,1)),
    native_mode_protocol INTEGER NOT NULL,
    capacity_cpu INTEGER NOT NULL CHECK(capacity_cpu BETWEEN 0 AND 1099511627776),
    capacity_memory_mi_b INTEGER NOT NULL CHECK(capacity_memory_mi_b BETWEEN 0 AND 1099511627776),
    capacity_disk_mi_b INTEGER NOT NULL CHECK(capacity_disk_mi_b BETWEEN 0 AND 1099511627776),
    capacity_disk_total_mi_b INTEGER NOT NULL CHECK(capacity_disk_total_mi_b BETWEEN 0 AND 1099511627776),
    limit_cpu INTEGER NOT NULL CHECK(limit_cpu BETWEEN 1 AND 4096),
    limit_memory_mi_b INTEGER NOT NULL CHECK(limit_memory_mi_b BETWEEN 128 AND 1073741824),
    limit_disk_mi_b INTEGER NOT NULL CHECK(limit_disk_mi_b BETWEEN 128 AND 1099511627776),
    slots INTEGER CHECK(slots IS NULL OR slots BETWEEN 1 AND 4096),
    runtime_slots INTEGER CHECK(runtime_slots IS NULL OR runtime_slots BETWEEN 1 AND 4096),
    native_enabled INTEGER NOT NULL CHECK(native_enabled IN (0,1)),
    native_mode_request TEXT CHECK(native_mode_request IS NULL OR json_valid(native_mode_request)),
    native_mode_observed TEXT CHECK(native_mode_observed IS NULL OR json_valid(native_mode_observed)),
    native_mode_proposal TEXT CHECK(native_mode_proposal IS NULL OR json_valid(native_mode_proposal)),
    native_mode_status TEXT CHECK(native_mode_status IS NULL OR native_mode_status IN ('pending','applied','failed')),
    exclusive_reservations INTEGER NOT NULL CHECK(exclusive_reservations IN (0,1)),
    exclusive_reservation TEXT CHECK(exclusive_reservation IS NULL OR json_valid(exclusive_reservation)),
    usage TEXT CHECK(usage IS NULL OR json_valid(usage)),
    budget_error TEXT ,
    pressure TEXT ,
    execution_ready INTEGER NOT NULL CHECK(execution_ready IN (0,1)),
    maintenance TEXT ,
    maintenance_error TEXT ,
    maintenance_generation TEXT ,
    maintenance_started_at INTEGER CHECK(maintenance_started_at IS NULL OR maintenance_started_at BETWEEN 0 AND 9007199254740991),
    image_digest TEXT ,
    update_error TEXT ,
    runtime_id TEXT NOT NULL,
    last_seen INTEGER CHECK(last_seen IS NULL OR last_seen BETWEEN 0 AND 9007199254740991),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    storage_cache_mi_b INTEGER NOT NULL,
    storage_memory_cache_mi_b INTEGER NOT NULL,
    storage_reserve_mi_b INTEGER NOT NULL,
    storage_reserve_percent INTEGER NOT NULL,
    storage_backup_seconds INTEGER NOT NULL,
    storage_max_dirty_seconds INTEGER NOT NULL,
    disk_growth_protocol INTEGER ,
    retained_runs TEXT CHECK(retained_runs IS NULL OR json_valid(retained_runs)),
    disk_capacity TEXT CHECK(disk_capacity IS NULL OR json_valid(disk_capacity)),
    updated_at INTEGER NOT NULL
);
CREATE INDEX nodes_inventory ON nodes(updated_at DESC,id);
CREATE TABLE node_tags(
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    system INTEGER NOT NULL CHECK(system IN (0,1)),
    position INTEGER NOT NULL CHECK(position>=0),
    tag TEXT NOT NULL CHECK(length(tag) BETWEEN 1 AND 40 AND tag NOT GLOB '*[^A-Za-z0-9_:./-]*'),
    PRIMARY KEY(node_id,system,position)
) WITHOUT ROWID;
CREATE TABLE node_trusted_accounts(
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 99),
    account_id TEXT NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
    PRIMARY KEY(node_id,position)
) WITHOUT ROWID;
CREATE TABLE node_reviewed_agents(
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position>=0),
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    PRIMARY KEY(node_id,position)
) WITHOUT ROWID;

";

const COLUMNS: &str = "id,name,revision,local,accepting,revoked,cap_os,cap_arch,backend,kvm,ublk,native_mode_protocol,capacity_cpu,capacity_memory_mi_b,capacity_disk_mi_b,capacity_disk_total_mi_b,limit_cpu,limit_memory_mi_b,limit_disk_mi_b,slots,runtime_slots,native_enabled,native_mode_request,native_mode_observed,native_mode_proposal,native_mode_status,exclusive_reservations,exclusive_reservation,usage,budget_error,pressure,execution_ready,maintenance,maintenance_error,maintenance_generation,maintenance_started_at,image_digest,update_error,runtime_id,last_seen,created_at,storage_cache_mi_b,storage_memory_cache_mi_b,storage_reserve_mi_b,storage_reserve_percent,storage_backup_seconds,storage_max_dirty_seconds,disk_growth_protocol,retained_runs,disk_capacity,updated_at";

fn optional_json<T: serde::de::DeserializeOwned>(
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

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionNode> {
    let id: String = row.get(0)?;
    let id = uuid::Uuid::parse_str(&id).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(ExecutionNode {
        id: NodeId(id),
        name: row.get(1)?,
        revision: row.get(2)?,
        local: row.get(3)?,
        accepting: row.get(4)?,
        revoked: row.get(5)?,
        slots: row.get(19)?,
        runtime_slots: row.get(20)?,
        native_enabled: row.get(21)?,
        native_mode_request: optional_json(row, 22)?,
        native_mode_observed: optional_json(row, 23)?,
        native_mode_proposal: optional_json(row, 24)?,
        native_mode_status: row.get(25)?,
        exclusive_reservations: row.get(26)?,
        exclusive_reservation: optional_json(row, 27)?,
        usage: optional_json(row, 28)?,
        budget_error: row.get(29)?,
        pressure: row.get(30)?,
        execution_ready: row.get(31)?,
        maintenance: row.get(32)?,
        maintenance_error: row.get(33)?,
        maintenance_generation: row.get(34)?,
        maintenance_started_at: row.get(35)?,
        image_digest: row.get(36)?,
        update_error: row.get(37)?,
        runtime_id: row.get(38)?,
        last_seen: row.get(39)?,
        created_at: row.get(40)?,
        disk_growth_protocol: row.get(47)?,
        retained_runs: optional_json(row, 48)?,
        disk_capacity: optional_json(row, 49)?,
        capabilities: NodeCapabilities {
            os: row.get(6)?,
            arch: row.get(7)?,
            backend: {
                let s: String = row.get(8)?;
                serde_json::from_value(Value::String(s)).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?
            },
            kvm: row.get(9)?,
            ublk: row.get(10)?,
            native_mode_protocol: row.get(11)?,
            cpu: row.get(12)?,
            memory_mi_b: row.get::<_, i64>(13)? as u64,
            disk_mi_b: row.get::<_, i64>(14)? as u64,
            disk_total_mi_b: row.get::<_, i64>(15)? as u64,
        },
        limits: NodeResources {
            cpu: row.get(16)?,
            memory_mi_b: row.get::<_, i64>(17)? as u64,
            disk_mi_b: row.get::<_, i64>(18)? as u64,
        },
        storage: NodeStoragePolicy {
            cache_mi_b: row.get::<_, i64>(41)? as u64,
            memory_cache_mi_b: row.get::<_, i64>(42)? as u64,
            reserve_mi_b: row.get::<_, i64>(43)? as u64,
            reserve_percent: row.get::<_, i64>(44)? as u64,
            backup_seconds: row.get::<_, i64>(45)? as u64,
            max_dirty_seconds: row.get::<_, i64>(46)? as u64,
        },
        tags: vec![],
        system_tags: vec![],
        native_accounts: vec![],
        native_reviewed_agents: vec![],
        status: NodeStatus::Offline,
        exclusive_agents: vec![],
        occupied_slots: 0,
        available_slots: 0,
        agents: vec![],
        stale_disks: StaleDisks::default(),
        disk_accounting: None,
    })
}

impl Db<'_> {
    pub fn nodes(&self) -> Result<Vec<ExecutionNode>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).nodes();
        }
        let mut nodes: Vec<ExecutionNode> = self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM nodes ORDER BY updated_at DESC,id"
            ))?
            .query_map([], row)?
            .collect::<rusqlite::Result<_>>()?;
        self.node_relations(&mut nodes, None)?;
        Ok(nodes)
    }

    pub fn node(&self, id: &str) -> Result<Option<ExecutionNode>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).node(id);
        }
        let mut node = self
            .0
            .prepare_cached(&format!("SELECT {COLUMNS} FROM nodes WHERE id=?"))?
            .query_row([id], row)
            .optional()?;
        if let Some(node) = &mut node {
            self.node_relations(std::slice::from_mut(node), Some(id))?;
        }
        Ok(node)
    }

    fn node_relations(&self, nodes: &mut [ExecutionNode], only: Option<&str>) -> Result<()> {
        let filter = if only.is_some() {
            " WHERE node_id=?1"
        } else {
            ""
        };
        let args = only.map_or_else(Vec::new, |id| vec![id]);
        let tags = self
            .0
            .prepare_cached(&format!(
                "SELECT node_id,system,tag FROM node_tags{filter} ORDER BY node_id,system,position"
            ))?
            .query_map(rusqlite::params_from_iter(&args), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let trusted = self.0.prepare_cached(&format!("SELECT node_id,account_id FROM node_trusted_accounts{filter} ORDER BY node_id,position"))?.query_map(rusqlite::params_from_iter(&args), |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let reviewed = self.0.prepare_cached(&format!("SELECT node_id,agent_id FROM node_reviewed_agents{filter} ORDER BY node_id,position"))?.query_map(rusqlite::params_from_iter(&args), |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let grants = self.0.prepare_cached(&format!("SELECT node_id,agent_id FROM agent_exclusive_nodes{filter} ORDER BY node_id,agent_id"))?.query_map(rusqlite::params_from_iter(&args), |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut by_id = nodes
            .iter_mut()
            .map(|node| (node.id.to_string(), node))
            .collect::<std::collections::HashMap<_, _>>();
        for (id, system, tag) in tags {
            if let Some(node) = by_id.get_mut(&id) {
                if system {
                    node.system_tags.push(tag);
                } else {
                    node.tags.push(tag);
                }
            }
        }
        for (id, account) in trusted {
            if let Some(node) = by_id.get_mut(&id) {
                node.native_accounts.push(account);
            }
        }
        for (id, agent) in reviewed {
            if let Some(node) = by_id.get_mut(&id) {
                node.native_reviewed_agents.push(agent);
            }
        }
        for (id, agent) in grants {
            if let Some(node) = by_id.get_mut(&id) {
                node.exclusive_agents.push(agent);
            }
        }
        Ok(())
    }

    fn write_node_relations(&self, node: &ExecutionNode) -> Result<()> {
        let id = node.id.to_string();
        self.0
            .execute("DELETE FROM node_tags WHERE node_id=?", [&id])?;
        self.0
            .execute("DELETE FROM node_trusted_accounts WHERE node_id=?", [&id])?;
        self.0
            .execute("DELETE FROM node_reviewed_agents WHERE node_id=?", [&id])?;
        for (system, tags) in [(false, &node.tags), (true, &node.system_tags)] {
            for (position, tag) in tags.iter().enumerate() {
                self.0
                    .prepare_cached("INSERT INTO node_tags VALUES(?,?,?,?)")?
                    .execute(params![id, system, position as i64, tag])?;
            }
        }
        for (position, account) in node.native_accounts.iter().enumerate() {
            self.0
                .prepare_cached("INSERT INTO node_trusted_accounts VALUES(?,?,?)")?
                .execute(params![id, position as i64, account])?;
        }
        for (position, agent) in node.native_reviewed_agents.iter().enumerate() {
            self.0
                .prepare_cached("INSERT INTO node_reviewed_agents VALUES(?,?,?)")?
                .execute(params![id, position as i64, agent])?;
        }
        Ok(())
    }

    /// Runtime observations retain the configuration revision. Configuration commands
    /// check and rotate it in their transaction, independently of heartbeat traffic.
    pub fn save_node(&self, node: &ExecutionNode) -> Result<ExecutionNode> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let result = Db(&tx).save_node(node)?;
            tx.commit()?;
            return Ok(result);
        }
        self.0.execute_batch("SAVEPOINT node_inventory")?;
        let before = self.0.total_changes();
        let result = self.write_node(node);
        match result {
            Ok(node) => {
                self.0.execute_batch("RELEASE node_inventory")?;
                Ok(node)
            }
            Err(error) => {
                self.0
                    .execute_batch("ROLLBACK TO node_inventory; RELEASE node_inventory")?;
                let discarded = self.0.total_changes() - before;
                if discarded > 0 && self.0.query_row("SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name='cairn_discarded_changes')", [], |r| r.get::<_,bool>(0))? {
                    self.0.query_row("SELECT cairn_discarded_changes(?)", [i64::try_from(discarded).unwrap_or(i64::MAX)], |_| Ok(()))?;
                }
                Err(error)
            }
        }
    }

    fn write_node(&self, node: &ExecutionNode) -> Result<ExecutionNode> {
        node.limits.validate().map_err(Error::bad)?;
        node.storage.validate().map_err(Error::bad)?;
        self.0.prepare_cached("INSERT INTO nodes(id,name,revision,local,accepting,revoked,cap_os,cap_arch,backend,kvm,ublk,native_mode_protocol,capacity_cpu,capacity_memory_mi_b,capacity_disk_mi_b,capacity_disk_total_mi_b,limit_cpu,limit_memory_mi_b,limit_disk_mi_b,slots,runtime_slots,native_enabled,native_mode_request,native_mode_observed,native_mode_proposal,native_mode_status,exclusive_reservations,exclusive_reservation,usage,budget_error,pressure,execution_ready,maintenance,maintenance_error,maintenance_generation,maintenance_started_at,image_digest,update_error,runtime_id,last_seen,created_at,storage_cache_mi_b,storage_memory_cache_mi_b,storage_reserve_mi_b,storage_reserve_percent,storage_backup_seconds,storage_max_dirty_seconds,disk_growth_protocol,retained_runs,disk_capacity,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35,?36,?37,?38,?39,?40,?41,?42,?43,?44,?45,?46,?47,?48,?49,?50,?51) ON CONFLICT(id) DO UPDATE SET name=excluded.name,revision=excluded.revision,local=excluded.local,accepting=excluded.accepting,revoked=excluded.revoked,cap_os=excluded.cap_os,cap_arch=excluded.cap_arch,backend=excluded.backend,kvm=excluded.kvm,ublk=excluded.ublk,native_mode_protocol=excluded.native_mode_protocol,capacity_cpu=excluded.capacity_cpu,capacity_memory_mi_b=excluded.capacity_memory_mi_b,capacity_disk_mi_b=excluded.capacity_disk_mi_b,capacity_disk_total_mi_b=excluded.capacity_disk_total_mi_b,limit_cpu=excluded.limit_cpu,limit_memory_mi_b=excluded.limit_memory_mi_b,limit_disk_mi_b=excluded.limit_disk_mi_b,slots=excluded.slots,runtime_slots=excluded.runtime_slots,native_enabled=excluded.native_enabled,native_mode_request=excluded.native_mode_request,native_mode_observed=excluded.native_mode_observed,native_mode_proposal=excluded.native_mode_proposal,native_mode_status=excluded.native_mode_status,exclusive_reservations=excluded.exclusive_reservations,exclusive_reservation=excluded.exclusive_reservation,usage=excluded.usage,budget_error=excluded.budget_error,pressure=excluded.pressure,execution_ready=excluded.execution_ready,maintenance=excluded.maintenance,maintenance_error=excluded.maintenance_error,maintenance_generation=excluded.maintenance_generation,maintenance_started_at=excluded.maintenance_started_at,image_digest=excluded.image_digest,update_error=excluded.update_error,runtime_id=excluded.runtime_id,last_seen=excluded.last_seen,created_at=excluded.created_at,storage_cache_mi_b=excluded.storage_cache_mi_b,storage_memory_cache_mi_b=excluded.storage_memory_cache_mi_b,storage_reserve_mi_b=excluded.storage_reserve_mi_b,storage_reserve_percent=excluded.storage_reserve_percent,storage_backup_seconds=excluded.storage_backup_seconds,storage_max_dirty_seconds=excluded.storage_max_dirty_seconds,disk_growth_protocol=excluded.disk_growth_protocol,retained_runs=excluded.retained_runs,disk_capacity=excluded.disk_capacity,updated_at=excluded.updated_at")?.execute(params![
            node.id.to_string(),
            node.name,
            node.revision,
            node.local,
            node.accepting,
            node.revoked,
            node.capabilities.os,
            node.capabilities.arch,
            serde_json::to_value(node.capabilities.backend)?.as_str().unwrap(),
            node.capabilities.kvm,
            node.capabilities.ublk,
            node.capabilities.native_mode_protocol,
            node.capabilities.cpu,
            node.capabilities.memory_mi_b as i64,
            node.capabilities.disk_mi_b as i64,
            node.capabilities.disk_total_mi_b as i64,
            node.limits.cpu,
            node.limits.memory_mi_b as i64,
            node.limits.disk_mi_b as i64,
            node.slots,
            node.runtime_slots,
            node.native_enabled,
            node.native_mode_request.as_ref().map(serde_json::to_string).transpose()?,
            node.native_mode_observed.as_ref().map(serde_json::to_string).transpose()?,
            node.native_mode_proposal.as_ref().map(serde_json::to_string).transpose()?,
            node.native_mode_status,
            node.exclusive_reservations,
            node.exclusive_reservation.as_ref().map(serde_json::to_string).transpose()?,
            node.usage.as_ref().map(serde_json::to_string).transpose()?,
            node.budget_error,
            node.pressure,
            node.execution_ready,
            node.maintenance,
            node.maintenance_error,
            node.maintenance_generation,
            node.maintenance_started_at,
            node.image_digest,
            node.update_error,
            node.runtime_id,
            node.last_seen,
            node.created_at,
            node.storage.cache_mi_b as i64,
            node.storage.memory_cache_mi_b as i64,
            node.storage.reserve_mi_b as i64,
            node.storage.reserve_percent as i64,
            node.storage.backup_seconds as i64,
            node.storage.max_dirty_seconds as i64,
            node.disk_growth_protocol,
            node.retained_runs.as_ref().map(serde_json::to_string).transpose()?,
            node.disk_capacity.as_ref().map(serde_json::to_string).transpose()?,
            now(),
        ])?;
        self.write_node_relations(node)?;
        Ok(node.clone())
    }

    /// Temporary A10/A11/A18 boundary: attempts, placement, maintenance, native
    /// provenance and disk publication still speak JSON. No records reader/writer.
    pub fn node_boundary(&self, id: &str) -> Result<Option<Value>> {
        self.node(id)?
            .map(|n| Ok(serde_json::to_value(n)?))
            .transpose()
    }

    pub fn nodes_boundary(&self) -> Result<Vec<Value>> {
        self.nodes()?
            .into_iter()
            .map(|n| Ok(serde_json::to_value(n)?))
            .collect()
    }

    pub fn save_node_boundary(&self, node: &Value) -> Result<Value> {
        Ok(serde_json::to_value(
            self.save_node(&serde_json::from_value(node.clone())?)?,
        )?)
    }
}

impl Store {
    pub async fn nodes(&self) -> Result<Vec<ExecutionNode>> {
        self.read(|db| db.nodes()).await
    }

    pub async fn node(&self, id: &str) -> Result<Option<ExecutionNode>> {
        let id = id.to_owned();
        self.read(move |db| db.node(&id)).await
    }

    pub async fn node_boundary(&self, id: &str) -> Result<Option<Value>> {
        let id = id.to_owned();
        self.read(move |db| db.node_boundary(&id)).await
    }

    pub async fn nodes_boundary(&self) -> Result<Vec<Value>> {
        self.read(|db| db.nodes_boundary()).await
    }

    pub async fn save_node_boundary(&self, node: Value) -> Result<Value> {
        self.transaction(move |db| db.save_node_boundary(&node))
            .await
    }
}

impl crate::service::Service {
    pub async fn get_node(&self, id: &str) -> Result<ExecutionNode> {
        self.store
            .node(id)
            .await?
            .ok_or_else(|| Error::not_found("Node not found."))
    }

    pub async fn node_boundary(&self, id: &str) -> Result<Value> {
        Ok(serde_json::to_value(self.get_node(id).await?)?)
    }
}
