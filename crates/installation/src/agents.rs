//! Typed profiles. Permission relations are kept in their current JSON representation
//! until A3; adapters here serialize typed profiles only at unconverted domain edges.
use crate::{
    config::{id, now},
    error::Result,
    store::{Db, Store},
};
pub use cairn_contracts::{Agent, AgentId, AgentSettings};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

pub(crate) const SCHEMA: &str = "
CREATE TABLE agents(
 id TEXT PRIMARY KEY NOT NULL,
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
 description TEXT NOT NULL CHECK(length(description)<=500),
 model TEXT NOT NULL CHECK(length(model)<=100),
 reasoning TEXT NOT NULL CHECK(length(reasoning)<=40),
 speed TEXT NOT NULL CHECK(length(speed)<=40),
 instructions TEXT NOT NULL CHECK(length(instructions)<=20000),
 timeout_minutes INTEGER NOT NULL CHECK(timeout_minutes BETWEEN 0 AND 720),
 access TEXT NOT NULL,
 exclusive_nodes TEXT NOT NULL,
 plugins TEXT NOT NULL,
 revision TEXT NOT NULL CHECK(length(revision)>0),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL,
 avatar_status TEXT CHECK(avatar_status IN ('generating','ready','failed')),
 avatar_revision TEXT,
 avatar_url TEXT,
 avatar_error TEXT,
 CHECK((avatar_status IS NULL AND avatar_revision IS NULL AND avatar_url IS NULL AND avatar_error IS NULL) OR (avatar_status IS NOT NULL AND avatar_revision IS NOT NULL))
);
CREATE INDEX agents_updated ON agents(updated_at DESC,id);
";

const COLUMNS: &str = "id,name,provider,description,model,reasoning,speed,instructions,timeout_minutes,access,exclusive_nodes,plugins,revision,created_at,updated_at,avatar_status,avatar_revision,avatar_url,avatar_error";

fn decode<T: serde::de::DeserializeOwned>(index: usize, value: &str) -> rusqlite::Result<T> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Agent> {
    let identifier: String = row.get(0)?;
    let identifier = uuid::Uuid::parse_str(&identifier).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let provider: String = row.get(2)?;
    let status: Option<String> = row.get(15)?;
    Ok(Agent {
        id: AgentId(identifier),
        settings: AgentSettings {
            name: row.get(1)?,
            provider: decode(2, &serde_json::to_string(&provider).unwrap())?,
            description: row.get(3)?,
            model: row.get(4)?,
            reasoning: row.get(5)?,
            speed: row.get(6)?,
            instructions: row.get(7)?,
            timeout_minutes: row.get(8)?,
            access: decode(9, &row.get::<_, String>(9)?)?,
            exclusive_nodes: decode(10, &row.get::<_, String>(10)?)?,
            plugins: decode(11, &row.get::<_, String>(11)?)?,
        },
        revision: row.get(12)?,
        created_at: row.get(13)?,
        avatar: match status {
            Some(status) => Some(cairn_contracts::AgentAvatar {
                status,
                revision: row.get(16)?,
                url: row.get(17)?,
                error: row.get(18)?,
            }),
            None => None,
        },
    })
}

impl Db<'_> {
    pub fn agents(&self) -> Result<Vec<Agent>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM agents ORDER BY updated_at DESC,id"
            ))?
            .query_map([], row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn agent(&self, id: &str) -> Result<Option<Agent>> {
        Ok(self
            .0
            .prepare_cached(&format!("SELECT {COLUMNS} FROM agents WHERE id=?"))?
            .query_row([id], row)
            .optional()?)
    }

    pub fn save_agent(&self, agent: &Agent) -> Result<Agent> {
        let settings = &agent.settings;
        let provider = serde_json::to_value(settings.provider)?;
        let avatar = agent.avatar.as_ref();
        self.0.prepare_cached("INSERT INTO agents VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)
ON CONFLICT(id) DO UPDATE SET name=excluded.name,provider=excluded.provider,description=excluded.description,model=excluded.model,reasoning=excluded.reasoning,speed=excluded.speed,instructions=excluded.instructions,timeout_minutes=excluded.timeout_minutes,access=excluded.access,exclusive_nodes=excluded.exclusive_nodes,plugins=excluded.plugins,revision=excluded.revision,created_at=excluded.created_at,updated_at=excluded.updated_at,avatar_status=excluded.avatar_status,avatar_revision=excluded.avatar_revision,avatar_url=excluded.avatar_url,avatar_error=excluded.avatar_error")?.execute(params![
 agent.id.to_string(), settings.name, provider.as_str().unwrap(), settings.description, settings.model, settings.reasoning, settings.speed, settings.instructions, settings.timeout_minutes,
 serde_json::to_string(&settings.access)?, serde_json::to_string(&settings.exclusive_nodes)?, serde_json::to_string(&settings.plugins)?, agent.revision, agent.created_at, now(),
 avatar.map(|a| &a.status), avatar.map(|a| &a.revision), avatar.and_then(|a| a.url.as_ref()), avatar.and_then(|a| a.error.as_ref())
 ])?;
        Ok(agent.clone())
    }

    pub fn remove_agent(&self, id: &str) -> Result<()> {
        self.0
            .prepare_cached("DELETE FROM agents WHERE id=?")?
            .execute([id])?;
        self.audit("agents.deleted", &serde_json::json!({ "id": id }))
    }

    /// A3/unconverted run, plugin, native-admission and node boundaries only.
    pub fn agent_snapshot(&self, id: &str) -> Result<Option<Value>> {
        self.agent(id)?
            .map(serde_json::to_value)
            .transpose()
            .map_err(Into::into)
    }

    pub fn agent_snapshots(&self) -> Result<Vec<Value>> {
        self.agents()?
            .into_iter()
            .map(|agent| Ok(serde_json::to_value(agent)?))
            .collect()
    }

    /// A3 mutates the existing permission representation. Always decode back into
    /// the authoritative profile before writing; no generic records writer remains.
    pub fn save_agent_permissions(&self, value: &Value) -> Result<Value> {
        let agent: Agent = serde_json::from_value(value.clone())?;
        Ok(serde_json::to_value(self.save_agent(&agent)?)?)
    }
}

impl Store {
    pub async fn agents(&self) -> Result<Vec<Agent>> {
        self.read(|db| db.agents()).await
    }

    pub async fn agent(&self, id: &str) -> Result<Option<Agent>> {
        let id = id.to_owned();
        self.read(move |db| db.agent(&id)).await
    }

    pub async fn agent_snapshot(&self, id: &str) -> Result<Option<Value>> {
        let id = id.to_owned();
        self.read(move |db| db.agent_snapshot(&id)).await
    }

    pub async fn agent_snapshots(&self) -> Result<Vec<Value>> {
        self.read(|db| db.agent_snapshots()).await
    }
}

pub fn new_agent(settings: AgentSettings, agent_id: Option<AgentId>) -> Agent {
    Agent {
        id: agent_id.unwrap_or_else(|| AgentId(uuid::Uuid::new_v4())),
        settings,
        revision: id(),
        created_at: now(),
        avatar: None,
    }
}

/// Exactly one complete create or revision-guarded replacement.
pub(crate) enum SaveProfile {
    Create(cairn_contracts::CreateAgentInput),
    Replace(cairn_contracts::ReplaceAgentInput),
}
