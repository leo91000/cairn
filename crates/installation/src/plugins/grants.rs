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
pub(crate) fn migrate(db: &Db<'_>, directory: &std::path::Path) -> Result<()> {
    db.0.execute_batch(SCHEMA)?;
    let mut instances = db.plugin_instances()?;
    instances.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    let mut chosen = std::collections::BTreeSet::new();
    for instance in instances {
        // A temporarily unavailable package must keep the exact legacy choice.
        // It cannot supply anything until its approved code loads again.
        let supplies = super::registry::upgrade_package(db, directory, &instance.plugin_id)
            .map_or(true, |package| package.manifest.run_credentials.is_some());
        for agent in &instance.agent_ids {
            db.0.execute(
                "INSERT OR IGNORE INTO plugin_credential_suppliers VALUES(?,?,NULL)",
                params![agent, instance.plugin_id],
            )?;
            if supplies
                && instance.enabled
                && chosen.insert((agent.clone(), instance.plugin_id.clone()))
            {
                db.0.execute("UPDATE plugin_credential_suppliers SET instance_id=? WHERE agent_id=? AND plugin_id=?",params![instance.id,agent,instance.plugin_id])?;
            }
        }
    }
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
        let directory = self
            .0
            .path()
            .and_then(|path| std::path::Path::new(path).parent())
            .unwrap_or(std::path::Path::new(""));
        let mut packages = BTreeMap::new();
        for instance in instances {
            self.0.execute(
                "DELETE FROM plugin_contribution_grants WHERE agent_id=? AND instance_id=?",
                params![agent, instance],
            )?;
            let connection =
                required(self.plugin_instance(instance)?, "Plugin instance not found")?;
            let supplies = *packages
                .entry(connection.plugin_id.clone())
                .or_insert_with(|| {
                    super::registry::upgrade_package(self, directory, &connection.plugin_id)
                        .is_ok_and(|package| package.manifest.run_credentials.is_some())
                });
            let choice = supplies.then_some(instance);
            // Even a package without credentials gets an empty choice: adding
            // credentials in a later version must not silently select an instance.
            self.0.execute(
                "INSERT OR IGNORE INTO plugin_credential_suppliers VALUES(?,?,?)",
                params![agent, connection.plugin_id, choice],
            )?;
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
        || current
            .iter()
            .map(|r| &r.resource_type)
            .collect::<std::collections::BTreeSet<_>>()
            .iter()
            .all(|kind| {
                let mut selected = proposed
                    .iter()
                    .filter(|r| &r.resource_type == *kind)
                    .peekable();
                selected.peek().is_some()
                    && selected.all(|next| {
                        current.iter().any(|held| {
                            held.resource_type == next.resource_type && held.id == next.id
                        })
                    })
            })
}

/// Ticket E/F supplies the rule usage lookup here, in the same publication
/// transaction. Until provenance has been established, every current account's
/// use is protected, including the owner's and disabled/blocked rules.
pub(crate) fn protect_rule_uses(
    db: &Db<'_>,
    agent: &str,
    _before: &AgentPluginGrants,
    after: &cairn_contracts::ReplaceAgentPluginGrantsInput,
) -> Result<()> {
    for usage in rule_uses(db, agent)? {
        let proposed = after
            .grants
            .iter()
            .find(|g| g.instance_id == usage.instance_id);
        let used = Access::Selected {
            contributions: vec![usage.contribution],
        };
        if proposed.is_none_or(|proposed| !access_covers(&proposed.access, &used)) {
            return Err(Error::forbidden(
                "An agent cannot reduce a grant used by another account's rule, or by any account's rule outside an eligible conversation.",
            ));
        }
    }
    Ok(())
}

pub(crate) struct RuleGrantUse {
    pub instance_id: PluginInstanceId,
    pub contribution: PluginContributionGrant,
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
    let target = id.clone();
    let before = s
        .store
        .read(move |db| db.agent_plugin_grants(&target))
        .await?;
    if before.revision != input.revision {
        return Err(Error::conflict(
            "This agent changed. Reload it before saving.",
        ));
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
        let package = match super::registry::active(s, &instance.plugin_id).await {
            Ok(package) => Some(package),
            Err(error) => {
                let retained = before
                    .grants
                    .iter()
                    .find(|held| held.instance_id == grant.instance_id)
                    .is_some_and(|held| {
                        held.access == grant.access
                            || matches!(held.access, Access::Selected { .. })
                                && access_covers(&held.access, &grant.access)
                    });
                if !retained {
                    return Err(error);
                }
                None
            }
        };
        if package
            .as_ref()
            .is_some_and(|package| package.manifest.agent_access.is_none())
        {
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
                    || package
                        .as_ref()
                        .is_some_and(|package| !declared(&package.manifest, contribution))
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
                    if package.as_ref().is_some_and(|package| {
                        package
                            .manifest
                            .resource_type(&resource.resource_type)
                            .is_none()
                    }) || resource.id.is_empty()
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
            || match package {
                Some(package) => package.manifest.run_credentials.is_none(),
                None => !before.run_credential_instances.contains(supplier),
            }
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
    let mut reduction_snapshots = Vec::new();
    if reduce_only {
        if input
            .run_credential_instances
            .iter()
            .any(|id| !before.run_credential_instances.contains(id))
        {
            return Err(Error::forbidden(
                "Installation agents cannot choose another credential supplier.",
            ));
        }
        for next in &input.grants {
            let held = before
                .grants
                .iter()
                .find(|held| held.instance_id == next.instance_id)
                .ok_or_else(|| {
                    Error::forbidden("Installation agents cannot grant another instance.")
                })?;
            if access_covers(&held.access, &next.access) {
                continue;
            }
            let (
                Access::Selected {
                    contributions: current,
                },
                Access::Selected {
                    contributions: proposed,
                },
            ) = (&held.access, &next.access)
            else {
                return Err(Error::forbidden(
                    "Installation agents cannot widen plugin access.",
                ));
            };
            let bearer = authority
                .bearer
                .as_deref()
                .ok_or_else(|| Error::forbidden("A calling execution is required."))?;
            let caller = crate::installation_mcp::authorize(s, bearer).await?;
            let caller_agent = crate::validation::text(&caller["snapshot"]["agent"], "id");
            let (instance, package) = &packages[&next.instance_id.to_string()];
            let package = package.as_ref().ok_or_else(|| {
                Error::conflict("Activate this package before comparing resource ancestry.")
            })?;
            let snapshot = super::grant_authorization::Snapshot::capture_installation(
                s,
                caller_agent,
                bearer,
                package,
            )
            .await?;
            for next in proposed {
                let held = current
                    .iter()
                    .find(|held| held.kind == next.kind && held.name == next.name)
                    .ok_or_else(|| {
                        Error::forbidden("Installation agents cannot add a contribution.")
                    })?;
                super::grant_authorization::resource_set_covered(
                    s,
                    &super::grant_authorization::ResourceAuthorization {
                        package,
                        instance,
                        agent: caller_agent,
                        snapshot: snapshot.clone(),
                        preparation: Some(authority.clone()),
                    },
                    held,
                    &next.resources,
                )
                .await?;
            }
            reduction_snapshots.push(snapshot);
        }
    }
    authority.recheck(s).await?;
    let _publication = s.plugins.package_publication.lock().await;
    for (_, package) in packages.values() {
        let Some(package) = package else {
            continue;
        };
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
    s.store
        .transaction(move |db| {
            authority.authorize(db)?;
            let before = db.agent_plugin_grants(&id)?;

            if before.revision != input.revision {
                return Err(Error::conflict("This agent changed. Reload it before saving."));
            }

            for (instance, _) in packages.values() {
                super::connections::check_revision(db, instance)?;
            }

            if reduce_only {
                for snapshot in &reduction_snapshots {
                    snapshot.authorize(db)?;
                }
                protect_rule_uses(db, &id, &before, &input)?;
            }

            let mut agent = required(db.agent(&id)?, "Agent not found")?;
            agent.settings.plugins = input.grants.iter()
                .filter(|grant| matches!(grant.access, Access::Full))
                .map(|grant| grant.instance_id.to_string()).collect();
            agent.revision = crate::config::id();
            db.save_agent(&agent)?;

            db.0.execute("DELETE FROM plugin_contribution_grants WHERE agent_id=?", [&id])?;
            for grant in &input.grants {
                if let Access::Selected { contributions } = &grant.access {
                    let instance = grant.instance_id.to_string();
                    for contribution in contributions {
                        db.0.execute(
                            "INSERT INTO plugin_contribution_grants VALUES(?,?,?,?)",
                            params![id, instance, contribution.kind.key(), contribution.name],
                        )?;
                        for resource in &contribution.resources {
                            db.0.execute(
                                "INSERT INTO plugin_grant_resources VALUES(?,?,?,?,?,?)",
                                params![
                                    id, instance, contribution.kind.key(), contribution.name,
                                    resource.resource_type, resource.id,
                                ],
                            )?;
                        }
                    }
                }
            }

            db.0.execute("UPDATE plugin_credential_suppliers SET instance_id=NULL WHERE agent_id=?", [&id])?;
            for supplier in &input.run_credential_instances {
                let (instance, _) = &packages[&supplier.to_string()];
                db.0.execute(
                    "INSERT INTO plugin_credential_suppliers VALUES(?,?,?) ON CONFLICT(agent_id,plugin_id) DO UPDATE SET instance_id=excluded.instance_id",
                    params![id, instance.plugin_id, instance.id],
                )?;
            }

            db.audit("agent.plugin_grants.changed", &serde_json::json!({ "agentId": id }))?;
            db.agent_plugin_grants(&id)
        })
        .await
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

/// Local half of delegated authorization. Resource ancestry needs asynchronous
/// host resolution; callers must also run `check_coverage` before acting.
pub(crate) fn contribution_ceiling_in(db: &Db<'_>, requester: &str, target: &str) -> Result<bool> {
    if requester == target {
        return Ok(true);
    }
    let current = db.agent_plugin_grants(requester)?;
    let proposed = db.agent_plugin_grants(target)?;
    Ok(proposed.grants.iter().all(|next| {
        current.grants.iter().any(|held| {
            if held.instance_id != next.instance_id {
                return false;
            }
            match (&held.access, &next.access) {
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
                            && (held.resources.is_empty() || !next.resources.is_empty())
                    })
                }),
            }
        })
    }) && proposed
        .run_credential_instances
        .iter()
        .all(|id| current.run_credential_instances.contains(id)))
}

/// Resolve selected resource roots afresh rather than trusting stored labels or
/// ancestry. Return revisions so a mutation can recheck the same profiles locally.
pub(crate) async fn check_coverage_authorized(
    s: &crate::service::Service,
    requester: &str,
    target: &str,
    authority: Option<crate::capabilities::plugin_packages::Authority>,
) -> Result<(String, String)> {
    tokio::time::timeout(
        std::time::Duration::from_secs(40),
        Box::pin(check_coverage_inner(s, requester, target, authority)),
    )
    .await
    .map_err(|_| Error::unavailable("Resource coverage could not be resolved in time."))?
}

async fn check_coverage_inner(
    s: &crate::service::Service,
    requester: &str,
    target: &str,
    authority: Option<crate::capabilities::plugin_packages::Authority>,
) -> Result<(String, String)> {
    if let Some(authority) = &authority {
        authority.recheck(s).await?;
    }
    let (requester, target) = (requester.to_owned(), target.to_owned());
    let (left, right) = (requester.clone(), target.clone());
    let (current, proposed) = s
        .store
        .read(move |db| {
            if !contribution_ceiling_in(db, &left, &right)? {
                return Err(Error::forbidden(
                    "The requesting agent does not cover the delegated plugin contributions.",
                ));
            }
            Ok((
                db.agent_plugin_grants(&left)?,
                db.agent_plugin_grants(&right)?,
            ))
        })
        .await?;
    let (left, right) = (requester.clone(), target.clone());
    let expected = (current.revision.clone(), proposed.revision.clone());
    let authority = Some(authority.unwrap_or_default().with_guard(move |db| {
        if required(db.agent(&left)?, "Requesting agent not found")?.revision != expected.0
            || required(db.agent(&right)?, "Agent not found")?.revision != expected.1
        {
            return Err(Error::forbidden(
                "Plugin grants changed during resource coverage resolution.",
            ));
        }
        Ok(())
    }));
    if requester != target {
        for next in &proposed.grants {
            let held = current
                .grants
                .iter()
                .find(|held| held.instance_id == next.instance_id)
                .ok_or_else(|| Error::forbidden("Plugin instance not covered."))?;
            if let (
                Access::Selected {
                    contributions: current,
                },
                Access::Selected {
                    contributions: proposed,
                },
            ) = (&held.access, &next.access)
            {
                let instance = required(
                    s.store
                        .plugin_instance(&next.instance_id.to_string())
                        .await?,
                    "Plugin instance not found",
                )?;
                let package = super::registry::active(s, &instance.plugin_id).await?;
                let (reader, snapshot) = if let Some(bearer) = authority
                    .as_ref()
                    .and_then(|authority| authority.bearer.as_deref())
                {
                    let caller = crate::installation_mcp::authorize(s, bearer).await?;
                    let agent =
                        crate::validation::text(&caller["snapshot"]["agent"], "id").to_owned();
                    let snapshot = super::grant_authorization::Snapshot::capture_installation(
                        s, &agent, bearer, &package,
                    )
                    .await?;
                    (agent, snapshot)
                } else {
                    let snapshot = super::grant_authorization::Snapshot::capture(
                        s, &requester, None, &package,
                    )
                    .await?;
                    (requester.clone(), snapshot)
                };
                for next in proposed {
                    let held = current
                        .iter()
                        .find(|held| held.kind == next.kind && held.name == next.name)
                        .ok_or_else(|| Error::forbidden("Plugin contribution not covered."))?;
                    super::grant_authorization::resource_set_covered(
                        s,
                        &super::grant_authorization::ResourceAuthorization {
                            package: &package,
                            instance: &instance,
                            agent: &reader,
                            snapshot: snapshot.clone(),
                            preparation: authority.clone(),
                        },
                        held,
                        &next.resources,
                    )
                    .await?;
                }
            }
        }
    }
    let revisions = (current.revision.clone(), proposed.revision.clone());
    let expected = revisions.clone();
    s.store
        .read(move |db| {
            if required(db.agent(&requester)?, "Requesting agent not found")?.revision != expected.0
                || required(db.agent(&target)?, "Agent not found")?.revision != expected.1
            {
                return Err(Error::conflict(
                    "Plugin grants changed during delegation authorization.",
                ));
            }
            Ok(())
        })
        .await?;
    if let Some(authority) = &authority {
        authority.recheck(s).await?;
    }
    Ok(revisions)
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn migration_retains_the_previously_enabled_name_ordered_supplier() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        let directory = root.path().to_path_buf();
        store
            .transaction(move |db| {
                let mut profile = crate::agent_fixture::profile(
                    json!({"id":"00000000-0000-4000-8000-000000000022"}),
                )?;
                let earlier = "00000000-0000-4000-8000-000000000020";
                let later = "00000000-0000-4000-8000-000000000021";
                for (id, name) in [(earlier, "A"), (later, "Z")] {
                    let mut instance = super::super::connections::Connection::draft(
                        id,
                        "github",
                        json!({}),
                        Vec::new(),
                    );
                    instance.name = name.into();
                    instance.enabled = true;
                    instance.revision = "r1".into();
                    db.save_plugin_instance(&instance)?;
                }
                profile.settings.plugins = vec![later.into(), earlier.into()];
                db.save_agent(&profile)?;
                let agent = profile.id.to_string();
                db.0.execute("DELETE FROM plugin_credential_suppliers", [])?;
                migrate(db, &directory)?;
                assert_eq!(
                    db.plugin_credential_supplier(&agent, "github")?.as_deref(),
                    Some(earlier)
                );
                assert_eq!(
                    db.agent(&agent)?.unwrap().settings.plugins,
                    vec![later, earlier]
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod unavailable_migration_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn migration_keeps_the_legacy_choice_when_approved_package_files_are_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).unwrap();
        let directory = root.path().to_path_buf();
        store
            .transaction(move |db| {
                db.activate_plugin_version("unavailable", "1", &"0".repeat(64), true)?;
                let instance = "00000000-0000-4000-8000-000000000025";
                let mut connection = super::super::connections::Connection::draft(
                    instance,
                    "unavailable",
                    json!({}),
                    Vec::new(),
                );
                connection.name = "Unavailable".into();
                connection.enabled = true;
                connection.revision = "r1".into();
                db.save_plugin_instance(&connection)?;
                let mut profile = crate::agent_fixture::profile(
                    json!({"id":"00000000-0000-4000-8000-000000000026"}),
                )?;
                profile.settings.plugins = vec![instance.into()];
                db.save_agent(&profile)?;
                db.0.execute("DELETE FROM plugin_credential_suppliers", [])?;
                migrate(db, &directory)?;
                assert_eq!(
                    db.plugin_credential_supplier(&profile.id.to_string(), "unavailable")?
                        .as_deref(),
                    Some(instance)
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}
