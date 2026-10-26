//! Live grants an agent holds on resources: exclusive reservations of nodes and
//! plugin instances such as 1Password service accounts.
//!
//! All current permissions live in SQL. The unconverted node/plugin wire views
//! project their agent lists from these relations; they never persist a copy.
//!
//! These grants are live, outside a run's access snapshot. Changing them does not
//! broaden its frozen policy. Delegation reapproval continues to compare the
//! approved access policy, independently of these live grants (ADR-0068).

use crate::{
    error::{Error, Result, required},
    plugins::connections::KIND as PLUGIN_INSTANCES,
    store::Db,
    validation::text,
};
use serde_json::{Value, json};

pub const EXCLUSIVE_NODES: &str = "exclusiveNodes";
pub const PLUGINS: &str = "plugins";

fn ids(value: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for id in value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !ids.iter().any(|known| known == id) {
            ids.push(id.to_owned());
        }
    }
    ids
}

/// Nodes on which the agent may suspend other conversations.
pub fn exclusive_nodes(agent: &Value) -> Vec<String> {
    ids(&agent[EXCLUSIVE_NODES])
}

/// Plugin instances whose commands, tools and instructions the agent receives.
pub fn plugins(agent: &Value) -> Vec<String> {
    ids(&agent[PLUGINS])
}

/// Validate targets in the publication transaction, including references whose
/// domain still lives in records. Projects additionally have an SQL foreign key.
pub(crate) fn validate(
    db: &Db<'_>,
    agent: &cairn_contracts::Agent,
    accepts_agents: &[String],
) -> Result<()> {
    let current = db.agent(&agent.id.to_string())?;
    let held = current
        .as_ref()
        .map(|saved| saved.settings.plugins.as_slice())
        .unwrap_or_default();
    let settings = &agent.settings;
    for project in settings.access.projects.as_ref().into_iter().flatten() {
        required(db.project(project)?, "Project no longer exists")?;
    }
    for connection in settings
        .access
        .mcps
        .as_ref()
        .into_iter()
        .flatten()
        .chain(settings.access.mcp_tools.keys())
    {
        required(
            db.get("mcps", connection)?,
            "MCP connection no longer exists",
        )?;
    }
    for connection in settings.access.mcp_tools.keys() {
        if settings
            .access
            .mcps
            .as_ref()
            .is_some_and(|connections| !connections.contains(connection))
        {
            return Err(Error::bad(
                "Tool permissions require access to the MCP connection.",
            ));
        }
    }
    for node in &settings.exclusive_nodes {
        let record = db.get("nodes", node)?.ok_or_else(|| {
            Error::bad("A node selected for exclusive reservations no longer exists.")
        })?;
        if record["revoked"] == true {
            return Err(Error::bad("A revoked node cannot be reserved."));
        }
        if settings.access.backend == "native" || crate::native::policy::node_native(&record) {
            return Err(Error::bad(
                "Exclusive reservations need a Firecracker VM node used by a Firecracker agent.",
            ));
        }
    }
    for instance in &settings.plugins {
        db.get(PLUGIN_INSTANCES, instance)?
            .ok_or_else(|| Error::bad("A selected plugin instance no longer exists."))?;
        if !accepts_agents.contains(instance) && !held.contains(instance) {
            return Err(Error::bad(
                "A selected plugin instance does not give agents access.",
            ));
        }
    }
    Ok(())
}

/// Update the grants on typed profiles; resource-side projections are read-only.
pub(crate) fn set_instance_agents(db: &Db<'_>, instance: &str, agents: &[String]) -> Result<()> {
    for mut agent in db.agents()? {
        let granted = agents.iter().any(|id| *id == agent.id.to_string());
        let present = agent.settings.plugins.iter().any(|id| id == instance);
        if granted == present {
            continue;
        }
        if granted {
            agent.settings.plugins.push(instance.to_owned());
        } else {
            agent.settings.plugins.retain(|id| id != instance);
        }
        agent.revision = crate::config::id();
        db.save_agent(&agent)?;
    }
    Ok(())
}

/// A deleted instance or revoked node cannot remain in a current profile.
pub(crate) fn forget_resource(db: &Db<'_>, key: &str, resource: &str) -> Result<()> {
    for mut agent in db.agents()? {
        let grants = match key {
            EXCLUSIVE_NODES => &mut agent.settings.exclusive_nodes,
            PLUGINS => &mut agent.settings.plugins,
            _ => return Err(Error::bad("Unknown permission relation.")),
        };
        let before = grants.len();
        grants.retain(|id| id != resource);
        if before != grants.len() {
            agent.revision = crate::config::id();
            db.save_agent(&agent)?;
        }
    }
    Ok(())
}

pub(crate) fn save_profile(
    db: &Db<'_>,
    mut agent: cairn_contracts::Agent,
    owner_review: bool,
    native_reviews: &[String],
) -> Result<cairn_contracts::Agent> {
    let current = db.agent(&agent.id.to_string())?;
    let access = &agent.settings.access;
    // Keep previously selected nodes only while the execution backend is unchanged.
    let previous_nodes = current
        .as_ref()
        .filter(|saved| saved.settings.access.backend == access.backend)
        .and_then(|saved| saved.settings.access.nodes.as_ref());
    let held = |node: &String| previous_nodes.is_some_and(|nodes| nodes.contains(node));
    for node in access.nodes.as_ref().into_iter().flatten() {
        if node == crate::nodes::LOCAL_NODE_ID {
            if access.backend == "native" && !held(node) {
                return Err(Error::bad(
                    "Native agents need explicit native nodes: the current runner only runs Firecracker VMs.",
                ));
            }
            continue;
        }
        let record = required(db.get("nodes", node)?, "Node not found")?;
        if record["revoked"] == true {
            return Err(Error::bad("A revoked node cannot be authorized."));
        }
        if !held(node)
            && crate::native::policy::node_native(&record) != (access.backend == "native")
        {
            return Err(Error::bad(
                "Choose nodes that use this agent's execution backend.",
            ));
        }
    }
    if let Some(current) = &current {
        agent.avatar = current.avatar.clone();
    }
    // Native node admission is still owned by A10: its snapshot boundary is explicit.
    crate::nodes::native_mode::review_agent(
        db,
        &serde_json::to_value(&agent)?,
        current
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?
            .as_ref(),
        owner_review,
        native_reviews,
    )?;
    agent.revision = crate::config::id();
    db.save_agent(&agent)?;
    db.audit("agent.saved", &json!({ "id": agent.id }))?;
    Ok(agent)
}

/// Whether a current typed permission set still covers the saved run policy.
/// Placement nodes are checked separately because existing work can be moved.
pub(crate) fn covers(
    current: &cairn_contracts::AccessPolicy,
    granted: &cairn_contracts::AccessPolicy,
) -> bool {
    let scopes = [
        (&current.projects, &granted.projects),
        (&current.skills, &granted.skills),
        (&current.mcps, &granted.mcps),
    ]
    .into_iter()
    .all(|(current, granted)| selection_covers(current.as_deref(), granted.as_deref()));
    let tools = current.mcp_tools.iter().all(|(server, selection)| {
        selection_covers(
            Some(selection),
            granted.mcp_tools.get(server).map(Vec::as_slice),
        )
    });
    let rank = |sandbox: &str| match sandbox {
        "yolo" => 2,
        "workspace-write" => 1,
        _ => 0,
    };
    current.backend == granted.backend
        && scopes
        && tools
        && (current.installation || !granted.installation)
        && (current.delegation || !granted.delegation)
        && rank(&current.sandbox) >= rank(&granted.sandbox)
}

pub(crate) fn selection_covers(current: Option<&[String]>, granted: Option<&[String]>) -> bool {
    current.is_none_or(|current| {
        granted.is_some_and(|granted| granted.iter().all(|item| current.contains(item)))
    })
}

pub(crate) const SCHEMA: &str = "
CREATE TABLE agent_permissions(
 agent_id TEXT PRIMARY KEY NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 backend TEXT NOT NULL CHECK(backend IN ('firecracker','native')),
 sandbox TEXT NOT NULL CHECK(sandbox IN ('yolo','workspace-write','read-only')),
 installation INTEGER NOT NULL CHECK(installation IN (0,1)),
 delegation INTEGER NOT NULL CHECK(delegation IN (0,1)),
 all_projects INTEGER NOT NULL CHECK(all_projects IN (0,1)),
 all_skills INTEGER NOT NULL CHECK(all_skills IN (0,1)),
 all_nodes INTEGER NOT NULL CHECK(all_nodes IN (0,1)),
 all_mcps INTEGER NOT NULL CHECK(all_mcps IN (0,1))
) WITHOUT ROWID;
CREATE TABLE agent_projects(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,project_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE INDEX agent_projects_resource ON agent_projects(project_id,agent_id);
CREATE TABLE agent_skills(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 skill TEXT NOT NULL CHECK(length(skill)<=160),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,skill), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE TABLE agent_nodes(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 node_id TEXT NOT NULL CHECK(length(node_id)=36),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,node_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE INDEX agent_nodes_resource ON agent_nodes(node_id,agent_id);
CREATE TABLE agent_mcps(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 mcp_id TEXT NOT NULL CHECK(length(mcp_id)=36),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,mcp_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE INDEX agent_mcps_resource ON agent_mcps(mcp_id,agent_id);
CREATE TABLE agent_mcp_selections(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 mcp_id TEXT NOT NULL CHECK(length(mcp_id)=36),
 PRIMARY KEY(agent_id,mcp_id)
) WITHOUT ROWID;
CREATE TABLE agent_mcp_tools(
 agent_id TEXT NOT NULL,
 mcp_id TEXT NOT NULL,
 tool TEXT NOT NULL CHECK(length(tool) BETWEEN 1 AND 200),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,mcp_id,tool), UNIQUE(agent_id,mcp_id,position),
 FOREIGN KEY(agent_id,mcp_id) REFERENCES agent_mcp_selections(agent_id,mcp_id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE agent_plugins(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 instance_id TEXT NOT NULL CHECK(length(instance_id)=36),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,instance_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE INDEX agent_plugins_resource ON agent_plugins(instance_id,agent_id);
CREATE TABLE agent_exclusive_nodes(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 node_id TEXT NOT NULL CHECK(length(node_id)=36),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,node_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE INDEX agent_exclusive_nodes_resource ON agent_exclusive_nodes(node_id,agent_id);
";

pub(crate) struct Permissions {
    pub access: cairn_contracts::AccessPolicy,
    pub exclusive_nodes: Vec<String>,
    pub plugins: Vec<String>,
}

fn permission_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, Permissions)> {
    Ok((
        row.get(0)?,
        Permissions {
            access: cairn_contracts::AccessPolicy {
                backend: row.get(1)?,
                sandbox: row.get(2)?,
                installation: row.get(3)?,
                delegation: row.get(4)?,
                projects: if row.get::<_, bool>(5)? {
                    None
                } else {
                    Some(Vec::new())
                },
                skills: if row.get::<_, bool>(6)? {
                    None
                } else {
                    Some(Vec::new())
                },
                nodes: if row.get::<_, bool>(7)? {
                    None
                } else {
                    Some(Vec::new())
                },
                mcps: if row.get::<_, bool>(8)? {
                    None
                } else {
                    Some(Vec::new())
                },
                mcp_tools: std::collections::BTreeMap::new(),
            },
            exclusive_nodes: Vec::new(),
            plugins: Vec::new(),
        },
    ))
}

const PERMISSION_COLUMNS: &str =
    "agent_id,backend,sandbox,installation,delegation,all_projects,all_skills,all_nodes,all_mcps";

impl Db<'_> {
    pub(crate) fn agent_permissions(&self, id: &str) -> rusqlite::Result<Permissions> {
        let (_, mut permissions) = self
            .0
            .prepare_cached(&format!(
                "SELECT {PERMISSION_COLUMNS} FROM agent_permissions WHERE agent_id=?"
            ))?
            .query_row([id], permission_row)?;
        let access = &mut permissions.access;
        for (sql, selection) in [
            (
                "SELECT project_id FROM agent_projects WHERE agent_id=? ORDER BY position",
                &mut access.projects,
            ),
            (
                "SELECT skill FROM agent_skills WHERE agent_id=? ORDER BY position",
                &mut access.skills,
            ),
            (
                "SELECT node_id FROM agent_nodes WHERE agent_id=? ORDER BY position",
                &mut access.nodes,
            ),
            (
                "SELECT mcp_id FROM agent_mcps WHERE agent_id=? ORDER BY position",
                &mut access.mcps,
            ),
        ] {
            if let Some(selection) = selection {
                *selection = self
                    .0
                    .prepare_cached(sql)?
                    .query_map([id], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
            }
        }
        let connections = self
            .0
            .prepare_cached(
                "SELECT mcp_id FROM agent_mcp_selections WHERE agent_id=? ORDER BY mcp_id",
            )?
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for connection in connections {
            let tools = self.0.prepare_cached("SELECT tool FROM agent_mcp_tools WHERE agent_id=? AND mcp_id=? ORDER BY position")?.query_map(rusqlite::params![id, connection], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
            access.mcp_tools.insert(connection, tools);
        }
        let exclusive = self
            .0
            .prepare_cached(
                "SELECT node_id FROM agent_exclusive_nodes WHERE agent_id=? ORDER BY position",
            )?
            .query_map([id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let plugins = self
            .0
            .prepare_cached(
                "SELECT instance_id FROM agent_plugins WHERE agent_id=? ORDER BY position",
            )?
            .query_map([id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        permissions.exclusive_nodes = exclusive;
        permissions.plugins = plugins;
        Ok(permissions)
    }

    /// The catalogue reads each relation once under its caller's SQLite snapshot,
    /// rather than issuing one set of queries for every agent in the list.
    pub(crate) fn agent_permission_sets(
        &self,
    ) -> rusqlite::Result<std::collections::BTreeMap<String, Permissions>> {
        let mut permissions = self
            .0
            .prepare_cached(&format!(
                "SELECT {PERMISSION_COLUMNS} FROM agent_permissions"
            ))?
            .query_map([], permission_row)?
            .collect::<rusqlite::Result<std::collections::BTreeMap<_, _>>>()?;
        type Selection = fn(&mut Permissions) -> Option<&mut Vec<String>>;
        let selections: [(&str, Selection); 6] = [
            (
                "SELECT agent_id,project_id FROM agent_projects ORDER BY agent_id,position",
                |p| p.access.projects.as_mut(),
            ),
            (
                "SELECT agent_id,skill FROM agent_skills ORDER BY agent_id,position",
                |p| p.access.skills.as_mut(),
            ),
            (
                "SELECT agent_id,node_id FROM agent_nodes ORDER BY agent_id,position",
                |p| p.access.nodes.as_mut(),
            ),
            (
                "SELECT agent_id,mcp_id FROM agent_mcps ORDER BY agent_id,position",
                |p| p.access.mcps.as_mut(),
            ),
            (
                "SELECT agent_id,instance_id FROM agent_plugins ORDER BY agent_id,position",
                |p| Some(&mut p.plugins),
            ),
            (
                "SELECT agent_id,node_id FROM agent_exclusive_nodes ORDER BY agent_id,position",
                |p| Some(&mut p.exclusive_nodes),
            ),
        ];
        for (sql, selection) in selections {
            let mut statement = self.0.prepare_cached(sql)?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (agent, resource) = row?;
                let profile = permissions
                    .get_mut(&agent)
                    .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
                if let Some(values) = selection(profile) {
                    values.push(resource);
                }
            }
        }
        let mut statement = self.0.prepare_cached(
            "SELECT agent_id,mcp_id FROM agent_mcp_selections ORDER BY agent_id,mcp_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (agent, connection) = row?;
            permissions
                .get_mut(&agent)
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?
                .access
                .mcp_tools
                .insert(connection, Vec::new());
        }
        let mut statement = self.0.prepare_cached(
            "SELECT agent_id,mcp_id,tool FROM agent_mcp_tools ORDER BY agent_id,mcp_id,position",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (agent, connection, tool) = row?;
            permissions
                .get_mut(&agent)
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?
                .access
                .mcp_tools
                .get_mut(&connection)
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?
                .push(tool);
        }
        Ok(permissions)
    }

    pub(crate) fn save_permissions(&self, agent: &cairn_contracts::Agent) -> Result<()> {
        let id = agent.id.to_string();
        let access = &agent.settings.access;
        self.0.prepare_cached("INSERT INTO agent_permissions VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(agent_id) DO UPDATE SET backend=excluded.backend,sandbox=excluded.sandbox,installation=excluded.installation,delegation=excluded.delegation,all_projects=excluded.all_projects,all_skills=excluded.all_skills,all_nodes=excluded.all_nodes,all_mcps=excluded.all_mcps")?.execute(rusqlite::params![id,access.backend,access.sandbox,access.installation,access.delegation,access.projects.is_none(),access.skills.is_none(),access.nodes.is_none(),access.mcps.is_none()])?;
        for (delete, insert, selection) in [
            (
                "DELETE FROM agent_projects WHERE agent_id=?",
                "INSERT INTO agent_projects VALUES(?,?,?)",
                access.projects.as_deref().unwrap_or_default(),
            ),
            (
                "DELETE FROM agent_skills WHERE agent_id=?",
                "INSERT INTO agent_skills VALUES(?,?,?)",
                access.skills.as_deref().unwrap_or_default(),
            ),
            (
                "DELETE FROM agent_nodes WHERE agent_id=?",
                "INSERT INTO agent_nodes VALUES(?,?,?)",
                access.nodes.as_deref().unwrap_or_default(),
            ),
            (
                "DELETE FROM agent_mcps WHERE agent_id=?",
                "INSERT INTO agent_mcps VALUES(?,?,?)",
                access.mcps.as_deref().unwrap_or_default(),
            ),
            (
                "DELETE FROM agent_plugins WHERE agent_id=?",
                "INSERT INTO agent_plugins VALUES(?,?,?)",
                &agent.settings.plugins,
            ),
            (
                "DELETE FROM agent_exclusive_nodes WHERE agent_id=?",
                "INSERT INTO agent_exclusive_nodes VALUES(?,?,?)",
                &agent.settings.exclusive_nodes,
            ),
        ] {
            self.0.prepare_cached(delete)?.execute([&id])?;
            let mut statement = self.0.prepare_cached(insert)?;
            for (position, value) in selection.iter().enumerate() {
                statement.execute(rusqlite::params![id, value, position as i64])?;
            }
        }
        self.0
            .prepare_cached("DELETE FROM agent_mcp_selections WHERE agent_id=?")?
            .execute([&id])?;
        for (connection, tools) in &access.mcp_tools {
            self.0
                .prepare_cached("INSERT INTO agent_mcp_selections VALUES(?,?)")?
                .execute(rusqlite::params![id, connection])?;
            for (position, tool) in tools.iter().enumerate() {
                self.0
                    .prepare_cached("INSERT INTO agent_mcp_tools VALUES(?,?,?,?)")?
                    .execute(rusqlite::params![id, connection, tool, position as i64])?;
            }
        }
        Ok(())
    }

    /// Projection for domains awaiting A9/A12. Persisted resource JSON is never an authority.
    pub(crate) fn resource_grants(&self, kind: &str, value: &mut Value) -> Result<()> {
        let (sql, field) = match kind {
            "nodes" => (
                "SELECT agent_id FROM agent_exclusive_nodes WHERE node_id=? ORDER BY agent_id",
                "exclusiveAgents",
            ),
            PLUGIN_INSTANCES => (
                "SELECT agent_id FROM agent_plugins WHERE instance_id=? ORDER BY agent_id",
                "agentIds",
            ),
            _ => return Ok(()),
        };
        let agents = self
            .0
            .prepare_cached(sql)?
            .query_map([text(value, "id")], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        value[field] = json!(agents);
        Ok(())
    }
}
