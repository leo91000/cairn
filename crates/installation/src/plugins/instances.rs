//! SQL persistence for configured instances. Manifest values and typed approval
//! snapshots are JSON; identity, delegation policy and live grants are relational.
use super::connections::{AccessMode, Connection, Label};
use crate::{
    error::Result,
    store::{Db, Store},
};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;

pub(crate) const SCHEMA: &str = "
CREATE TABLE plugin_instances(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36),
 plugin_id TEXT NOT NULL REFERENCES plugin_packages(id) CHECK(length(plugin_id) BETWEEN 1 AND 80),
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 revision TEXT NOT NULL CHECK(length(revision)>0),
 settings TEXT NOT NULL CHECK(json_valid(settings) AND json_type(settings)='object'),
 secret_keys TEXT NOT NULL CHECK(json_valid(secret_keys) AND json_type(secret_keys)='array'),
 owner_access_id TEXT,
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 state TEXT NOT NULL DEFAULT 'disconnected' CHECK(state IN ('disconnected','connecting','connected','needs_review','ready')),
 state_message TEXT,
 state_updated_at INTEGER
) WITHOUT ROWID;
CREATE INDEX plugin_instances_updated ON plugin_instances(updated_at DESC,id);
CREATE INDEX plugin_instances_package ON plugin_instances(plugin_id,id);
CREATE TABLE external_delegations(
 instance_id TEXT PRIMARY KEY NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 agent_id TEXT NOT NULL REFERENCES agents(id),
 project_id TEXT REFERENCES projects(id),
 granted_access TEXT NOT NULL CHECK(json_valid(granted_access) AND json_type(granted_access)='object'),
 native_authority TEXT CHECK(native_authority IS NULL OR (json_valid(native_authority) AND json_type(native_authority)='object')),
 access_mode TEXT NOT NULL CHECK(access_mode IN ('allowlist','requests','everyone')),
 max_messages_per_hour INTEGER NOT NULL CHECK(max_messages_per_hour BETWEEN 1 AND 100),
 max_per_person_per_hour INTEGER NOT NULL CHECK(max_per_person_per_hour BETWEEN 1 AND 50)
) WITHOUT ROWID;
CREATE INDEX external_delegations_agent ON external_delegations(agent_id,instance_id);
CREATE INDEX external_delegations_project ON external_delegations(project_id,instance_id);
CREATE TABLE instance_people(
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 user_id TEXT NOT NULL CHECK(length(user_id) BETWEEN 1 AND 64),
 name TEXT NOT NULL CHECK(length(name)<=100),
 position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 99),
 PRIMARY KEY(instance_id,user_id), UNIQUE(instance_id,position)
) WITHOUT ROWID;
";

const SELECT: &str = "SELECT i.id,i.plugin_id,i.name,i.enabled,i.revision,i.settings,i.secret_keys,i.owner_access_id,i.updated_at,d.agent_id,d.project_id,d.granted_access,d.native_authority,d.access_mode,d.max_messages_per_hour,d.max_per_person_per_hour FROM plugin_instances i LEFT JOIN external_delegations d ON d.instance_id=i.id";

fn json<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Connection> {
    let mode: Option<String> = row.get(13)?;
    Ok(Connection {
        id: row.get(0)?,
        plugin_id: row.get(1)?,
        name: row.get(2)?,
        enabled: row.get(3)?,
        revision: row.get(4)?,
        settings: json(row, 5)?,
        secret_keys: json(row, 6)?,
        owner_access_id: row.get(7)?,
        updated_at: row.get(8)?,
        agent_id: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
        project_id: row.get(10)?,
        granted_access: row
            .get::<_, Option<String>>(11)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    11,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        native_authority: row
            .get::<_, Option<String>>(12)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    12,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        access_mode: match mode.as_deref() {
            Some("requests") => AccessMode::Requests,
            Some("everyone") => AccessMode::Everyone,
            _ => AccessMode::Allowlist,
        },
        max_messages_per_hour: row.get::<_, Option<u32>>(14)?.unwrap_or(20),
        max_per_person_per_hour: row.get::<_, Option<u32>>(15)?.unwrap_or(5),
        user_ids: Vec::new(),
        people: Vec::new(),
        agent_ids: Vec::new(),
    })
}

impl Db<'_> {
    pub fn plugin_instance(&self, id: &str) -> Result<Option<Connection>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).plugin_instance(id);
        }
        let Some(mut instance) = self
            .0
            .prepare_cached(&format!("{SELECT} WHERE i.id=?"))?
            .query_row([id], row)
            .optional()?
        else {
            return Ok(None);
        };
        instance.people = self
            .0
            .prepare_cached(
                "SELECT user_id,name FROM instance_people WHERE instance_id=? ORDER BY position",
            )?
            .query_map([id], |row| {
                Ok(Label {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        instance.user_ids = instance
            .people
            .iter()
            .map(|person| person.id.clone())
            .collect();
        instance.agent_ids = self
            .0
            .prepare_cached(
                "SELECT agent_id FROM instance_grants WHERE instance_id=? ORDER BY agent_id",
            )?
            .query_map([id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(instance))
    }

    /// One snapshot and three queries for the entire catalogue, independent of size.
    pub fn plugin_instances(&self) -> Result<Vec<Connection>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            return Db(&tx).plugin_instances();
        }
        let mut instances = self
            .0
            .prepare_cached(&format!("{SELECT} ORDER BY i.updated_at DESC,i.id"))?
            .query_map([], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let positions: BTreeMap<String, usize> = instances
            .iter()
            .enumerate()
            .map(|(index, instance)| (instance.id.clone(), index))
            .collect();
        let mut people = self.0.prepare_cached(
            "SELECT instance_id,user_id,name FROM instance_people ORDER BY instance_id,position",
        )?;
        for person in people.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Label {
                    id: row.get(1)?,
                    name: row.get(2)?,
                },
            ))
        })? {
            let (id, person) = person?;
            if let Some(index) = positions.get(&id) {
                instances[*index].user_ids.push(person.id.clone());
                instances[*index].people.push(person);
            }
        }
        let mut grants = self.0.prepare_cached(
            "SELECT instance_id,agent_id FROM instance_grants ORDER BY instance_id,agent_id",
        )?;
        for grant in grants.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (id, agent) = grant?;
            if let Some(index) = positions.get(&id) {
                instances[*index].agent_ids.push(agent);
            }
        }
        Ok(instances)
    }

    /// Caller owns the transaction and revision check. Agent grants are written
    /// exclusively by the existing typed profile writer, never copied from here.
    pub fn save_plugin_instance(&self, instance: &Connection) -> Result<()> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            Db(&tx).save_plugin_instance(instance)?;
            tx.commit()?;
            return Ok(());
        }
        self.0.prepare_cached("INSERT INTO plugin_instances(id,plugin_id,name,enabled,revision,settings,secret_keys,owner_access_id,updated_at) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,enabled=excluded.enabled,revision=excluded.revision,settings=excluded.settings,secret_keys=excluded.secret_keys,owner_access_id=excluded.owner_access_id,updated_at=excluded.updated_at")?.execute(params![instance.id,instance.plugin_id,instance.name,instance.enabled,instance.revision,serde_json::to_string(&instance.settings)?,serde_json::to_string(&instance.secret_keys)?,instance.owner_access_id,instance.updated_at])?;
        if instance.agent_id.is_empty() {
            self.0
                .prepare_cached("DELETE FROM external_delegations WHERE instance_id=?")?
                .execute([&instance.id])?;
        } else {
            let mode = match instance.access_mode {
                AccessMode::Allowlist => "allowlist",
                AccessMode::Requests => "requests",
                AccessMode::Everyone => "everyone",
            };
            self.0.prepare_cached("INSERT INTO external_delegations VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(instance_id) DO UPDATE SET agent_id=excluded.agent_id,project_id=excluded.project_id,granted_access=excluded.granted_access,native_authority=excluded.native_authority,access_mode=excluded.access_mode,max_messages_per_hour=excluded.max_messages_per_hour,max_per_person_per_hour=excluded.max_per_person_per_hour")?.execute(params![instance.id,instance.agent_id,instance.project_id,serde_json::to_string(&instance.granted_access)?,instance.native_authority.as_ref().map(serde_json::to_string).transpose()?,mode,instance.max_messages_per_hour,instance.max_per_person_per_hour])?;
        }
        self.0
            .prepare_cached("DELETE FROM instance_people WHERE instance_id=?")?
            .execute([&instance.id])?;
        let mut people = self
            .0
            .prepare_cached("INSERT INTO instance_people VALUES(?,?,?,?)")?;
        for (position, id) in instance.user_ids.iter().enumerate() {
            let name = instance
                .people
                .iter()
                .find(|person| person.id == *id)
                .map_or("", |person| person.name.as_str());
            people.execute(params![instance.id, id, name, position as i64])?;
        }
        Ok(())
    }

    pub fn set_plugin_status(
        &self,
        id: &str,
        revision: &str,
        status: &cairn_contracts::PluginStatus,
    ) -> Result<()> {
        let state = serde_json::to_value(status.state)?;
        self.0.prepare_cached("UPDATE plugin_instances SET state=?,state_message=?,state_updated_at=? WHERE id=? AND revision=?")?.execute(params![state.as_str(),status.message,crate::config::now(),id,revision])?;
        Ok(())
    }

    pub fn plugin_statuses(&self) -> Result<BTreeMap<String, cairn_contracts::PluginStatus>> {
        let mut statement = self
            .0
            .prepare_cached("SELECT id,state,state_message FROM plugin_instances")?;
        Ok(statement
            .query_map([], |row| {
                let state: String = row.get(1)?;
                let state =
                    serde_json::from_value(serde_json::Value::String(state)).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                Ok((
                    row.get(0)?,
                    cairn_contracts::PluginStatus {
                        state,
                        message: row.get(2)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn remove_plugin_instance(&self, id: &str) -> Result<()> {
        self.0
            .prepare_cached("DELETE FROM plugin_instances WHERE id=?")?
            .execute([id])?;
        Ok(())
    }
}

impl Store {
    pub async fn plugin_instance(&self, id: &str) -> Result<Option<Connection>> {
        let id = id.to_owned();
        self.read(move |db| db.plugin_instance(&id)).await
    }

    pub async fn plugin_instances(&self) -> Result<Vec<Connection>> {
        self.read(|db| db.plugin_instances()).await
    }
}
