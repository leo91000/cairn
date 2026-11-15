//! Bounded resource discovery and host-resolved ancestry for pickers and grants.
use super::{
    connections, management, registry,
    scope::{self, Scope},
};
use crate::{
    capabilities::plugin_packages::Authority,
    error::{Error, Result},
    service::Service,
};
use cairn_contracts::{
    ListPluginResourcesInput, ListPluginResourcesOutput, PluginResource, PluginResourceRef,
    ResolvePluginResourceInput, ResolvePluginResourceOutput,
};
use cairn_plugin_api::Manifest;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

fn plain(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn invalid() -> Error {
    Error::bad_gateway("The plugin returned invalid or unresolved resources.")
}

fn not_granted() -> Error {
    Error::forbidden("This agent is not granted that plugin instance.")
}

fn item(manifest: &Manifest, raw: Value, kind: &str) -> Result<PluginResource> {
    let resource: PluginResource = serde_json::from_value(raw).map_err(|_| invalid())?;
    let declaration = manifest.resource_type(kind).ok_or_else(invalid)?;
    if resource.resource_type != kind || !plain(&resource.id, 200) || !plain(&resource.name, 300) {
        return Err(invalid());
    }
    match (&declaration.parent_type, &resource.parent) {
        (None, None) => {}
        (Some(kind), Some(parent)) if parent.resource_type == *kind && plain(&parent.id, 200) => {}
        _ => return Err(invalid()),
    }
    Ok(resource)
}

/// Prepared before enqueueing: settings, grants and package identity cannot
/// silently switch while an operation waits for its worker or is polled.
pub(crate) struct Read {
    package: registry::Package,
    scope: Scope,
}

impl Read {
    pub(crate) async fn authorize_result(
        self: Arc<Self>,
        s: &Service,
        caller: &Authority,
    ) -> Result<()> {
        self.authorize(s).await?;
        // The poller can differ from the originating public authorization.
        // Recheck it after the originating authority's remote check, then take
        // one final local grant/revision snapshot after all remote I/O.
        caller.recheck(s).await?;
        // Activation can race either remote authorization. Serialize the final
        // package check and local snapshot with package publication.
        let _publication = s.plugins.package_publication.lock().await;
        self.check_package(s).await?;
        s.store
            .read(move |db| scope::authorize_in(db, &self.scope))
            .await
    }

    async fn check_package(&self, s: &Service) -> Result<()> {
        let current = registry::active(s, &self.package.manifest.id).await?;
        if current.digest != self.package.digest {
            return Err(Error::forbidden(
                "The plugin package changed. Retry the resource read.",
            ));
        }
        Ok(())
    }

    pub(crate) async fn authorize(&self, s: &Service) -> Result<()> {
        self.check_package(s).await?;
        scope::authorize(s, &self.scope).await
    }
}

/// Resource reads use credentials. Installation access alone must not give an
/// agent access to an ungranted instance's service account.
async fn instance(
    s: &Service,
    plugin: &str,
    instance: &str,
    kind: &str,
    agent: Option<&str>,
    authority: Option<Authority>,
) -> Result<Read> {
    // A read through an agent learns nothing of a package or instance the agent is
    // not granted: neither whether it exists, nor its approval, types or state.
    if let Some(agent) = agent {
        let (agent, plugin, id) = (agent.to_owned(), plugin.to_owned(), instance.to_owned());
        let granted = s
            .store
            .read(move |db| {
                Ok(db
                    .plugin_instance(&id)?
                    .is_some_and(|connection| connection.plugin_id == plugin)
                    && db.has_plugin_grant(&agent, &id)?)
            })
            .await?;
        if !granted {
            return Err(not_granted());
        }
    }
    let package = registry::active(s, plugin).await?;
    if package.manifest.resource_type(kind).is_none() {
        return Err(Error::not_found("This plugin has no such resource type."));
    }
    let connection = connections::get(s, plugin, instance).await?;
    if !connection.enabled {
        return Err(Error::bad(
            "Enable this instance before reading its resources.",
        ));
    }

    // Refresh publication and HTTP pre-send checks also need the expected
    // instance: revoking a plugin grant does not change its settings revision.
    // A named agent retains its grant/package authority even when the caller is
    // a person or public MCP client, without an installation bearer. Management
    // checks this snapshot locally after asynchronous policy checks and waits.
    let authority = match authority {
        Some(authority) => {
            let snapshot = match (agent, authority.bearer.as_deref()) {
                (Some(agent), Some(bearer)) => Some(
                    super::grant_authorization::Snapshot::capture_installation(
                        s, agent, bearer, &package,
                    )
                    .await?,
                ),
                (Some(agent), None) => Some(
                    super::grant_authorization::Snapshot::capture(s, agent, None, &package).await?,
                ),
                (None, _) => None,
            };
            let managed = management::Authority::resource_read(authority, connection.clone());
            Some(match snapshot {
                Some(snapshot) => managed.bind_snapshot(snapshot),
                None => managed,
            })
        }
        None => None,
    };

    let scope = if let Some(agent) = agent {
        let (agent_id, instance_id) = (agent.to_owned(), connection.id.clone());
        let permitted = s
            .store
            .read(move |db| db.has_plugin_grant(&agent_id, &instance_id))
            .await?;
        if !permitted {
            return Err(not_granted());
        }
        Scope::Agent {
            agent_id: agent.to_owned(),
            instances: vec![connection],
        }
    } else {
        Scope::Instance(Box::new(connection))
    };
    let scope = match authority {
        Some(authority) => Scope::Managed(Box::new(scope), authority),
        None => scope,
    };
    Ok(Read { package, scope })
}

pub async fn list(
    s: &Service,
    input: ListPluginResourcesInput,
    agent: Option<&str>,
) -> Result<ListPluginResourcesOutput> {
    prepare_list(s, &input, agent, None)
        .await?
        .list(s, input)
        .await
}

pub(crate) async fn prepare_list(
    s: &Service,
    input: &ListPluginResourcesInput,
    agent: Option<&str>,
    authority: Option<Authority>,
) -> Result<Read> {
    if !(1..=10_000).contains(&input.page)
        || input.query.chars().count() > 200
        || input.query.chars().any(char::is_control)
    {
        return Err(Error::bad(
            "Choose a page from 1 to 10000 and a query of at most 200 characters.",
        ));
    }
    instance(
        s,
        &input.plugin_id,
        &input.instance_id.to_string(),
        &input.resource_type,
        agent,
        authority,
    )
    .await
}

pub async fn resolve(
    s: &Service,
    input: ResolvePluginResourceInput,
    agent: Option<&str>,
) -> Result<ResolvePluginResourceOutput> {
    prepare_resolve(s, &input, agent, None)
        .await?
        .resolve(s, input)
        .await
}

pub(crate) async fn prepare_resolve(
    s: &Service,
    input: &ResolvePluginResourceInput,
    agent: Option<&str>,
    authority: Option<Authority>,
) -> Result<Read> {
    if !plain(&input.id, 200) {
        return Err(Error::bad("Choose a resource ID of at most 200 bytes."));
    }
    instance(
        s,
        &input.plugin_id,
        &input.instance_id.to_string(),
        &input.resource_type,
        agent,
        authority,
    )
    .await
}

impl Read {
    pub(crate) fn with_snapshot(
        mut self,
        snapshot: super::grant_authorization::Snapshot,
        instance: connections::Connection,
        preparation: Option<Authority>,
    ) -> Self {
        self.scope = Scope::Managed(
            Box::new(self.scope),
            management::Authority::agent_action(instance, snapshot, None)
                .with_preparation(preparation),
        );
        self
    }

    pub(crate) async fn list(
        &self,
        s: &Service,
        input: ListPluginResourcesInput,
    ) -> Result<ListPluginResourcesOutput> {
        self.authorize(s).await?;
        let package = &self.package;
        let scope = &self.scope;
        let result = scope::invoke(
            s,
            package,
            "resources",
            json!({
                "op": "list",
                "resourceType": input.resource_type,
                "page": input.page,
                "query": input.query,
                "instance": scope.instances()[0],
            }),
            scope,
        )
        .await?;
        let raw = result["items"]
            .as_array()
            .filter(|items| items.len() <= 200)
            .ok_or_else(invalid)?;
        let mut seen = BTreeSet::new();
        let mut items = Vec::with_capacity(raw.len());
        for raw in raw {
            let resource = item(&package.manifest, raw.clone(), &input.resource_type)?;
            if !seen.insert(resource.id.clone()) {
                return Err(invalid());
            }
            items.push(resource);
        }
        let next_page = match result.get("nextPage") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_i64()
                    .filter(|page| *page > input.page && *page <= 10_000)
                    .ok_or_else(invalid)?,
            ),
        };
        self.authorize(s).await?;
        Ok(ListPluginResourcesOutput { items, next_page })
    }

    pub(crate) async fn resolve(
        &self,
        s: &Service,
        input: ResolvePluginResourceInput,
    ) -> Result<ResolvePluginResourceOutput> {
        let package = &self.package;
        let scope = &self.scope;
        let mut target = PluginResourceRef {
            resource_type: input.resource_type,
            id: input.id,
        };
        let mut chain = Vec::new();
        // The manifest is an acyclic graph of at most 32 types. Each parent is
        // independently resolved; caller-provided ancestors are never trusted.
        for _ in 0..package.manifest.resources.len() {
            self.authorize(s).await?;
            let raw = scope::invoke(
                s,
                package,
                "resources",
                json!({
                    "op": "resolve",
                    "resourceType": target.resource_type,
                    "id": target.id,
                    "instance": scope.instances()[0],
                }),
                scope,
            )
            .await?;
            let resource = item(&package.manifest, raw, &target.resource_type)?;
            if resource.id != target.id {
                return Err(invalid());
            }
            let parent = resource.parent.clone();
            chain.push(resource);
            match parent {
                Some(parent) => target = parent,
                None => {
                    let resource = chain.remove(0);
                    self.authorize(s).await?;
                    return Ok(ResolvePluginResourceOutput {
                        resource,
                        ancestors: chain,
                    });
                }
            }
        }
        Err(invalid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::MAIN_AGENT_ID,
        plugin_grant_fixture::{Fixture, contribution, support},
    };

    #[tokio::test]
    async fn named_agent_authority_survives_the_oauth_lock_wait() {
        for revoke_package in [false, true] {
            let f = Fixture::new().await;
            f.set(json!([contribution("hook", "message", support())]))
                .await;
            let input: ListPluginResourcesInput = serde_json::from_value(json!({
                "pluginId": "grant-probe",
                "instanceId": f.instance,
                "resourceType": "channel",
                "agentId": MAIN_AGENT_ID,
            }))
            .unwrap();
            let owner = Authority::human(&f.s, "fixture-owner").await.unwrap();
            let read = prepare_list(&f.s, &input, Some(MAIN_AGENT_ID), Some(owner))
                .await
                .unwrap();
            let Scope::Managed(_, authority) = &read.scope else {
                panic!("the read retains the caller's authority");
            };
            let connection = connections::get(&f.s, "grant-probe", &f.instance)
                .await
                .unwrap();
            f.s.vault
                .set(
                    &connections::credential_key(&f.instance),
                    &json!({ "token": "synthetic-resource-credential" }),
                )
                .await
                .unwrap();
            let lock = f.s.plugins.oauth_flows.lock(&f.instance);
            let held = lock.lock().await;
            let access =
                super::super::oauth::access_token_managed(&f.s, &connection, Some(authority));
            tokio::pin!(access);
            assert!(futures_util::poll!(&mut access).is_pending());

            if revoke_package {
                f.s.store
                    .transaction(|db| {
                        db.0.execute(
                            "UPDATE plugin_approvals SET digest=?1 WHERE plugin_id='grant-probe'",
                            ["0".repeat(64)],
                        )?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            } else {
                f.remove_for(MAIN_AGENT_ID).await;
            }
            drop(held);

            let result = access.await;
            assert!(
                result.is_err(),
                "OAuth must refuse the revoked named-agent authority after waiting (package={revoke_package})"
            );
            assert_eq!(
                result.unwrap_err().status,
                if revoke_package { 409 } else { 403 }
            );
            f.s.shutdown.cancel();
        }
    }
}
