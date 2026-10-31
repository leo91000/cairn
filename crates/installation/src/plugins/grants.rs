//! Live contribution grants. Legacy `plugins` profiles project full access only;
//! selected contributions and credential suppliers are independent relations.
use crate::{
    error::{Error, Result, required},
    store::Db,
};
use cairn_contracts::{
    AgentPluginGrants, PluginContributionGrant, PluginContributionKind as Kind,
    PluginGrantAccess as Access, PluginInstanceGrant, PluginInstanceId, PluginResourceRef,
};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS plugin_contribution_grants(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('tool','command','hook','runCredentials','instructions','condition','projectSource','skillSource')),
 name TEXT NOT NULL CHECK(length(name)<=200),
 PRIMARY KEY(agent_id,instance_id,kind,name)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS plugin_contribution_instances ON plugin_contribution_grants(instance_id,agent_id);
CREATE TABLE IF NOT EXISTS plugin_grant_resources(
 agent_id TEXT NOT NULL,
 instance_id TEXT NOT NULL,
 kind TEXT NOT NULL,
 name TEXT NOT NULL,
 resource_type TEXT NOT NULL CHECK(length(resource_type) BETWEEN 1 AND 40),
 resource_id TEXT NOT NULL CHECK(length(resource_id) BETWEEN 1 AND 200),
 PRIMARY KEY(agent_id,instance_id,kind,name,resource_type,resource_id),
 FOREIGN KEY(agent_id,instance_id,kind,name) REFERENCES plugin_contribution_grants(agent_id,instance_id,kind,name) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS plugin_credential_suppliers(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 plugin_id TEXT NOT NULL REFERENCES plugin_packages(id) ON DELETE CASCADE,
 instance_id TEXT REFERENCES plugin_instances(id) ON DELETE SET NULL,
 PRIMARY KEY(agent_id,plugin_id)
) WITHOUT ROWID;
";

/// Called once by the schema upgrade. Preserve exactly the old name-ordered,
/// enabled supplier; subsequent removals retain a NULL tombstone, never a fallback.
pub(crate) fn migrate(db: &Db<'_>) -> Result<()> {
    db.0.execute_batch(SCHEMA)?;
    db.0.execute_batch("INSERT OR IGNORE INTO plugin_credential_suppliers(agent_id,plugin_id,instance_id)
SELECT g.agent_id,i.plugin_id,i.id FROM instance_grants g JOIN plugin_instances i ON i.id=g.instance_id
WHERE i.enabled=1 ORDER BY i.name,i.id;")?;
    Ok(())
}

impl Db<'_> {
    pub fn agent_plugin_grants(&self, agent: &str) -> Result<AgentPluginGrants> {
        let profile = required(self.agent(agent)?, "Agent not found")?;
        let mut grants = BTreeMap::<String, Access>::new();
        for id in &profile.settings.plugins {
            grants.insert(id.clone(), Access::Full);
        }
        let mut statement = self.0.prepare_cached("SELECT instance_id,kind,name FROM plugin_contribution_grants WHERE agent_id=? ORDER BY instance_id,kind,name")?;
        let rows = statement.query_map([agent], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (instance, kind, name) = row?;
            let kind: Kind = serde_json::from_value(serde_json::Value::String(kind))?;
            let resources = self.0.prepare_cached("SELECT resource_type,resource_id FROM plugin_grant_resources WHERE agent_id=? AND instance_id=? AND kind=? AND name=? ORDER BY resource_type,resource_id")?.query_map(params![agent,instance,kind.key(),name], |r| Ok(PluginResourceRef {resource_type:r.get(0)?,id:r.get(1)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
            if let Access::Selected { contributions } =
                grants.entry(instance).or_insert_with(|| Access::Selected {
                    contributions: Vec::new(),
                })
            {
                contributions.push(PluginContributionGrant {
                    kind,
                    name,
                    resources,
                });
            }
        }
        let grants = grants
            .into_iter()
            .map(|(id, access)| {
                Ok(PluginInstanceGrant {
                    instance_id: PluginInstanceId(
                        uuid::Uuid::parse_str(&id)
                            .map_err(|_| Error::bad("Invalid saved instance ID."))?,
                    ),
                    access,
                })
            })
            .collect::<Result<_>>()?;
        let suppliers = self.0.prepare_cached("SELECT instance_id FROM plugin_credential_suppliers WHERE agent_id=? AND instance_id IS NOT NULL ORDER BY plugin_id")?.query_map([agent], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(AgentPluginGrants {
            agent_id: profile.id,
            revision: profile.revision,
            grants,
            run_credential_instances: suppliers
                .into_iter()
                .map(|id| {
                    uuid::Uuid::parse_str(&id)
                        .map(PluginInstanceId)
                        .map_err(|_| Error::bad("Invalid saved supplier ID."))
                })
                .collect::<Result<_>>()?,
        })
    }

    pub(crate) fn plugin_contribution(
        &self,
        agent: &str,
        instance: &str,
        kind: Kind,
        name: &str,
    ) -> Result<Option<PluginContributionGrant>> {
        let full = self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM instance_grants WHERE agent_id=? AND instance_id=?)",
            params![agent, instance],
            |r| r.get::<_, bool>(0),
        )?;
        if full {
            return Ok(Some(PluginContributionGrant {
                kind,
                name: name.into(),
                resources: Vec::new(),
            }));
        }
        let present = self.0.query_row("SELECT EXISTS(SELECT 1 FROM plugin_contribution_grants WHERE agent_id=? AND instance_id=? AND kind=? AND name=?)", params![agent,instance,kind.key(),name], |r| r.get::<_,bool>(0))?;
        if !present {
            return Ok(None);
        }
        let resources = self.0.prepare_cached("SELECT resource_type,resource_id FROM plugin_grant_resources WHERE agent_id=? AND instance_id=? AND kind=? AND name=? ORDER BY resource_type,resource_id")?.query_map(params![agent,instance,kind.key(),name], |r| Ok(PluginResourceRef {resource_type:r.get(0)?,id:r.get(1)?}))?.collect::<rusqlite::Result<_>>()?;
        Ok(Some(PluginContributionGrant {
            kind,
            name: name.into(),
            resources,
        }))
    }

    pub(crate) fn has_plugin_grant(&self, agent: &str, instance: &str) -> Result<bool> {
        Ok(self.0.query_row("SELECT EXISTS(SELECT 1 FROM instance_grants WHERE agent_id=?1 AND instance_id=?2 UNION ALL SELECT 1 FROM plugin_contribution_grants WHERE agent_id=?1 AND instance_id=?2)", params![agent,instance], |r| r.get(0))?)
    }

    pub(crate) fn plugin_credential_supplier(
        &self,
        agent: &str,
        plugin: &str,
    ) -> Result<Option<String>> {
        Ok(self.0.query_row("SELECT instance_id FROM plugin_credential_suppliers WHERE agent_id=? AND plugin_id=?", params![agent,plugin], |r| r.get(0)).optional()?.flatten())
    }

    /// Legacy owner editors explicitly add full access. Only the first choice can
    /// initialize a supplier; a revoked choice cannot be replaced by this path.
    pub(crate) fn sync_full_plugin_grants(&self, agent: &str, instances: &[String]) -> Result<()> {
        for instance in instances {
            self.0.execute(
                "DELETE FROM plugin_contribution_grants WHERE agent_id=? AND instance_id=?",
                params![agent, instance],
            )?;
            self.0.execute("INSERT OR IGNORE INTO plugin_credential_suppliers(agent_id,plugin_id,instance_id) SELECT ?,plugin_id,id FROM plugin_instances WHERE id=?", params![agent,instance])?;
        }
        self.clear_ungranted_suppliers(agent)
    }

    pub(crate) fn clear_ungranted_suppliers(&self, agent: &str) -> Result<()> {
        self.0.execute("UPDATE plugin_credential_suppliers SET instance_id=NULL WHERE agent_id=? AND instance_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM instance_grants g WHERE g.agent_id=plugin_credential_suppliers.agent_id AND g.instance_id=plugin_credential_suppliers.instance_id) AND NOT EXISTS(SELECT 1 FROM plugin_contribution_grants g WHERE g.agent_id=plugin_credential_suppliers.agent_id AND g.instance_id=plugin_credential_suppliers.instance_id AND kind='runCredentials' AND name='')", [agent])?;
        Ok(())
    }
}

fn declared(manifest: &cairn_plugin_api::Manifest, grant: &PluginContributionGrant) -> bool {
    match grant.kind {
        Kind::Tool => manifest.tool(&grant.name).is_some(),
        Kind::Command => manifest.command(&grant.name).is_some(),
        Kind::Hook => manifest.hook(&grant.name).is_some(),
        Kind::Condition => manifest.conditions.iter().any(|c| c.name == grant.name),
        Kind::ProjectSource => manifest.project_source(&grant.name).is_some(),
        Kind::SkillSource => manifest.skill_source(&grant.name).is_some(),
        Kind::RunCredentials => grant.name.is_empty() && manifest.run_credentials.is_some(),
        Kind::Instructions => grant.name.is_empty() && !manifest.instructions.is_empty(),
    }
}

pub(crate) fn access_covers(current: &Access, proposed: &Access) -> bool {
    match (current, proposed) {
        (Access::Full, _) => true,
        (_, Access::Full) => false,
        (
            Access::Selected {
                contributions: current,
            },
            Access::Selected {
                contributions: proposed,
            },
        ) => proposed.iter().all(|next| {
            current.iter().any(|held| {
                held.kind == next.kind
                    && held.name == next.name
                    && resources_cover(&held.resources, &next.resources)
            })
        }),
    }
}

// No resource ancestry is inferred from names or picker results. Exact references
// suffice for the synchronous ceiling; effect authorization resolves live ancestry.
fn resources_cover(current: &[PluginResourceRef], proposed: &[PluginResourceRef]) -> bool {
    current.is_empty()
        || (!proposed.is_empty()
            && current.iter().all(|restriction| {
                proposed
                    .iter()
                    .any(|r| r.resource_type == restriction.resource_type)
            })
            && proposed.iter().all(|r| {
                current
                    .iter()
                    .any(|held| held.resource_type == r.resource_type && held.id == r.id)
            }))
}

pub(crate) fn covers_in(db: &Db<'_>, requester: &str, target: &str) -> Result<bool> {
    if requester == target {
        return Ok(true);
    }
    let current = db.agent_plugin_grants(requester)?;
    let proposed = db.agent_plugin_grants(target)?;
    Ok(proposed.grants.iter().all(|next| {
        current.grants.iter().any(|held| {
            held.instance_id == next.instance_id && access_covers(&held.access, &next.access)
        })
    }) && proposed
        .run_credential_instances
        .iter()
        .all(|id| current.run_credential_instances.contains(id)))
}

/// Ticket E/F supplies the rule usage lookup here, in the same publication
/// transaction. Until provenance has been established, every current account's
/// use is protected, including the owner's and disabled/blocked rules.
pub(crate) fn protect_rule_uses(
    db: &Db<'_>,
    agent: &str,
    before: &AgentPluginGrants,
    after: &cairn_contracts::ReplaceAgentPluginGrantsInput,
) -> Result<()> {
    for usage in rule_uses(db, agent)? {
        let previous = before
            .grants
            .iter()
            .find(|g| g.instance_id == usage.instance_id);
        let proposed = after
            .grants
            .iter()
            .find(|g| g.instance_id == usage.instance_id);
        if previous.is_some_and(|previous| {
            proposed.is_none_or(|proposed| !access_covers(&proposed.access, &previous.access))
        }) {
            return Err(Error::forbidden(
                "An agent cannot reduce a grant used by another account's rule, or by any account's rule outside an eligible conversation.",
            ));
        }
    }
    Ok(())
}

pub(crate) struct RuleGrantUse {
    pub instance_id: PluginInstanceId,
}

pub(crate) fn rule_uses(_db: &Db<'_>, _agent: &str) -> Result<Vec<RuleGrantUse>> {
    // Rules do not exist before ticket E. Its canonical rule store replaces this
    // empty lookup; do not infer absence from rule state or budget.
    Ok(Vec::new())
}

pub(crate) async fn replace(
    s: &crate::service::Service,
    input: cairn_contracts::ReplaceAgentPluginGrantsInput,
    authority: crate::capabilities::plugin_packages::Authority,
    reduce_only: bool,
) -> Result<AgentPluginGrants> {
    use std::collections::BTreeSet;
    let id = input.agent_id.to_string();
    if input.grants.len() > 100 || input.run_credential_instances.len() > 100 {
        return Err(Error::bad("Select at most 100 plugin instances."));
    }
    let mut instances = BTreeSet::new();
    let mut packages = BTreeMap::new();
    for grant in &input.grants {
        if !instances.insert(grant.instance_id.to_string()) {
            return Err(Error::bad("Select each instance once."));
        }
        let instance = required(
            s.store
                .plugin_instance(&grant.instance_id.to_string())
                .await?,
            "Plugin instance not found",
        )?;
        let package = super::registry::active(s, &instance.plugin_id).await?;
        if package.manifest.agent_access.is_none() {
            return Err(Error::bad(
                "This plugin does not grant agent contributions.",
            ));
        }
        if let Access::Selected { contributions } = &grant.access {
            if contributions.is_empty() || contributions.len() > 500 {
                return Err(Error::bad("Select between 1 and 500 contributions."));
            }
            let mut seen = BTreeSet::new();
            for contribution in contributions {
                if !seen.insert((contribution.kind, &contribution.name))
                    || !declared(&package.manifest, contribution)
                    || contribution.resources.len() > 100
                {
                    return Err(Error::bad("Choose distinct declared contributions."));
                }
                if !contribution.resources.is_empty()
                    && !matches!(contribution.kind, Kind::Tool | Kind::Command | Kind::Hook)
                {
                    return Err(Error::bad(
                        "Only tools, commands and hooks accept resource restrictions.",
                    ));
                }
                let mut resources = BTreeSet::new();
                for resource in &contribution.resources {
                    if package
                        .manifest
                        .resource_type(&resource.resource_type)
                        .is_none()
                        || resource.id.is_empty()
                        || resource.id.len() > 200
                        || resource.id.chars().any(char::is_control)
                        || !resources.insert((&resource.resource_type, &resource.id))
                    {
                        return Err(Error::bad("Choose distinct resources of declared types."));
                    }
                }
            }
        }
        packages.insert(instance.id.clone(), (instance, package));
    }
    let mut suppliers = BTreeSet::new();
    for supplier in &input.run_credential_instances {
        let (instance, package) = packages
            .get(&supplier.to_string())
            .ok_or_else(|| Error::bad("A credential supplier needs an instance grant."))?;
        let grant = input
            .grants
            .iter()
            .find(|g| g.instance_id == *supplier)
            .unwrap();
        let permits = matches!(&grant.access, Access::Full)
            || matches!(&grant.access, Access::Selected { contributions } if contributions.iter().any(|c| c.kind == Kind::RunCredentials));
        if !permits
            || package.manifest.run_credentials.is_none()
            || !suppliers.insert(instance.plugin_id.clone())
        {
            return Err(Error::bad(
                "Explicitly choose one authorized credential supplier per package.",
            ));
        }
    }
    // Selecting run credentials is itself the explicit supplier choice.
    for grant in &input.grants {
        if matches!(&grant.access, Access::Selected { contributions } if contributions.iter().any(|c| c.kind == Kind::RunCredentials))
            && !input.run_credential_instances.contains(&grant.instance_id)
        {
            return Err(Error::bad(
                "Choose the supplying instance for run credentials.",
            ));
        }
    }
    authority.recheck(s).await?;
    let _publication = s.plugins.package_publication.lock().await;
    for (_, package) in packages.values() {
        if super::registry::active(s, &package.manifest.id)
            .await?
            .digest
            != package.digest
        {
            return Err(Error::conflict(
                "The plugin package changed. Retry the grant change.",
            ));
        }
    }
    s.store.transaction(move |db| {
        authority.authorize(db)?;
        let before = db.agent_plugin_grants(&id)?;
        if before.revision != input.revision { return Err(Error::conflict("This agent changed. Reload it before saving.")); }
        for (instance, _) in packages.values() { super::connections::check_revision(db, instance)?; }
        if reduce_only {
            let covered = input.grants.iter().all(|next| before.grants.iter().any(|held| held.instance_id == next.instance_id && access_covers(&held.access, &next.access))) && input.run_credential_instances.iter().all(|id| before.run_credential_instances.contains(id));
            if !covered { return Err(Error::forbidden("Installation agents may only read or reduce plugin grants, never widen them or choose another credential supplier.")); }
            protect_rule_uses(db, &id, &before, &input)?;
        }
        let mut agent = required(db.agent(&id)?, "Agent not found")?;
        agent.settings.plugins = input.grants.iter().filter(|g| matches!(g.access, Access::Full)).map(|g| g.instance_id.to_string()).collect();
        agent.revision = crate::config::id();
        db.save_agent(&agent)?;
        db.0.execute("DELETE FROM plugin_contribution_grants WHERE agent_id=?", [&id])?;
        for grant in &input.grants {
            if let Access::Selected { contributions } = &grant.access {
                for contribution in contributions {
                    db.0.execute("INSERT INTO plugin_contribution_grants VALUES(?,?,?,?)", params![id,grant.instance_id.to_string(),contribution.kind.key(),contribution.name])?;
                    for resource in &contribution.resources {
                        db.0.execute("INSERT INTO plugin_grant_resources VALUES(?,?,?,?,?,?)", params![id,grant.instance_id.to_string(),contribution.kind.key(),contribution.name,resource.resource_type,resource.id])?;
                    }
                }
            }
        }
        db.0.execute("UPDATE plugin_credential_suppliers SET instance_id=NULL WHERE agent_id=?", [&id])?;
        for supplier in &input.run_credential_instances {
            let (instance, _) = &packages[&supplier.to_string()];
            db.0.execute("INSERT INTO plugin_credential_suppliers VALUES(?,?,?) ON CONFLICT(agent_id,plugin_id) DO UPDATE SET instance_id=excluded.instance_id", params![id,instance.plugin_id,instance.id])?;
        }
        db.audit("agent.plugin_grants.changed", &serde_json::json!({"agentId":id}))?;
        db.agent_plugin_grants(&id)
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn legacy_full_access_is_per_instance_and_supplier_removal_never_falls_back() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let first = "00000000-0000-4000-8000-000000000010";
                let second = "00000000-0000-4000-8000-000000000011";
                let mut agent = crate::agent_fixture::profile(
                    json!({"id":"00000000-0000-4000-8000-000000000012"}),
                )?;
                for (id, name) in [(first, "First"), (second, "Second")] {
                    let mut instance = super::super::connections::Connection::draft(
                        id,
                        "github",
                        json!({}),
                        Vec::new(),
                    );
                    instance.name = name.into();
                    instance.revision = "r1".into();
                    instance.enabled = true;
                    db.save_plugin_instance(&instance)?;
                }
                agent.settings.plugins = vec![first.into(), second.into()];
                db.save_agent(&agent)?;
                let id = agent.id.to_string();
                assert_eq!(
                    db.plugin_credential_supplier(&id, "github")?.as_deref(),
                    Some(first)
                );
                assert!(
                    db.plugin_contribution(&id, first, Kind::Hook, "future-hook")?
                        .is_some()
                );
                assert!(
                    db.plugin_contribution(&id, second, Kind::Tool, "future-tool")?
                        .is_some()
                );
                agent.settings.plugins.retain(|instance| instance != first);
                db.save_agent(&agent)?;
                assert_eq!(db.plugin_credential_supplier(&id, "github")?, None);
                assert!(
                    db.plugin_contribution(&id, first, Kind::Tool, "future-tool")?
                        .is_none()
                );
                assert!(
                    db.plugin_contribution(&id, second, Kind::Tool, "future-tool")?
                        .is_some()
                );
                db.save_agent(&agent)?;
                assert_eq!(db.plugin_credential_supplier(&id, "github")?, None);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[test]
    fn reductions_compare_contributions_and_every_restricted_type() {
        let grant = |name: &str, resources: Vec<(&str, &str)>| Access::Selected {
            contributions: vec![PluginContributionGrant {
                kind: Kind::Tool,
                name: name.into(),
                resources: resources
                    .into_iter()
                    .map(|(kind, id)| PluginResourceRef {
                        resource_type: kind.into(),
                        id: id.into(),
                    })
                    .collect(),
            }],
        };
        let current = grant("post", vec![("channel", "support"), ("channel", "ops")]);
        assert!(access_covers(&Access::Full, &current));
        assert!(!access_covers(&current, &Access::Full));
        assert!(access_covers(
            &current,
            &grant("post", vec![("channel", "support")])
        ));
        assert!(!access_covers(&current, &grant("post", vec![])));
        assert!(!access_covers(
            &current,
            &grant("read", vec![("channel", "support")])
        ));
        assert!(!access_covers(
            &current,
            &grant("post", vec![("channel", "general")])
        ));
    }
}
