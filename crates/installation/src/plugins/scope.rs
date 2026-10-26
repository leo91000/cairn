//! Capabilities for one-shot handlers (commands, tools, views, actions, tests). The
//! scope names who acts and which instances' storage, credentials and secrets the
//! script may use; every call rereads the instance first.
use super::{
    capabilities::{self, Context},
    connections::{self, Connection},
    registry::{self, Package},
};
use crate::{
    error::{Error, Result},
    service::Service,
    validation::text,
};
use cairn_plugin_host::{Denied, InvocationError};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub enum Scope {
    Managed(Box<Scope>, Box<super::management::Authority>),
    /// The owner, through one instance: views, actions and tests.
    Instance(Box<Connection>, Arc<super::operations::Owner>),
    /// A signed public request. It has no Cairn account or owner authority.
    Ingress(Box<Connection>),
    /// A signed setup challenge. Pure Luau only, with no operational capabilities.
    Setup(Box<super::setup::SetupDraft>),
    ResourceAgent(Box<super::grants::Authorization>),
    /// The owner editing an instance, before it is saved: its form. Only `http` and
    /// `exec`, with the draft's credential and secrets.
    Draft(Box<Connection>, Arc<super::forms::Draft>),
    /// A run's agent, through the enabled instances it is granted: commands and tools.
    Agent(Box<Agent>),
}

/// Host-only proof of both the issuing run and its original requester. The target
/// instance's agent grant cannot replace a revoked source participant grant.
#[derive(Clone)]
pub struct Agent {
    agent_id: String,
    instances: Vec<Connection>,
    bearer: String,
    requester: Option<super::requesters::RunRequester>,
    observation: Option<Arc<super::requesters::Observation>>,
}

/// Opaque host proof retained by a handler through its effects and delayed writes.
/// It cannot be supplied by a package or reconstructed from its arguments.
#[derive(Clone)]
pub struct LocalAuthority {
    agent: Option<Agent>,
    approval: registry::Approval,
    digest: String,
}

impl LocalAuthority {
    pub(super) fn for_package(approved: &registry::ApprovedPackage) -> Self {
        Self {
            agent: None,
            approval: approved.snapshot(),
            digest: approved.package.digest.clone(),
        }
    }

    pub(super) async fn recheck_loaded(&self, s: &Service, digest: &str) -> Result<()> {
        if self.digest != digest {
            return Err(Error::forbidden("The loaded plugin package changed."));
        }
        self.recheck(s).await
    }

    pub(super) async fn recheck(&self, s: &Service) -> Result<()> {
        if let Some(agent) = &self.agent {
            agent.recheck(s).await?;
        }
        self.approval.recheck(s, &self.digest).await?;
        let expected = self.clone();
        s.store.read(move |db| expected.recheck_in(db)).await
    }

    pub(super) fn recheck_in(&self, db: &crate::store::Db<'_>) -> Result<()> {
        self.approval.recheck_in(db)?;
        if let Some(agent) = &self.agent {
            agent.recheck_in(db)?;
        }
        Ok(())
    }
}

impl Agent {
    pub(crate) async fn recheck(&self, s: &Service) -> Result<()> {
        if let Some(observation) = &self.observation {
            observation.recheck(s).await?;
        }
        if let Some(requester) = &self.requester {
            super::requesters::recheck(s, requester).await?;
        }
        Ok(())
    }

    /// A condition's result remains bound to the source and instance snapshots
    /// supplied to its handler, including while publication waits for the writer.
    pub(crate) fn recheck_in(&self, db: &crate::store::Db<'_>) -> Result<()> {
        if let Some(observation) = &self.observation {
            observation.recheck_in(db)?;
        } else {
            crate::project_workspaces::authorize_in(db, &self.bearer)?;
        }
        if let Some(requester) = &self.requester {
            super::requesters::recheck_in(db, requester)?;
        }
        for instance in &self.instances {
            let current = connections::authorize(db, instance)?;
            if !current.grants(&self.agent_id) {
                return Err(Error::forbidden("The condition's instance grant changed."));
            }
        }
        Ok(())
    }
}

impl Scope {
    pub async fn for_agent(
        s: &Service,
        run: &Value,
        instances: Vec<Connection>,
        bearer: &str,
    ) -> Result<Self> {
        let requester = super::requesters::for_run(s, run).await?;
        if let Some(super::requesters::Principal::External(actor)) =
            requester.as_ref().map(|requester| &requester.principal)
        {
            let actor = actor.clone();
            let origin = s
                .store
                .read(move |db| super::requesters::external_in(db, &actor))
                .await?;
            if instances
                .iter()
                .any(|instance| instance.plugin_id == origin.plugin_id)
            {
                return Err(Error::forbidden(
                    "Use a resource operation within the requester's admitted source scope.",
                ));
            }
        }
        Ok(Self::Agent(Box::new(Agent {
            agent_id: text(&run["snapshot"]["agent"], "id").to_owned(),
            instances,
            bearer: bearer.to_owned(),
            requester,
            observation: None,
        })))
    }

    /// An internal wait probe has no running bearer, but retains its original turn.
    pub(crate) async fn for_condition(
        s: &Service,
        run: &Value,
        condition: &Value,
        instances: Vec<Connection>,
    ) -> Result<Self> {
        let observation = super::requesters::Observation::capture(s, run, condition).await?;
        if let Some(super::requesters::Principal::External(actor)) = &observation.principal {
            let actor = actor.clone();
            let origin = s
                .store
                .read(move |db| super::requesters::external_in(db, &actor))
                .await?;
            if instances
                .iter()
                .any(|instance| instance.plugin_id == origin.plugin_id)
            {
                return Err(Error::forbidden(
                    "Use a resource operation within the requester's admitted source scope.",
                ));
            }
        }
        Ok(Self::Agent(Box::new(Agent {
            agent_id: text(&run["snapshot"]["agent"], "id").to_owned(),
            instances,
            bearer: String::new(),
            requester: None,
            observation: Some(Arc::new(observation)),
        })))
    }

    /// What the script receives about the instances it may use. Never secret values.
    pub fn instances(&self) -> Vec<Value> {
        match self {
            Scope::Managed(scope, _) => scope.instances(),
            Scope::Instance(connection, _)
            | Scope::Ingress(connection)
            | Scope::Draft(connection, _) => {
                vec![summary(connection)]
            }
            Scope::Setup(draft) => vec![summary(&draft.connection())],
            Scope::Agent(agent) => agent.instances.iter().map(summary).collect(),
            Scope::ResourceAgent(authorization) => vec![summary(&authorization.connection)],
        }
    }
}

pub fn summary(connection: &Connection) -> Value {
    json!({
        "id": connection.id,
        "name": connection.name,
        "enabled": connection.enabled,
        "settings": connection.settings,
        "secrets": connection.secret_keys,
    })
}

/// Rereads the target instance. Owner scopes need it unchanged; agent scopes also need
/// it enabled and still granting the agent.
async fn current(s: &Service, scope: &Scope, args: &Value) -> Result<Connection> {
    let scope = if let Scope::Managed(inner, authority) = scope {
        authority.recheck(s).await?;
        inner.as_ref()
    } else {
        scope
    };
    match scope {
        Scope::Instance(_, owner) => owner.recheck(s).await?,
        Scope::Draft(_, values) => {
            if let Some(resource) = &values.resource {
                resource.recheck(s).await?;
            }
            if let Some(owner) = &values.owner {
                owner.recheck(s).await?;
            }
            if let Some(root) = &values.root {
                let root = root.clone();
                s.store.read(move |db| root.current(db)).await?;
            }
        }
        Scope::Agent(agent) => {
            if let Some(observation) = &agent.observation {
                observation.recheck(s).await?;
            }
            if let Some(requester) = &agent.requester {
                super::requesters::recheck(s, requester).await?;
            }
        }
        _ => {}
    }
    let requested = args["instance"].as_str();
    if let Scope::Setup(expected) = scope {
        if requested.is_some_and(|id| id != expected.id) {
            return Err(Error::forbidden(
                "Another setup draft is not available here.",
            ));
        }
        let draft = super::setup::load(s, &expected.id).await?;
        if draft.revision != expected.revision {
            return Err(Error::forbidden("Setup changed. Retry its challenge."));
        }
        return Ok(draft.connection());
    }
    if let Scope::ResourceAgent(authorization) = scope {
        if requested.is_some_and(|id| id != authorization.connection.id) {
            return Err(Error::forbidden("Another instance is not available here."));
        }
        super::grants::recheck(s, authorization).await?;
        return Ok(authorization.connection.clone());
    }
    if let Scope::Draft(connection, values) = scope {
        if requested.is_some_and(|id| id != connection.id) {
            return Err(Error::forbidden("Another instance is not available here."));
        }
        if values.resource.is_some() {
            return Ok((**connection).clone());
        }
        if let Some(expected) = &values.setup {
            let draft = super::setup::load(s, &expected.id).await?;
            if draft.revision != expected.revision {
                return Err(Error::conflict("Setup changed. Reload its form."));
            }
        }
        return Ok((**connection).clone());
    }
    let expected = match scope {
        Scope::Managed(..) | Scope::Draft(..) | Scope::ResourceAgent(_) | Scope::Setup(_) => {
            unreachable!()
        }
        Scope::Instance(connection, _) | Scope::Ingress(connection) => {
            if requested.is_some_and(|id| id != connection.id) {
                return Err(Error::forbidden("Another instance is not available here."));
            }
            connection
        }
        Scope::Agent(agent) => match requested {
            Some(id) => agent
                .instances
                .iter()
                .find(|connection| connection.id == id)
                .ok_or_else(|| Error::forbidden("This agent is not granted that instance."))?,
            None if agent.instances.len() == 1 => &agent.instances[0],
            None => return Err(Error::bad("Name the instance to use.")),
        },
    };
    let id = expected.id.clone();
    let authority = match scope {
        Scope::Agent(agent) => Some((
            agent.bearer.clone(),
            agent.requester.clone(),
            agent.observation.clone(),
        )),
        _ => None,
    };
    let value = s
        .store
        .read(move |db| {
            if let Some((bearer, requester, observation)) = authority {
                if let Some(observation) = observation {
                    observation.recheck_in(db)?;
                } else {
                    crate::project_workspaces::authorize_in(db, &bearer)?;
                }
                if let Some(requester) = requester {
                    super::requesters::recheck_in(db, &requester)?;
                }
            }
            db.get(connections::KIND, &id)
        })
        .await?
        .ok_or_else(|| Error::forbidden("The instance was removed."))?;
    let connection: Connection = serde_json::from_value(value)?;
    let unchanged = connection.revision == expected.revision;
    let granted = match scope {
        Scope::Instance(..) | Scope::Draft(..) => true,
        Scope::Ingress(_) => connection.enabled,
        Scope::Managed(..) | Scope::ResourceAgent(_) | Scope::Setup(_) => unreachable!(),
        Scope::Agent(agent) => connection.grants(&agent.agent_id),
    };
    if !unchanged || !granted {
        return Err(Error::forbidden(
            "The instance or its grants changed. Retry the operation.",
        ));
    }
    Ok(connection)
}

/// One capability call from a one-shot handler, after the manifest gate.
pub async fn call(
    s: &Service,
    package: &Package,
    scope: &Scope,
    method: &str,
    args: &Value,
) -> Result<Value> {
    let original_scope = scope;
    let (scope, management) = match scope {
        Scope::Managed(inner, authority) => (inner.as_ref(), Some(authority.as_ref().clone())),
        _ => (scope, None),
    };
    if matches!(scope, Scope::Setup(_)) {
        return Err(Error::forbidden(
            "Setup challenges cannot use operational capabilities.",
        ));
    }
    let connection = current(s, original_scope, args).await?;
    let approved = registry::approved(s, &package.manifest.id).await?;
    if approved.package.digest != package.digest {
        return Err(Error::forbidden("The plugin approval changed."));
    }
    if let Scope::ResourceAgent(authorization) = scope {
        return super::grants::call(s, package, authorization, method, args).await;
    }
    let draft = match scope {
        Scope::Draft(_, draft) => Some(draft.clone()),
        _ => None,
    };
    if draft.is_some() && !matches!(method, "http.request" | "exec.run") {
        return Err(Error::forbidden(
            "An instance being edited can only use http and exec.",
        ));
    }
    if matches!(scope, Scope::Agent(_))
        && (method.starts_with("resources.") || args["auth"].get("resource").is_some())
    {
        return Err(Error::forbidden(
            "This operation requires a resource grant.",
        ));
    }
    let context = Context {
        connection,
        manifest: package.manifest.clone(),
        event: Value::Null,
        link: None,
        draft,
        resource_grant: None,
        management: management.clone(),
        local_authority: match scope {
            Scope::Agent(agent) => Some(Arc::new(LocalAuthority {
                agent: Some(agent.as_ref().clone()),
                approval: approved.snapshot(),
                digest: approved.package.digest.clone(),
            })),
            _ => None,
        },
    };
    if matches!(scope, Scope::Ingress(_)) && matches!(method, "inbound.accept" | "inbound.deliver")
    {
        return capabilities::approved(s, &context, &package.digest, method, args.clone()).await;
    }
    if [
        "conversations.",
        "inbound.",
        "instance.",
        "socket.",
        "oauth.",
    ]
    .iter()
    .any(|family| method.starts_with(family))
    {
        return Err(Error::forbidden(
            "Conversations and connections belong to the instance's worker.",
        ));
    }
    let result = capabilities::effect(s, &context, method, args.clone(), || async {
        current(s, original_scope, args).await?;
        registry::require_digest(s, &package.manifest.id, &package.digest).await
    })
    .await?;
    // Revocation or a changed secret during slow I/O must not deliver its result.
    let bound_form = matches!(scope, Scope::Draft(_, values) if values.setup.is_some() || values.resource.is_some() || values.root.is_some());
    if (matches!(scope, Scope::Agent(_) | Scope::Instance(..))
        || bound_form
        || management.is_some())
        && (matches!(method, "exec.run" | "http.request") || method.starts_with("files."))
    {
        current(s, original_scope, args).await?;
    }
    Ok(result)
}

/// Runs one handler in a fresh worker. Host refusals reach the caller with their
/// message; the script only ever sees short codes.
pub async fn invoke(
    s: &Service,
    package: &Package,
    event: &str,
    payload: Value,
    scope: &Scope,
) -> Result<Value> {
    let refused = Mutex::new(None::<Error>);
    let refused_ref = &refused;
    let preparing = super::program_installation::needs_preparation(s, package).then(|| {
        s.plugins
            .host
            .with_deadline(std::time::Duration::from_secs(180))
    });
    let host = preparing.as_ref().unwrap_or(&s.plugins.host);
    let result = host
        .invoke(
            &s.plugins.executable()?,
            &package.manifest,
            &package.source,
            event,
            payload,
            |method, args| async move {
                // Nested forms keep host-effect state off the caller's stack.
                Box::pin(call(s, package, scope, &method, &args))
                    .await
                    .map_err(|error| {
                        let code = capabilities::code(&error);
                        *refused_ref.lock().unwrap() = Some(error);
                        Denied(code)
                    })
            },
        )
        .await;
    if result.is_ok() {
        match scope {
            Scope::Agent(agent) => {
                for instance in &agent.instances {
                    current(s, scope, &json!({"instance": instance.id})).await?;
                }
            }
            _ => {
                current(s, scope, &Value::Null).await?;
            }
        }
        if registry::active(s, &package.manifest.id).await?.digest != package.digest {
            return Err(Error::forbidden("The plugin approval changed."));
        }
    }
    match (result, refused.into_inner().unwrap()) {
        (Ok(value), _) => Ok(value),
        (Err(InvocationError::Rejected(message)), _) => Err(Error::bad_gateway(message)),
        (Err(_), Some(error)) => Err(error),
        (Err(_), None) => Err(Error::bad_gateway(format!(
            "The {} plugin failed while handling {event}.",
            package.manifest.name
        ))),
    }
}
