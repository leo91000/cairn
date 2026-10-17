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
};
use cairn_plugin_host::Denied;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub enum Scope {
    /// The owner, through one instance: views, actions and tests.
    Instance(Box<Connection>),
    /// A signed public request. It has no Cairn account or owner authority.
    Ingress(Box<Connection>),
    ResourceAgent(Box<super::grants::Authorization>),
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
            Scope::Instance(connection)
            | Scope::Ingress(connection)
            | Scope::Draft(connection, _) => {
                vec![summary(connection)]
            }
            Scope::Agent { instances, .. } => instances.iter().map(summary).collect(),
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
    let requested = args["instance"].as_str();
    if let Scope::ResourceAgent(authorization) = scope {
        if requested.is_some_and(|id| id != authorization.connection.id) {
            return Err(Error::forbidden("Another instance is not available here."));
        }
        super::grants::recheck(s, authorization).await?;
        return Ok(authorization.connection.clone());
    }
    if let Scope::Draft(connection, _) = scope {
        if requested.is_some_and(|id| id != connection.id) {
            return Err(Error::forbidden("Another instance is not available here."));
        }
        return Ok((**connection).clone());
    }
    let expected = match scope {
        Scope::Draft(..) | Scope::ResourceAgent(_) => unreachable!(),
        Scope::Instance(connection) | Scope::Ingress(connection) => {
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
        Scope::Instance(_) | Scope::Draft(..) => true,
        Scope::Ingress(_) => connection.enabled,
        Scope::ResourceAgent(_) => unreachable!(),
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
    if registry::active(s, &package.manifest.id).await?.digest != package.digest {
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
    if matches!(scope, Scope::Agent { .. })
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
    };
    if matches!(scope, Scope::Ingress(_)) && matches!(method, "inbound.accept" | "inbound.deliver")
    {
        return capabilities::checked(s, &context, method, args.clone()).await;
    }
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
    if matches!(scope, Scope::Agent { .. }) && matches!(method, "exec.run" | "http.request") {
        current(s, scope, args).await?;
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
    let result = s
        .plugins
        .host
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
    if result.is_ok() {
        match scope {
            Scope::Agent { instances, .. } => {
                for instance in instances {
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
        (Err(error), _) if error.starts_with(cairn_plugin_api::REJECTED) => {
            Err(Error::bad_gateway(
                error
                    .trim_start_matches(cairn_plugin_api::REJECTED)
                    .to_owned(),
            ))
        }
        (Err(_), Some(error)) => Err(error),
        (Err(_), None) => Err(Error::bad_gateway(format!(
            "The {} plugin failed while handling {event}.",
            package.manifest.name
        ))),
    }
}
