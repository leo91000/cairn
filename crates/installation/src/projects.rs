//! Project lifecycle. SQL is the only current source; JSON adapters belong to
//! callers whose domains have not yet converted (A2–A18).
use crate::{
    config::{id, now},
    error::{Error, Result, required},
    service::Service,
    store::{Db, Store},
};
pub use cairn_contracts::{
    CreateProjectInput, Project, ProjectId, ProjectSettings, ProjectSource, ReplaceProjectInput,
};
use rusqlite::{OptionalExtension, params};

pub(crate) const SCHEMA: &str = "
CREATE TABLE projects(
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
    path TEXT NOT NULL CHECK(length(path) BETWEEN 1 AND 2000),
    source_mode TEXT NOT NULL CHECK(source_mode IN ('remote','local')),
    base_branch TEXT NOT NULL CHECK(length(base_branch)>0),
    description TEXT NOT NULL CHECK(length(description)<=500),
    origin TEXT NOT NULL,
    source_plugin_id TEXT,
    source_name TEXT,
    source_instance_id TEXT,
    source_item_id TEXT,
    revision TEXT NOT NULL CHECK(length(revision)>0),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL,
    CHECK((source_plugin_id IS NULL AND source_name IS NULL AND source_instance_id IS NULL AND source_item_id IS NULL)
       OR (source_plugin_id IS NOT NULL AND source_name IS NOT NULL AND source_instance_id IS NOT NULL AND source_item_id IS NOT NULL))
);
CREATE INDEX projects_updated ON projects(updated_at DESC,id);
CREATE INDEX projects_source ON projects(source_plugin_id,source_item_id);
";

const COLUMNS: &str = "id,name,path,source_mode,base_branch,description,origin,source_plugin_id,source_name,source_instance_id,source_item_id,revision,created_at";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let id: String = row.get(0)?;
    let parsed = uuid::Uuid::parse_str(&id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let plugin_id: Option<String> = row.get(7)?;
    Ok(Project {
        id: ProjectId(parsed),
        settings: ProjectSettings {
            name: row.get(1)?,
            path: row.get(2)?,
            source_mode: row.get(3)?,
            base_branch: row.get(4)?,
            description: row.get(5)?,
        },
        origin: row.get(6)?,
        source: match plugin_id {
            Some(plugin_id) => Some(ProjectSource {
                plugin_id,
                source: row.get(8)?,
                instance: row.get(9)?,
                id: row.get(10)?,
            }),
            None => None,
        },
        revision: row.get(11)?,
        created_at: row.get(12)?,
    })
}

impl Db<'_> {
    pub fn projects(&self) -> Result<Vec<Project>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM projects ORDER BY updated_at DESC,id"
            ))?
            .query_map([], row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn project(&self, id: &str) -> Result<Option<Project>> {
        Ok(self
            .0
            .prepare_cached(&format!("SELECT {COLUMNS} FROM projects WHERE id=?"))?
            .query_row([id], row)
            .optional()?)
    }

    /// One writer for creates and full replacements. Call inside the owning transaction.
    pub fn save_project(&self, project: &Project, expected: Option<&str>) -> Result<Project> {
        let source = project.source.as_ref();
        let settings = &project.settings;
        let values = params![
            project.id.to_string(),
            settings.name,
            settings.path,
            settings.source_mode,
            settings.base_branch,
            settings.description,
            project.origin,
            source.map(|s| &s.plugin_id),
            source.map(|s| &s.source),
            source.map(|s| &s.instance),
            source.map(|s| &s.id),
            project.revision,
            project.created_at,
            now()
        ];
        if let Some(expected) = expected {
            let current = required(
                self.project(&project.id.to_string())?,
                "Project no longer exists.",
            )?;
            if current.revision != expected {
                return Err(Error::conflict(
                    "This project was changed elsewhere. Reload it before saving.",
                ));
            }
            self.0.prepare_cached("UPDATE projects SET name=?2,path=?3,source_mode=?4,base_branch=?5,description=?6,origin=?7,source_plugin_id=?8,source_name=?9,source_instance_id=?10,source_item_id=?11,revision=?12,created_at=?13,updated_at=?14 WHERE id=?1")?.execute(values)?;
        } else {
            self.0
                .prepare_cached(
                    "INSERT INTO projects VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                )?
                .execute(values)?;
        }
        self.audit("project.saved", &serde_json::json!({ "id": project.id }))?;
        Ok(project.clone())
    }

    pub(crate) fn delete_project(&self, id: &str) -> Result<()> {
        required(self.project(id)?, "Project not found")?;
        crate::service::ensure_removable(self, "projects", id)?;
        self.0
            .prepare_cached("DELETE FROM projects WHERE id=?")?
            .execute([id])?;
        self.audit("projects.deleted", &serde_json::json!({ "id": id }))
    }
}

impl Store {
    pub async fn projects(&self) -> Result<Vec<Project>> {
        self.read(|db| db.projects()).await
    }

    pub async fn project(&self, id: &str) -> Result<Option<Project>> {
        let id = id.to_owned();
        self.read(move |db| db.project(&id)).await
    }
}

impl Service {
    pub async fn get_project(&self, id: &str) -> Result<Project> {
        required(self.store.project(id).await?, "Project not found")
    }

    pub async fn create_project<F>(
        &self,
        input: CreateProjectInput,
        source: Option<ProjectSource>,
        authorize: F,
    ) -> Result<Project>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        self.create_project_live(
            input,
            source,
            crate::authorization::Authorization::once(authorize),
        )
        .await
    }

    pub(crate) async fn create_project_live(
        &self,
        mut input: CreateProjectInput,
        source: Option<ProjectSource>,
        authorize: crate::authorization::Authorization,
    ) -> Result<Project> {
        authorize.recheck().await?;
        if input
            .creation_id
            .is_some_and(|id| id.0.get_version() != Some(uuid::Version::Random))
        {
            return Err(Error::bad("creationId must be a random UUID"));
        }
        input.settings.validate().map_err(Error::bad)?;
        let path = crate::skills::workspace(
            std::path::Path::new(&input.settings.path),
            &self.config.workspace_roots,
        )
        .await?;
        input.settings.path = path.to_string_lossy().into_owned();
        let project = Project {
            id: input
                .creation_id
                .unwrap_or_else(|| ProjectId(uuid::Uuid::new_v4())),
            settings: input.settings,
            origin: crate::service::normalize_origin(&crate::service::git_origin(&path).await),
            source,
            revision: id(),
            created_at: now(),
        };
        authorize.recheck().await?;
        self.store
            .transaction(move |db| {
                authorize.authorize(db)?;
                if let Some(creation_id) = input.creation_id {
                    if let Some(project) = db.project(&creation_id.to_string())? {
                        return Ok(project);
                    }
                    if db.agent(&creation_id.to_string())?.is_some()
                        || db.mission(&creation_id.to_string())?.is_some()
                    {
                        return Err(Error::conflict(
                            "This creation identity belongs to a different resource.",
                        ));
                    }
                    if db.0.query_row(
                        "SELECT EXISTS(SELECT 1 FROM records WHERE id=?)",
                        [creation_id.to_string()],
                        |row| row.get::<_, bool>(0),
                    )? {
                        return Err(Error::conflict(
                            "This creation identity belongs to a different resource.",
                        ));
                    }
                }
                db.save_project(&project, None)
            })
            .await
    }

    pub async fn replace_project<F>(
        &self,
        input: ReplaceProjectInput,
        authorize: F,
    ) -> Result<Project>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        self.replace_project_live(input, crate::authorization::Authorization::once(authorize))
            .await
    }

    pub(crate) async fn replace_project_live(
        &self,
        mut input: ReplaceProjectInput,
        authorize: crate::authorization::Authorization,
    ) -> Result<Project> {
        authorize.recheck().await?;
        input.settings.validate().map_err(Error::bad)?;
        let path = crate::skills::workspace(
            std::path::Path::new(&input.settings.path),
            &self.config.workspace_roots,
        )
        .await?;
        input.settings.path = path.to_string_lossy().into_owned();
        let origin = crate::service::normalize_origin(&crate::service::git_origin(&path).await);
        authorize.recheck().await?;
        self.store
            .transaction(move |db| {
                authorize.authorize(db)?;
                let current = required(
                    db.project(&input.project_id.to_string())?,
                    "Project no longer exists.",
                )?;
                let source = current.source.filter(|_| current.origin == origin);
                let project = Project {
                    id: input.project_id,
                    settings: input.settings,
                    origin,
                    source,
                    revision: id(),
                    created_at: current.created_at,
                };
                db.save_project(&project, Some(&input.revision))
            })
            .await
    }

    pub async fn delete_project<F>(&self, project_id: ProjectId, authorize: F) -> Result<()>
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        self.delete_project_live(
            project_id,
            crate::authorization::Authorization::once(authorize),
        )
        .await
    }

    pub(crate) async fn delete_project_live(
        &self,
        project_id: ProjectId,
        authorize: crate::authorization::Authorization,
    ) -> Result<()> {
        authorize.recheck().await?;
        self.store
            .transaction(move |db| {
                authorize.authorize(db)?;
                db.delete_project(&project_id.to_string())
            })
            .await
    }
}
