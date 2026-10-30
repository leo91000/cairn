//! Durable plugin bookkeeping. Manifest contributions and timer payloads are the
//! only extensible data here; admission and lifecycle fields are named columns.
use crate::{
    error::Result,
    store::{Db, Store},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const SCHEMA: &str = "
CREATE TABLE plugin_packages(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 80),
 active_version TEXT,
 revision TEXT NOT NULL CHECK(length(revision)>0),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 FOREIGN KEY(id,active_version) REFERENCES plugin_versions(plugin_id,version) DEFERRABLE INITIALLY DEFERRED
) WITHOUT ROWID;
CREATE TABLE plugin_versions(
 plugin_id TEXT NOT NULL REFERENCES plugin_packages(id) ON DELETE CASCADE,
 version TEXT NOT NULL CHECK(length(version) BETWEEN 1 AND 80),
 digest TEXT NOT NULL CHECK(length(digest)=64),
 PRIMARY KEY(plugin_id,version)
) WITHOUT ROWID;
CREATE TABLE plugin_approvals(
 plugin_id TEXT NOT NULL,
 version TEXT NOT NULL,
 digest TEXT NOT NULL CHECK(length(digest)=64),
 PRIMARY KEY(plugin_id,version),
 FOREIGN KEY(plugin_id,version) REFERENCES plugin_versions(plugin_id,version) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE plugin_requests(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36),
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 user_id TEXT NOT NULL CHECK(length(user_id) BETWEEN 1 AND 64),
 name TEXT NOT NULL CHECK(length(name)<=100),
 status TEXT NOT NULL CHECK(status IN ('pending','denied')),
 mentions INTEGER NOT NULL CHECK(mentions BETWEEN 1 AND 4294967295),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 UNIQUE(instance_id,user_id)
) WITHOUT ROWID;
CREATE INDEX plugin_requests_updated ON plugin_requests(updated_at DESC,id);
CREATE TABLE plugin_activity(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36),
 delivery_id TEXT NOT NULL,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 chat_id TEXT NOT NULL,
 user_id TEXT NOT NULL,
 name TEXT NOT NULL,
 preview TEXT NOT NULL CHECK(length(preview)<=200),
 status TEXT NOT NULL CHECK(status IN ('working','replied','failed','stopped')),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE INDEX plugin_activity_recent ON plugin_activity(instance_id,created_at DESC,id);
CREATE INDEX plugin_activity_delivery ON plugin_activity(delivery_id,id);
CREATE TABLE plugin_timers(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36),
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 revision TEXT NOT NULL CHECK(length(revision)>0),
 name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 64),
 payload TEXT NOT NULL CHECK(json_valid(payload) AND length(CAST(payload AS BLOB))<=4096),
 at INTEGER NOT NULL CHECK(at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE INDEX plugin_timers_due ON plugin_timers(instance_id,at,id);
";

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: String,
    #[serde(rename = "connectionId")]
    pub instance_id: String,
    pub user_id: String,
    pub name: String,
    pub status: RequestStatus,
    pub mentions: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestStatus {
    Pending,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub id: String,
    pub delivery_id: String,
    #[serde(rename = "connectionId")]
    pub instance_id: String,
    pub chat_id: String,
    pub user_id: String,
    pub name: String,
    pub preview: String,
    pub status: cairn_contracts::PluginActivityStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timer {
    pub id: String,
    #[serde(rename = "connectionId")]
    pub instance_id: String,
    pub revision: String,
    pub name: String,
    pub payload: Value,
    pub at: i64,
}

#[derive(Default)]
pub struct PackageState {
    pub active: Option<String>,
    pub revision: Option<String>,
    pub approvals: std::collections::BTreeMap<String, String>,
}

fn decode<T: serde::de::DeserializeOwned>(value: &str) -> rusqlite::Result<T> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn request_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Request> {
    Ok(Request {
        id: r.get(0)?,
        instance_id: r.get(1)?,
        user_id: r.get(2)?,
        name: r.get(3)?,
        status: decode(&format!("\"{}\"", r.get::<_, String>(4)?))?,
        mentions: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

fn activity_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Activity> {
    Ok(Activity {
        id: r.get(0)?,
        delivery_id: r.get(1)?,
        instance_id: r.get(2)?,
        chat_id: r.get(3)?,
        user_id: r.get(4)?,
        name: r.get(5)?,
        preview: r.get(6)?,
        status: decode(&format!("\"{}\"", r.get::<_, String>(7)?))?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

fn timer_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Timer> {
    Ok(Timer {
        id: r.get(0)?,
        instance_id: r.get(1)?,
        revision: r.get(2)?,
        name: r.get(3)?,
        payload: decode(&r.get::<_, String>(4)?)?,
        at: r.get(5)?,
    })
}

impl Db<'_> {
    pub fn plugin_package(&self, id: &str) -> Result<PackageState> {
        let mut stmt = self.0.prepare_cached("SELECT p.active_version,p.revision,a.version,a.digest FROM plugin_packages p LEFT JOIN plugin_approvals a ON a.plugin_id=p.id WHERE p.id=?1 ORDER BY a.version")?;
        let rows = stmt.query_map([id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut state = PackageState::default();
        for row in rows {
            let (active, revision, version, digest) = row?;
            state.active = active;
            state.revision = Some(revision);
            if let (Some(version), Some(digest)) = (version, digest) {
                state.approvals.insert(version, digest);
            }
        }
        Ok(state)
    }

    pub fn plugin_packages(&self) -> Result<std::collections::BTreeMap<String, PackageState>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let result = Db(&tx).plugin_packages()?;
            tx.commit()?;
            return Ok(result);
        }
        let mut result = std::collections::BTreeMap::new();
        {
            let mut stmt = self.0.prepare_cached(
                "SELECT id,active_version,revision FROM plugin_packages ORDER BY id",
            )?;
            for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)))? {
                let (id, active, revision) = row?;
                result.insert(
                    id,
                    PackageState {
                        active,
                        revision,
                        approvals: std::collections::BTreeMap::new(),
                    },
                );
            }
        }
        {
            let mut stmt = self.0.prepare_cached(
                "SELECT plugin_id,version,digest FROM plugin_approvals ORDER BY plugin_id,version",
            )?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (id, version, digest) = row?;
                result
                    .get_mut(&id)
                    .expect("approval FK")
                    .approvals
                    .insert(version, digest);
            }
        }
        Ok(result)
    }

    pub fn save_plugin_version(&self, id: &str, version: &str, digest: &str) -> Result<()> {
        self.0.execute("INSERT INTO plugin_packages(id,revision,updated_at) VALUES(?1,?2,?3) ON CONFLICT(id) DO NOTHING", params![id,crate::config::id(),crate::config::now()])?;
        self.0.execute("INSERT INTO plugin_versions(plugin_id,version,digest) VALUES(?1,?2,?3) ON CONFLICT(plugin_id,version) DO UPDATE SET digest=excluded.digest", params![id,version,digest])?;
        Ok(())
    }

    pub fn activate_plugin_version(
        &self,
        id: &str,
        version: &str,
        digest: &str,
        approve: bool,
    ) -> Result<()> {
        self.save_plugin_version(id, version, digest)?;
        if approve {
            self.0.execute("INSERT INTO plugin_approvals VALUES(?1,?2,?3) ON CONFLICT(plugin_id,version) DO UPDATE SET digest=excluded.digest", params![id,version,digest])?;
        }
        self.0.execute(
            "UPDATE plugin_packages SET active_version=?2,revision=?3,updated_at=?4 WHERE id=?1",
            params![id, version, crate::config::id(), crate::config::now()],
        )?;
        Ok(())
    }

    pub fn remove_plugin_package(&self, id: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM plugin_packages WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn plugin_requests(&self) -> Result<Vec<Request>> {
        let mut stmt=self.0.prepare_cached("SELECT id,instance_id,user_id,name,status,mentions,created_at,updated_at FROM plugin_requests ORDER BY updated_at DESC,id")?;
        Ok(stmt
            .query_map([], request_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn plugin_request(&self, id: &str) -> Result<Option<Request>> {
        let mut stmt=self.0.prepare_cached("SELECT id,instance_id,user_id,name,status,mentions,created_at,updated_at FROM plugin_requests WHERE id=?1")?;
        Ok(stmt.query_row([id], request_row).optional()?)
    }

    pub fn save_plugin_request(&self, request: &Request) -> Result<()> {
        self.0.execute("INSERT INTO plugin_requests VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET name=excluded.name,status=excluded.status,mentions=excluded.mentions,updated_at=excluded.updated_at", params![request.id,request.instance_id,request.user_id,request.name,match request.status { RequestStatus::Pending => "pending",RequestStatus::Denied => "denied" },request.mentions,request.created_at,request.updated_at])?;
        Ok(())
    }

    pub fn remove_plugin_request(&self, id: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM plugin_requests WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn plugin_activity(&self) -> Result<Vec<Activity>> {
        let mut stmt = self.0.prepare_cached("SELECT id,delivery_id,instance_id,chat_id,user_id,name,preview,status,created_at,updated_at FROM plugin_activity ORDER BY created_at DESC,id")?;
        Ok(stmt
            .query_map([], activity_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn plugin_activity_item(&self, id: &str) -> Result<Option<Activity>> {
        let mut stmt=self.0.prepare_cached("SELECT id,delivery_id,instance_id,chat_id,user_id,name,preview,status,created_at,updated_at FROM plugin_activity WHERE id=?1")?;
        Ok(stmt.query_row([id], activity_row).optional()?)
    }

    pub fn save_plugin_activity(&self, activity: &Activity) -> Result<()> {
        let status = serde_json::to_value(activity.status)?;
        self.0.execute("INSERT INTO plugin_activity VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(id) DO UPDATE SET status=excluded.status,updated_at=excluded.updated_at",params![activity.id,activity.delivery_id,activity.instance_id,activity.chat_id,activity.user_id,activity.name,activity.preview,status.as_str().unwrap(),activity.created_at,activity.updated_at])?;
        self.0.execute("DELETE FROM plugin_activity WHERE instance_id=?1 AND id NOT IN (SELECT id FROM plugin_activity WHERE instance_id=?1 ORDER BY created_at DESC,id LIMIT 30)",[&activity.instance_id])?;
        Ok(())
    }

    pub fn finish_plugin_activity(
        &self,
        delivery: &str,
        status: cairn_contracts::PluginActivityStatus,
    ) -> Result<()> {
        self.0.execute(
            "UPDATE plugin_activity SET status=?2,updated_at=?3 WHERE delivery_id=?1",
            params![
                delivery,
                serde_json::to_value(status)?.as_str().unwrap(),
                crate::config::now()
            ],
        )?;
        Ok(())
    }

    pub fn plugin_timers(&self, instance: &str, due: i64) -> Result<Vec<Timer>> {
        let mut stmt = self.0.prepare_cached("SELECT id,instance_id,revision,name,payload,at FROM plugin_timers WHERE instance_id=?1 AND at<=?2 ORDER BY at,id")?;
        Ok(stmt
            .query_map(params![instance, due], timer_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn plugin_timer(&self, id: &str) -> Result<Option<Timer>> {
        let mut stmt = self.0.prepare_cached(
            "SELECT id,instance_id,revision,name,payload,at FROM plugin_timers WHERE id=?1",
        )?;
        Ok(stmt.query_row([id], timer_row).optional()?)
    }

    pub fn save_plugin_timer(&self, timer: &Timer) -> Result<()> {
        self.0.execute(
            "INSERT INTO plugin_timers VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                timer.id,
                timer.instance_id,
                timer.revision,
                timer.name,
                serde_json::to_string(&timer.payload)?,
                timer.at
            ],
        )?;
        Ok(())
    }

    pub fn remove_plugin_timer(&self, id: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM plugin_timers WHERE id=?1", [id])?;
        Ok(())
    }
}

impl Store {
    pub async fn plugin_requests(&self) -> Result<Vec<Request>> {
        self.read(|db| db.plugin_requests()).await
    }

    pub async fn plugin_activity(&self) -> Result<Vec<Activity>> {
        self.read(|db| db.plugin_activity()).await
    }
}
