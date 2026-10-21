//! Capabilities for one-shot handlers (commands, tools, views, actions, tests). The
//! scope names who acts and which instances' storage, credentials and secrets the
//! script may use; every call rereads the instance first.
use super::{
    capabilities::{self, Context},
    connections::{self, Connection},
    registry::Package,
};
use crate::{
    error::{Error, Result},
    service::Service,
};
use cairn_plugin_host::{Denied, InvocationError};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub enum Scope {
    Managed(Box<Scope>, super::management::Authority),
    /// The owner, through one instance: views, actions and tests.
    Instance(Box<Connection>),
    /// The owner editing an instance, before it is saved: its form. Only `http` and
    /// `exec`, with the draft's credential and secrets.
    Draft(Box<Connection>, Arc<super::forms::Draft>),
    /// A run's agent, through the enabled instances it is granted: commands and tools.
    Agent {
        agent_id: String,
        instances: Vec<Connection>,
    },
}

impl Scope {
    /// What the script receives about the instances it may use. Never secret values.
    pub fn instances(&self) -> Vec<Value> {
        match self {
            Scope::Managed(scope, _) => scope.instances(),
            Scope::Instance(connection) | Scope::Draft(connection, _) => {
                vec![summary(connection)]
            }
            Scope::Agent { instances, .. } => instances.iter().map(summary).collect(),
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
        let authority = authority.clone();
        s.store.read(move |db| authority.authorize(db)).await?;
        inner.as_ref()
    } else {
        scope
    };
    let requested = args["instance"].as_str();
    if let Scope::Draft(connection, _) = scope {
        if requested.is_some_and(|id| id != connection.id) {
            return Err(Error::forbidden("Another instance is not available here."));
        }
        return Ok((**connection).clone());
    }
    let expected = match scope {
        Scope::Managed(..) => unreachable!(),
        Scope::Draft(..) => unreachable!(),
        Scope::Instance(connection) => {
            if requested.is_some_and(|id| id != connection.id) {
                return Err(Error::forbidden("Another instance is not available here."));
            }
            connection
        }
        Scope::Agent { instances, .. } => match requested {
            Some(id) => instances
                .iter()
                .find(|connection| connection.id == id)
                .ok_or_else(|| Error::forbidden("This agent is not granted that instance."))?,
            None if instances.len() == 1 => &instances[0],
            None => return Err(Error::bad("Name the instance to use.")),
        },
    };
    let value = s
        .store
        .get(connections::KIND, &expected.id)
        .await?
        .ok_or_else(|| Error::forbidden("The instance was removed."))?;
    let connection: Connection = serde_json::from_value(value)?;
    let unchanged = connection.revision == expected.revision;
    let granted = match scope {
        Scope::Managed(..) => unreachable!(),
        Scope::Instance(_) | Scope::Draft(..) => true,
        Scope::Agent { agent_id, .. } => connection.grants(agent_id),
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
    let connection = current(s, scope, args).await?;
    let (scope, management) = match scope {
        Scope::Managed(inner, authority) => (inner.as_ref(), Some(authority.clone())),
        _ => (scope, None),
    };
    let draft = match scope {
        Scope::Draft(_, draft) => Some(draft.clone()),
        _ => None,
    };
    if draft.is_some() && !matches!(method, "http.request" | "exec.run") {
        return Err(Error::forbidden(
            "An instance being edited can only use http and exec.",
        ));
    }
    let context = Context {
        connection,
        manifest: package.manifest.clone(),
        event: Value::Null,
        link: None,
        draft,
        management: management.clone(),
    };
    if ["conversations.", "inbound.", "instance.", "socket."]
        .iter()
        .any(|family| method.starts_with(family))
    {
        return Err(Error::forbidden(
            "Conversations and connections belong to the instance's worker.",
        ));
    }
    let result = capabilities::effect(s, &context, method, args.clone()).await?;
    // Revocation or a changed secret during slow I/O must not deliver its result.
    if (matches!(scope, Scope::Agent { .. }) || management.is_some())
        && (matches!(method, "exec.run" | "http.request") || method.starts_with("files."))
    {
        current(s, scope, args).await?;
        if let Some(authority) = management {
            s.store.read(move |db| authority.authorize(db)).await?;
        }
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
                call(s, package, scope, &method, &args)
                    .await
                    .map_err(|error| {
                        let code = capabilities::code(&error);
                        *refused_ref.lock().unwrap() = Some(error);
                        Denied(code)
                    })
            },
        )
        .await;
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
