//! Caller-bound authorization retained through scripts, OAuth refresh and effects.
use super::{connections::Connection, registry::Package};
use crate::{
    error::{Error, Result, required},
    service::Service,
    store::Db,
    validation::text,
};
use cairn_contracts::{
    PluginContributionGrant, PluginContributionKind as Kind, PluginResourceRef,
    ResolvePluginResourceInput,
};
use cairn_plugin_api::{Manifest, ResourceField};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

fn denied() -> Error {
    Error::forbidden("This plugin contribution or its resources are not granted to the caller.")
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub agent: String,
    revision: String,
    requester: Option<(String, String)>,
    bearer: Option<(String, bool)>,
    run_id: Option<(String, Value)>,
    wait_owner: Option<Arc<crate::waits::Owner>>,
    package: Arc<Package>,
}

impl Snapshot {
    pub(crate) async fn capture(
        s: &Service,
        agent: &str,
        bearer: Option<&str>,
        package: &Package,
    ) -> Result<Self> {
        Self::capture_with(s, agent, bearer, false, package).await
    }

    pub(crate) async fn capture_installation(
        s: &Service,
        agent: &str,
        bearer: &str,
        package: &Package,
    ) -> Result<Self> {
        Self::capture_with(s, agent, Some(bearer), true, package).await
    }

    async fn capture_with(
        s: &Service,
        agent: &str,
        bearer: Option<&str>,
        installation: bool,
        package: &Package,
    ) -> Result<Self> {
        let agent = agent.to_owned();
        let bearer = bearer.map(|bearer| (bearer.to_owned(), installation));
        let package = Arc::new(package.clone());
        s.store
            .read(move |db| {
                let profile = required(db.agent(&agent)?, "Agent not found")?;
                let requester = if let Some((bearer, installation)) = &bearer {
                    let run = if *installation {
                        crate::installation_mcp::authorize_in(db, bearer)?
                    } else {
                        crate::project_workspaces::authorize_in(db, bearer)?
                    };
                    if run["snapshot"]["agent"]["id"] != agent {
                        return Err(denied());
                    }
                    requesting_agent(db, &run)?
                } else {
                    None
                };
                Ok(Self {
                    agent,
                    revision: profile.revision,
                    requester,
                    bearer,
                    run_id: None,
                    wait_owner: None,
                    package,
                })
            })
            .await
    }

    pub(crate) async fn for_run(s: &Service, run: &Value, package: &Package) -> Result<Self> {
        let mut snapshot =
            Self::capture(s, text(&run["snapshot"]["agent"], "id"), None, package).await?;
        let caller = run.clone();
        let expected = snapshot.clone();
        snapshot.requester = s
            .store
            .read(move |db| {
                expected.authorize(db)?;
                crate::delegation::authorize_run(db, &caller)?;
                requesting_agent(db, &caller)
            })
            .await?;
        snapshot.run_id = Some((
            text(run, "id").into(),
            crate::waits::execution_id(run).clone(),
        ));
        Ok(snapshot)
    }

    pub(crate) async fn for_wait(
        s: &Service,
        run: &Value,
        package: &Package,
        owner: Arc<crate::waits::Owner>,
    ) -> Result<Self> {
        let mut snapshot = Self::for_run(s, run, package).await?;
        snapshot.wait_owner = Some(owner);
        Ok(snapshot)
    }

    fn authority(&self) -> crate::capabilities::plugin_packages::Authority {
        let expected = self.clone();
        crate::capabilities::plugin_packages::Authority::default()
            .with_guard(move |db| expected.authorize(db))
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        let expected = self.clone();
        s.store.read(move |db| expected.authorize(db)).await?;
        if let Some((requester, _)) = &self.requester {
            Box::pin(super::grants::check_coverage_authorized(
                s,
                requester,
                &self.agent,
                Some(self.authority()),
            ))
            .await?;
        }
        let expected = self.clone();
        s.store.read(move |db| expected.authorize(db)).await
    }

    pub(crate) fn authorize(&self, db: &Db<'_>) -> Result<()> {
        if required(db.agent(&self.agent)?, "Agent not found")?.revision != self.revision {
            return Err(denied());
        }
        if let Some((id, revision)) = &self.requester
            && required(db.agent(id)?, "Requesting agent not found")?.revision != *revision
        {
            return Err(denied());
        }
        if let Some((bearer, installation)) = &self.bearer {
            let run = if *installation {
                crate::installation_mcp::authorize_in(db, bearer)?
            } else {
                crate::project_workspaces::authorize_in(db, bearer)?
            };
            if run["snapshot"]["agent"]["id"] != self.agent
                || requesting_agent(db, &run)? != self.requester
            {
                return Err(denied());
            }
        }
        if let Some((run_id, execution)) = &self.run_id {
            let run = required(db.run(run_id)?, "Run not found")?;
            if crate::waits::execution_id(&run) != execution || !run["cancelRequestedAt"].is_null()
            {
                return Err(denied());
            }
            if let Some(expected) = &self.wait_owner {
                let current = required(expected.reload(db)?, "Wait no longer exists")?;
                if current.run_id() != run_id
                    || current.wakeup.as_ref().map(|wait| &wait.id)
                        != expected.wakeup.as_ref().map(|wait| &wait.id)
                    || !current.resumable(&run)
                {
                    return Err(denied());
                }
            } else if !crate::run_status::RunStatus::of(&run)
                .is_some_and(crate::run_status::RunStatus::is_active)
                || crate::waits::Owner::for_run(db, &run)?.run_id() != run_id
            {
                return Err(denied());
            }
            crate::delegation::authorize_run(db, &run)?;
            if run["snapshot"]["agent"]["id"] != self.agent
                || requesting_agent(db, &run)? != self.requester
            {
                return Err(denied());
            }
        }
        if !self.package.builtin {
            let current = db.plugin_package(&self.package.manifest.id)?;
            if current.active.as_deref() != Some(&self.package.manifest.version)
                || current.approvals.get(&self.package.manifest.version)
                    != Some(&self.package.digest)
            {
                return Err(denied());
            }
        }
        Ok(())
    }
}

fn requesting_agent(db: &Db<'_>, run: &Value) -> Result<Option<(String, String)>> {
    let Some(chat) = db.conversation_for_run(text(run, "id"))? else {
        return Ok(None);
    };
    let Some(lineage) = chat.requested_by else {
        return Ok(None);
    };
    let Some(parent) = db
        .conversation(&lineage.chat_id)?
        .filter(|chat| chat.lifecycle == cairn_contracts::ConversationLifecycle::Active)
    else {
        return Ok(None);
    };
    let profile = required(db.agent(&parent.agent_id)?, "Requesting agent not found")?;
    Ok(Some((profile.id.to_string(), profile.revision)))
}

#[derive(Clone)]
pub(crate) struct Invocation {
    pub snapshot: Snapshot,
    instance: Connection,
    kind: Kind,
    name: String,
    input: Value,
    transfers: Arc<Mutex<Vec<Arc<TransferOrigin>>>>,
}

/// The permission that produced a transfer, without the producing run's token.
/// A consumer must authorize the same contribution and resources under its own
/// current run, so handles remain usable across turns without surviving revocation.
#[derive(Clone)]
pub(crate) struct TransferOrigin {
    instance: Connection,
    package: Arc<Package>,
    kind: Kind,
    name: String,
    input: Value,
    dependencies: Vec<Arc<TransferOrigin>>,
}

impl TransferOrigin {
    pub(crate) async fn authorize(
        &self,
        s: &Service,
        connection: &Connection,
        snapshot: Option<Snapshot>,
    ) -> Result<()> {
        let mut visited = BTreeSet::new();
        Box::pin(self.authorize_inner(s, connection, snapshot, &mut visited)).await
    }

    async fn authorize_inner(
        &self,
        s: &Service,
        connection: &Connection,
        snapshot: Option<Snapshot>,
        visited: &mut BTreeSet<usize>,
    ) -> Result<()> {
        if !visited.insert(std::ptr::from_ref(self) as usize) {
            return Ok(());
        }
        if visited.len() > 256 {
            return Err(denied());
        }
        if self.instance.id != connection.id
            || self.instance.revision != connection.revision
            || super::registry::active(s, &connection.plugin_id)
                .await?
                .digest
                != self.package.digest
        {
            return Err(denied());
        }
        for dependency in &self.dependencies {
            Box::pin(dependency.authorize_inner(s, connection, snapshot.clone(), visited)).await?;
        }
        if let Some(snapshot) = snapshot {
            if snapshot.package.digest != self.package.digest {
                return Err(denied());
            }
            Box::pin(Invocation::prepare(
                s,
                snapshot,
                connection.clone(),
                self.kind,
                &self.name,
                &self.input,
            ))
            .await?;
        }
        Ok(())
    }
}

impl Invocation {
    pub(crate) fn transfer_origin(&self) -> Arc<TransferOrigin> {
        Arc::new(TransferOrigin {
            instance: self.instance.clone(),
            package: self.snapshot.package.clone(),
            kind: self.kind,
            name: self.name.clone(),
            input: self.input.clone(),
            dependencies: self.transfers.lock().unwrap().clone(),
        })
    }

    pub(crate) fn remember_transfer(&self, origin: Arc<TransferOrigin>) -> Result<()> {
        let mut transfers = self.transfers.lock().unwrap();
        if !transfers.iter().any(|known| Arc::ptr_eq(known, &origin)) {
            if transfers.len() >= 256 {
                return Err(denied());
            }
            transfers.push(origin);
        }
        Ok(())
    }

    pub(crate) async fn prepare(
        s: &Service,
        snapshot: Snapshot,
        instance: Connection,
        kind: Kind,
        name: &str,
        input: &Value,
    ) -> Result<Arc<Self>> {
        let invocation = Arc::new(Self {
            snapshot,
            instance,
            kind,
            name: name.into(),
            input: input.clone(),
            transfers: Arc::default(),
        });
        invocation.recheck(s).await?;
        Ok(invocation)
    }

    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        let this = self.clone();
        let grants = s
            .store
            .read(move |db| {
                this.snapshot.authorize(db)?;
                super::connections::check_revision(db, &this.instance)?;
                if !this.instance.enabled {
                    return Err(denied());
                }
                let mut agents = vec![this.snapshot.agent.clone()];
                if let Some((requester, _)) = &this.snapshot.requester {
                    agents.push(requester.clone());
                }
                agents
                    .into_iter()
                    .map(|agent| {
                        db.plugin_contribution(&agent, &this.instance.id, this.kind, &this.name)?
                            .map(|grant| (agent, grant))
                            .ok_or_else(denied)
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .await?;
        let package = &self.snapshot.package;
        if super::registry::active(s, &package.manifest.id)
            .await?
            .digest
            != package.digest
        {
            return Err(denied());
        }
        if let Some((requester, _)) = &self.snapshot.requester {
            super::grants::check_coverage_authorized(
                s,
                requester,
                &self.snapshot.agent,
                Some(self.snapshot.authority()),
            )
            .await?;
        }
        let fields = fields(&package.manifest, self.kind, &self.name)?;
        for (agent, grant) in grants {
            ResourceAuthorization {
                package,
                instance: &self.instance,
                agent: &agent,
                snapshot: self.snapshot.clone(),
                preparation: None,
            }
            .authorize(s, &grant, &fields, &self.input)
            .await?;
        }
        let snapshot = self.snapshot.clone();
        s.store.read(move |db| snapshot.authorize(db)).await
    }
}

pub(crate) fn fields(manifest: &Manifest, kind: Kind, name: &str) -> Result<Vec<ResourceField>> {
    Ok(match kind {
        Kind::Tool => manifest
            .tool(name)
            .ok_or_else(denied)?
            .resource_fields
            .clone(),
        Kind::Hook => manifest
            .hook(name)
            .ok_or_else(denied)?
            .resource_fields
            .clone(),
        Kind::Command => manifest
            .command(name)
            .ok_or_else(denied)?
            .arguments
            .iter()
            .filter_map(|arg| {
                arg.resource_type.as_ref().map(|kind| ResourceField {
                    path: format!("/{}", arg.name),
                    resource_type: kind.clone(),
                })
            })
            .collect(),
        _ => Vec::new(),
    })
}

fn descendant(manifest: &Manifest, kind: &str, ancestor: &str) -> bool {
    let mut next = Some(kind);
    for _ in 0..manifest.resources.len() {
        let Some(kind) = next else {
            return false;
        };
        if kind == ancestor {
            return true;
        }
        next = manifest
            .resource_type(kind)
            .and_then(|kind| kind.parent_type.as_deref());
    }
    false
}

fn designated<'a>(input: &'a Value, path: &str) -> Result<Vec<&'a str>> {
    let mut nodes = vec![input];
    for part in path.trim_start_matches('/').split('/') {
        let key = part.replace("~1", "/").replace("~0", "~");
        let mut next = Vec::new();
        for node in nodes {
            if part == "*" {
                if let Some(items) = node.as_array() {
                    next.extend(items);
                } else if !node.is_null() {
                    return Err(denied());
                }
            } else if let Some(value) = node.get(&key) {
                next.push(value);
            }
        }
        nodes = next;
        if nodes.len() > 200 {
            return Err(denied());
        }
    }
    nodes
        .into_iter()
        .map(|v| {
            v.as_str()
                .filter(|id| !id.is_empty() && id.len() <= 200 && !id.chars().any(char::is_control))
                .ok_or_else(denied)
        })
        .collect()
}

pub(crate) struct ResourceAuthorization<'a> {
    pub package: &'a Package,
    pub instance: &'a Connection,
    pub agent: &'a str,
    pub snapshot: Snapshot,
    pub preparation: Option<crate::capabilities::plugin_packages::Authority>,
}

impl ResourceAuthorization<'_> {
    async fn authorize(
        &self,
        s: &Service,
        grant: &PluginContributionGrant,
        fields: &[ResourceField],
        input: &Value,
    ) -> Result<()> {
        let Self {
            package,
            instance,
            agent,
            snapshot,
            preparation,
        } = self;
        if grant.resources.is_empty() {
            return Ok(());
        }
        let mut resolved = BTreeMap::<(String, String), Vec<PluginResourceRef>>::new();
        let types = grant
            .resources
            .iter()
            .map(|r| &r.resource_type)
            .collect::<BTreeSet<_>>();
        let mut matched = BTreeSet::new();
        let mut references = Vec::new();
        // Resolve all designations in the restricted hierarchy, including siblings
        // supplied together. Unrelated types (for example authors) remain unrestricted.
        for field in fields {
            let relevant = types
                .iter()
                .filter(|kind| descendant(&package.manifest, &field.resource_type, kind))
                .collect::<Vec<_>>();
            let ancestor = types
                .iter()
                .any(|kind| descendant(&package.manifest, kind, &field.resource_type));
            if relevant.is_empty() && !ancestor {
                continue;
            }
            for id in designated(input, &field.path)? {
                matched.extend(relevant.iter().map(|kind| (**kind).clone()));
                references.push(PluginResourceRef {
                    resource_type: field.resource_type.clone(),
                    id: id.into(),
                });
            }
        }
        if matched.len() != types.len() || references.len() > 200 {
            return Err(denied());
        }
        for reference in &references {
            let key = (reference.resource_type.clone(), reference.id.clone());
            if resolved.contains_key(&key) {
                continue;
            }
            let input = ResolvePluginResourceInput {
                plugin_id: package.manifest.id.clone(),
                instance_id: uuid::Uuid::parse_str(&instance.id)
                    .map(cairn_contracts::PluginInstanceId)
                    .map_err(|_| denied())?,
                resource_type: reference.resource_type.clone(),
                id: reference.id.clone(),
            };
            let read = super::resources::prepare_resolve(s, &input, Some(agent), None).await?;
            let read =
                read.with_snapshot(snapshot.clone(), (*instance).clone(), preparation.clone());
            let resource = Box::pin(read.resolve(s, input)).await?;
            let chain = std::iter::once(resource.resource)
                .chain(resource.ancestors)
                .map(|r| PluginResourceRef {
                    resource_type: r.resource_type,
                    id: r.id,
                })
                .collect::<Vec<_>>();
            for kind in &types {
                if descendant(&package.manifest, &reference.resource_type, kind)
                    && !chain.iter().any(|r| {
                        &r.resource_type == *kind
                            && grant.resources.iter().any(|allowed| {
                                allowed.resource_type == r.resource_type && allowed.id == r.id
                            })
                    })
                {
                    return Err(denied());
                }
            }
            resolved.insert(key, chain);
        }
        // A supplied channel and thread must name one consistent hierarchy. Multiple
        // resources of the same type may legitimately be selected (batch operations).
        for ((kind, _), chain) in &resolved {
            let ancestor_types = references
                .iter()
                .filter(|r| {
                    r.resource_type != *kind
                        && descendant(&package.manifest, kind, &r.resource_type)
                })
                .map(|r| &r.resource_type)
                .collect::<BTreeSet<_>>();
            for ancestor_type in ancestor_types {
                if !chain.iter().any(|parent| {
                    &parent.resource_type == ancestor_type
                        && references
                            .iter()
                            .any(|r| r.resource_type == parent.resource_type && r.id == parent.id)
                }) {
                    return Err(denied());
                }
            }
        }
        Ok(())
    }
}

/// Ticket F calls this before delivering an event. Unknown hooks, schema-invalid,
/// uncovered, undesignated and unresolved payload resources are refused.
pub async fn authorize_hook_delivery(
    s: &Service,
    agent: &str,
    instance_id: &str,
    hook: &str,
    payload: &Value,
) -> Result<()> {
    let instance = required(
        s.store.plugin_instance(instance_id).await?,
        "Plugin instance not found",
    )?;
    let package = super::registry::active(s, &instance.plugin_id).await?;
    let declaration = package.manifest.hook(hook).ok_or_else(denied)?;
    if !jsonschema::is_valid(&declaration.payload_schema, payload) {
        return Err(denied());
    }
    let snapshot = Snapshot::capture(s, agent, None, &package).await?;
    Invocation::prepare(s, snapshot, instance, Kind::Hook, hook, payload).await?;
    Ok(())
}

pub(crate) async fn contribution_scope(
    s: &Service,
    snapshot: Snapshot,
    instances: Vec<Connection>,
    kind: Kind,
    name: &str,
    input: &Value,
) -> Result<super::scope::Scope> {
    let mut allowed = Vec::new();
    let mut authorities = Vec::new();
    for instance in instances {
        match Invocation::prepare(s, snapshot.clone(), instance.clone(), kind, name, input).await {
            Ok(invocation) => {
                authorities.push((
                    instance.id.clone(),
                    super::management::Authority::agent_action(
                        instance.clone(),
                        snapshot.clone(),
                        Some(invocation),
                    ),
                ));
                allowed.push(instance);
            }
            Err(error) if matches!(error.status, 400 | 403 | 404 | 502) => {}
            Err(error) => return Err(error),
        }
    }
    if allowed.is_empty() {
        return Err(denied());
    }
    Ok(super::scope::Scope::Contributions(
        Box::new(super::scope::Scope::Agent {
            agent_id: snapshot.agent,
            instances: allowed,
        }),
        authorities,
    ))
}

pub(crate) async fn resource_set_covered(
    s: &Service,
    authority: &ResourceAuthorization<'_>,
    held: &PluginContributionGrant,
    proposed: &[PluginResourceRef],
) -> Result<()> {
    let mut input = serde_json::Map::new();
    let fields = proposed
        .iter()
        .enumerate()
        .map(|(index, resource)| {
            let key = format!("r{index}");
            input.insert(key.clone(), Value::String(resource.id.clone()));
            ResourceField {
                path: format!("/{key}"),
                resource_type: resource.resource_type.clone(),
            }
        })
        .collect::<Vec<_>>();
    authority
        .authorize(s, held, &fields, &Value::Object(input))
        .await
}
