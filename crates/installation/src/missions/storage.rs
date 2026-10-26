use crate::{
    config::now,
    error::Result,
    store::{Db, Store},
};
use cairn_contracts::*;
use rusqlite::{OptionalExtension, params};

pub(crate) const SCHEMA: &str = "
CREATE TABLE missions(
 id TEXT PRIMARY KEY NOT NULL,
 agent_id TEXT NOT NULL REFERENCES agents(id),
 project_id TEXT REFERENCES projects(id),
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 prompt TEXT NOT NULL CHECK(length(trim(prompt)) BETWEEN 1 AND 50000),
 cron TEXT CHECK(cron IS NULL OR length(cron)<=100),
 timezone TEXT NOT NULL CHECK(length(timezone) BETWEEN 1 AND 100),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 archived INTEGER NOT NULL CHECK(archived IN (0,1)),
 worktree INTEGER NOT NULL CHECK(worktree IN (0,1)),
 skills_inherited INTEGER NOT NULL CHECK(skills_inherited IN (0,1)),
 next_run INTEGER CHECK(next_run BETWEEN 0 AND 9007199254740991),
 author_id TEXT,
 author_access_id TEXT,
 author_removed INTEGER NOT NULL CHECK(author_removed IN (0,1)),
 revision TEXT NOT NULL CHECK(length(revision)>0),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL,
 schedule_wait_reason TEXT,
 failure_message TEXT,
 failure_scheduled_for INTEGER CHECK(failure_scheduled_for BETWEEN 0 AND 9007199254740991),
 failure_at INTEGER CHECK(failure_at BETWEEN 0 AND 9007199254740991),
 native_principals TEXT NOT NULL CHECK(json_valid(native_principals) AND json_type(native_principals)='array'),
 native_authority_nodes TEXT NOT NULL CHECK(json_valid(native_authority_nodes) AND json_type(native_authority_nodes)='array'),
 native_parent_run_id TEXT,
 native_parent_access TEXT CHECK(native_parent_access IN ('installation','delegation')),
 native_provenance_unknown INTEGER NOT NULL CHECK(native_provenance_unknown IN (0,1)),
 CHECK((author_id IS NULL)=(author_access_id IS NULL)),
 CHECK(next_run IS NULL OR (enabled=1 AND archived=0 AND cron IS NOT NULL AND author_removed=0)),
 CHECK((failure_message IS NULL AND failure_scheduled_for IS NULL AND failure_at IS NULL)
    OR (failure_message IS NOT NULL AND failure_scheduled_for IS NOT NULL AND failure_at IS NOT NULL))
);
CREATE TRIGGER missions_author_immutable BEFORE UPDATE ON missions
 WHEN OLD.author_id IS NOT NEW.author_id OR OLD.author_access_id IS NOT NEW.author_access_id
 BEGIN SELECT RAISE(ABORT,'Mission authorship is immutable'); END;
CREATE INDEX missions_updated ON missions(updated_at DESC,id);
CREATE INDEX missions_due ON missions(next_run,id) WHERE enabled=1 AND archived=0 AND cron IS NOT NULL;
CREATE INDEX missions_agent ON missions(agent_id);
CREATE INDEX missions_project ON missions(project_id) WHERE project_id IS NOT NULL;
CREATE TABLE mission_tags(
 mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 9),
 tag TEXT NOT NULL CHECK(length(tag)<=30),
 PRIMARY KEY(mission_id,position)
) WITHOUT ROWID;
CREATE TABLE mission_skills(
 mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 99),
 skill TEXT NOT NULL CHECK(length(skill)<=160),
 PRIMARY KEY(mission_id,position)
) WITHOUT ROWID;
";

const COLUMNS: &str = "id,agent_id,project_id,name,prompt,cron,timezone,enabled,archived,worktree,skills_inherited,next_run,author_id,author_access_id,author_removed,revision,created_at,schedule_wait_reason,failure_message,failure_scheduled_for,failure_at,native_principals,native_authority_nodes,native_parent_run_id,native_parent_access,native_provenance_unknown";

fn decode<T: std::str::FromStr>(r: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let s: String = r.get(index)?;
    s.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn strings(r: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Vec<String>> {
    let s: String = r.get(index)?;
    serde_json::from_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Mission> {
    let project: Option<String> = r.get(2)?;
    let project_id = project
        .map(|s| uuid::Uuid::parse_str(&s).map(ProjectId))
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?;
    let failure: Option<String> = r.get(18)?;
    Ok(Mission {
        id: MissionId(decode(r, 0)?),
        settings: MissionSettings {
            agent_id: AgentId(decode(r, 1)?),
            project_id,
            name: r.get(3)?,
            prompt: r.get(4)?,
            cron: r.get(5)?,
            timezone: r.get(6)?,
            enabled: r.get(7)?,
            archived: r.get(8)?,
            worktree: r.get(9)?,
            skills: if r.get::<_, bool>(10)? {
                None
            } else {
                Some(Vec::new())
            },
            tags: Vec::new(),
        },
        next_run: r.get(11)?,
        author_id: r.get(12)?,
        author_access_id: r.get(13)?,
        author_removed: r.get(14)?,
        revision: r.get(15)?,
        created_at: r.get(16)?,
        schedule_wait_reason: r.get(17)?,
        schedule_failure: match failure {
            Some(message) => Some(ScheduleFailure {
                message,
                scheduled_for: r.get(19)?,
                at: r.get(20)?,
            }),
            None => None,
        },
        native_principals: strings(r, 21)?,
        native_authority_nodes: strings(r, 22)?,
        native_parent_run_id: r.get(23)?,
        native_parent_access: r.get(24)?,
        native_provenance_unknown: r.get(25)?,
    })
}

impl Db<'_> {
    fn mission_relations(&self, mission: &mut Mission) -> Result<()> {
        let id = mission.id.to_string();
        mission.settings.tags = self
            .0
            .prepare_cached("SELECT tag FROM mission_tags WHERE mission_id=? ORDER BY position")?
            .query_map([&id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if let Some(skills) = &mut mission.settings.skills {
            *skills = self
                .0
                .prepare_cached(
                    "SELECT skill FROM mission_skills WHERE mission_id=? ORDER BY position",
                )?
                .query_map([&id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
        }
        Ok(())
    }

    pub fn mission(&self, id: &str) -> Result<Option<Mission>> {
        let mut value = self
            .0
            .prepare_cached(&format!("SELECT {COLUMNS} FROM missions WHERE id=?"))?
            .query_row([id], row)
            .optional()?;
        if let Some(mission) = &mut value {
            self.mission_relations(mission)?;
        }
        Ok(value)
    }

    pub fn missions(&self) -> Result<Vec<Mission>> {
        self.query_missions("ORDER BY updated_at DESC,id", None)
    }

    pub fn due_missions(&self, at: i64) -> Result<Vec<Mission>> {
        self.query_missions("WHERE enabled=1 AND archived=0 AND cron IS NOT NULL AND next_run<=? ORDER BY next_run,id", Some(at))
    }

    fn query_missions(&self, suffix: &str, at: Option<i64>) -> Result<Vec<Mission>> {
        let mut stmt = self
            .0
            .prepare_cached(&format!("SELECT {COLUMNS} FROM missions {suffix}"))?;
        let mut values: Vec<Mission> = if let Some(at) = at {
            stmt.query_map([at], row)?
                .collect::<rusqlite::Result<_>>()?
        } else {
            stmt.query_map([], row)?.collect::<rusqlite::Result<_>>()?
        };
        for value in &mut values {
            self.mission_relations(value)?;
        }
        Ok(values)
    }

    /// Owning transaction supplies revision/authority checks. No generic records writer exists.
    pub fn save_mission(&self, m: &Mission) -> Result<()> {
        let s = &m.settings;
        let f = m.schedule_failure.as_ref();
        self.0.prepare_cached("INSERT INTO missions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)
   ON CONFLICT(id) DO UPDATE SET agent_id=excluded.agent_id,project_id=excluded.project_id,name=excluded.name,prompt=excluded.prompt,cron=excluded.cron,timezone=excluded.timezone,enabled=excluded.enabled,archived=excluded.archived,worktree=excluded.worktree,skills_inherited=excluded.skills_inherited,next_run=excluded.next_run,author_id=excluded.author_id,author_access_id=excluded.author_access_id,author_removed=excluded.author_removed,revision=excluded.revision,created_at=excluded.created_at,updated_at=excluded.updated_at,schedule_wait_reason=excluded.schedule_wait_reason,failure_message=excluded.failure_message,failure_scheduled_for=excluded.failure_scheduled_for,failure_at=excluded.failure_at,native_principals=excluded.native_principals,native_authority_nodes=excluded.native_authority_nodes,native_parent_run_id=excluded.native_parent_run_id,native_parent_access=excluded.native_parent_access,native_provenance_unknown=excluded.native_provenance_unknown")?.execute(params![
   m.id.to_string(), s.agent_id.to_string(), s.project_id.map(|id| id.to_string()), s.name,s.prompt,s.cron,s.timezone,s.enabled,s.archived,s.worktree,s.skills.is_none(),m.next_run,m.author_id,m.author_access_id,m.author_removed,m.revision,m.created_at,now(),m.schedule_wait_reason,f.map(|f| &f.message),f.map(|f| f.scheduled_for),f.map(|f| f.at),serde_json::to_string(&m.native_principals)?,serde_json::to_string(&m.native_authority_nodes)?,m.native_parent_run_id,m.native_parent_access,m.native_provenance_unknown
  ])?;
        let id = m.id.to_string();
        self.0
            .prepare_cached("DELETE FROM mission_tags WHERE mission_id=?")?
            .execute([&id])?;
        self.0
            .prepare_cached("DELETE FROM mission_skills WHERE mission_id=?")?
            .execute([&id])?;
        for (position, tag) in s.tags.iter().enumerate() {
            self.0
                .prepare_cached("INSERT INTO mission_tags VALUES(?,?,?)")?
                .execute(params![id, position as i64, tag])?;
        }
        for (position, skill) in s.skills.iter().flatten().enumerate() {
            self.0
                .prepare_cached("INSERT INTO mission_skills VALUES(?,?,?)")?
                .execute(params![id, position as i64, skill])?;
        }
        Ok(())
    }

    pub fn delete_mission(&self, id: &str) -> Result<()> {
        crate::error::required(self.mission(id)?, "Mission not found")?;
        crate::service::ensure_removable(self, "tasks", id)?;
        self.0
            .prepare_cached("DELETE FROM missions WHERE id=?")?
            .execute([id])?;
        self.audit("tasks.deleted", &serde_json::json!({ "id": id }))
    }
}

impl Store {
    pub async fn mission(&self, id: &str) -> Result<Option<Mission>> {
        let id = id.to_owned();
        self.read(move |db| {
            let tx = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let value = Db(&tx).mission(&id)?;
            tx.commit()?;
            Ok(value)
        })
        .await
    }

    pub async fn missions(&self) -> Result<Vec<Mission>> {
        self.read(|db| {
            let tx = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let values = Db(&tx).missions()?;
            tx.commit()?;
            Ok(values)
        })
        .await
    }

    pub async fn due_missions(&self, at: i64) -> Result<Vec<Mission>> {
        self.read(move |db| {
            let tx = rusqlite::Transaction::new_unchecked(
                db.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let values = Db(&tx).due_missions(at)?;
            tx.commit()?;
            Ok(values)
        })
        .await
    }
}
