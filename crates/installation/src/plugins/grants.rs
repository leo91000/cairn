//! Resource access is owner-managed. A tool/command pins one approved operation,
//! one resource and its input scopes before any package code can use credentials.
use super::{
    capabilities::{self, Context},
    connections::{self, Connection},
    registry::{self, Package},
    resources::{self, Resource},
    scope::Scope,
};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use cairn_plugin_api::{Manifest, Operation, Profile};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const KIND: &str = "plugin-resource-grants";

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    pub id: String,
    pub connection_id: String,
    pub resource_id: String,
    pub agent_id: String,
    pub profile: Option<Profile>,
    pub operations: Vec<String>,
    pub denied_operations: Vec<String>,
    pub scopes: BTreeMap<String, Vec<String>>,
    pub owner_access_id: String,
    pub revision: String,
    pub updated_at: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    agent_id: String,
    #[serde(default)]
    profile: Option<Profile>,
    #[serde(default)]
    operations: Vec<String>,
    #[serde(default)]
    denied_operations: Vec<String>,
    #[serde(default)]
    scopes: BTreeMap<String, Vec<String>>,
}

fn summary(grant: &Grant) -> Value {
    json!({"id": grant.id, "resourceId": grant.resource_id, "agentId": grant.agent_id,
        "profile": grant.profile, "operations": grant.operations, "deniedOperations": grant.denied_operations,
        "scopes": grant.scopes, "revision": grant.revision, "updatedAt": grant.updated_at})
}

fn permits(grant: &Grant, operation: &Operation, input: Option<&Value>) -> bool {
    if grant.denied_operations.contains(&operation.name)
        || !(grant
            .profile
            .is_some_and(|profile| profile >= operation.profile)
            || grant.operations.contains(&operation.name))
    {
        return false;
    }
    grant.scopes.iter().all(|(name, allowed)| {
        operation.scopes.contains(name)
            && input.is_none_or(|input| {
                input[name]
                    .as_str()
                    .is_some_and(|value| allowed.iter().any(|allowed| allowed == value))
            })
    })
}

pub(crate) fn agent_has_grants(db: &Db<'_>, agent: &str) -> Result<bool> {
    Ok(db.list(KIND)?.iter().any(|grant| grant["agentId"] == agent))
}

pub async fn create(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    account: &str,
    value: &Value,
) -> Result<Value> {
    let connection = connections::get(s, plugin, instance).await?;
    let resource = resources::get(s, plugin, instance, collection, resource).await?;
    let package = registry::active(s, plugin).await?;
    if package.manifest.agent_access.is_none() {
        return Err(Error::bad("This package declares no agent access."));
    }
    let input: Input =
        serde_json::from_value(value.clone()).map_err(|_| Error::bad("Invalid resource grant."))?;
    uuid(&input.agent_id)?;
    let operations = package
        .manifest
        .operations
        .iter()
        .filter(|operation| operation.collection == collection)
        .collect::<Vec<_>>();
    if operations.is_empty()
        || (input.profile.is_none() && input.operations.is_empty())
        || input.operations.len() > 128
        || input.denied_operations.len() > 128
        || input
            .operations
            .iter()
            .chain(&input.denied_operations)
            .any(|name| !operations.iter().any(|operation| &operation.name == name))
        || input.scopes.len() > 8
        || !input.scopes.iter().all(|(name, values)| {
            operations
                .iter()
                .any(|operation| operation.scopes.contains(name))
                && !values.is_empty()
                && values.len() <= 100
                && values.iter().all(|value| {
                    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
                })
        })
    {
        return Err(Error::bad(
            "Choose declared operations and non-empty scope limits.",
        ));
    }
    let owner = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::forbidden("The installation has no verified owner."))?
        .owner;
    if owner.account_id != account {
        return Err(Error::forbidden(
            "Only the current installation owner can grant resource access.",
        ));
    }
    let grant = Grant {
        id: id(),
        connection_id: instance.to_owned(),
        resource_id: resource.id.clone(),
        agent_id: input.agent_id,
        profile: input.profile,
        operations: input.operations,
        denied_operations: input.denied_operations,
        scopes: input.scopes,
        owner_access_id: owner.access_id,
        revision: id(),
        updated_at: now(),
    };
    s.store
        .transaction(move |db| {
            let current = db.get(connections::KIND, &connection.id)?;
            let current_resource = db.get(resources::KIND, &resource.id)?;
            if !current.is_some_and(|value| value["revision"] == connection.revision)
                || !current_resource.is_some_and(|value| value["revision"] == resource.revision)
            {
                return Err(Error::conflict(
                    "The resource changed. Reload before granting access.",
                ));
            }
            if db.get("agents", &grant.agent_id)?.is_none() {
                return Err(Error::bad("Agent not found."));
            }
            if db.list(connections::KIND)?.iter().any(|value| {
                value["agentId"] == grant.agent_id && value["accessMode"] == "everyone"
            }) {
                return Err(Error::bad(
                    "An agent reachable by anyone cannot receive resource credentials.",
                ));
            }
            if db
                .list(KIND)?
                .iter()
                .filter(|value| value["resourceId"] == resource.id)
                .count()
                >= 64
            {
                return Err(Error::too_large(
                    "A resource can have at most 64 grant rules.",
                ));
            }
            db.put(KIND, &serde_json::to_value(&grant)?)?;
            db.audit(
                "plugin.resource.granted",
                &json!({"id": grant.id, "connectionId": connection.id, "agentId": grant.agent_id}),
            )?;
            Ok(summary(&grant))
        })
        .await
}

pub async fn list(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
) -> Result<Value> {
    resources::get(s, plugin, instance, collection, resource).await?;
    let values = s
        .store
        .list(KIND)
        .await?
        .into_iter()
        .filter(|value| value["resourceId"] == resource && value["connectionId"] == instance)
        .map(serde_json::from_value::<Grant>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(json!({"items": values.iter().map(summary).collect::<Vec<_>>()}))
}

pub async fn remove(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    grant: &str,
) -> Result<Value> {
    resources::get(s, plugin, instance, collection, resource).await?;
    uuid(grant)?;
    let (instance, resource, grant) = (instance.to_owned(), resource.to_owned(), grant.to_owned());
    s.store
        .transaction(move |db| {
            let value = db
                .get(KIND, &grant)?
                .ok_or_else(|| Error::not_found("Resource grant not found."))?;
            if value["connectionId"] != instance || value["resourceId"] != resource {
                return Err(Error::not_found("Resource grant not found."));
            }
            db.remove(KIND, &grant)?;
            db.audit(
                "plugin.resource.revoked",
                &json!({"id": grant, "connectionId": instance}),
            )?;
            Ok(json!({"deleted": true}))
        })
        .await
}

pub(crate) struct Catalog {
    grants: Vec<Grant>,
    resources: HashMap<String, Resource>,
}

impl Catalog {
    pub async fn load(s: &Service, agent: &str) -> Result<Self> {
        let mut grants = s
            .store
            .list(KIND)
            .await?
            .into_iter()
            .filter(|value| value["agentId"] == agent)
            .map(serde_json::from_value::<Grant>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if grants.is_empty() {
            return Ok(Self {
                grants,
                resources: HashMap::new(),
            });
        }
        let owner = crate::relay::task_author_policy(s).await?;
        grants.retain(|grant| {
            owner
                .as_ref()
                .is_some_and(|owner| owner.owner.access_id == grant.owner_access_id)
        });
        let resources = s
            .store
            .list(resources::KIND)
            .await?
            .into_iter()
            .map(serde_json::from_value::<Resource>)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|resource| (resource.id.clone(), resource))
            .collect();
        Ok(Self { grants, resources })
    }

    fn enabled(&self, resource: &Resource) -> bool {
        let mut current = resource;
        let mut seen = HashSet::new();
        loop {
            if !current.enabled
                || current.connection_id != resource.connection_id
                || !seen.insert(&current.id)
            {
                return false;
            }
            let Some(parent) = &current.parent_id else {
                return true;
            };
            let Some(parent) = self.resources.get(parent) else {
                return false;
            };
            current = parent;
        }
    }

    pub fn instances(&self) -> HashSet<String> {
        self.grants
            .iter()
            .filter_map(|grant| self.resources.get(&grant.resource_id))
            .filter(|resource| self.enabled(resource))
            .map(|resource| resource.connection_id.clone())
            .collect()
    }

    pub fn operations(&self, manifest: &Manifest, instances: &[Connection]) -> HashSet<String> {
        manifest
            .operations
            .iter()
            .filter(|operation| {
                self.grants.iter().any(|grant| {
                    self.resources
                        .get(&grant.resource_id)
                        .is_some_and(|resource| {
                            self.enabled(resource)
                                && resource.collection == operation.collection
                                && instances
                                    .iter()
                                    .any(|instance| instance.id == resource.connection_id)
                                && permits(grant, operation, None)
                        })
                })
            })
            .map(|operation| operation.name.clone())
            .collect()
    }
}

#[derive(Clone)]
pub struct Authorization {
    pub connection: Connection,
    pub resource: Resource,
    pub operation: Operation,
    pub scopes: BTreeMap<String, String>,
    grant: Grant,
    reference: resources::Reference,
    pub(crate) requester: Option<super::requesters::RunRequester>,
}

pub async fn authorize(
    s: &Service,
    run: &Value,
    package: &Package,
    name: &str,
    input: &Value,
) -> Result<Scope> {
    let agent = text(&run["snapshot"]["agent"], "id");
    let denied =
        || Error::forbidden("This agent is not granted that resource operation and scope.");
    let operation = package
        .manifest
        .operations
        .iter()
        .find(|operation| operation.name == name)
        .ok_or_else(denied)?
        .clone();
    let catalog = Catalog::load(s, agent).await?;
    let resource = catalog
        .resources
        .get(text(input, "resource"))
        .ok_or_else(denied)?
        .clone();
    if resource.collection != operation.collection || !catalog.enabled(&resource) {
        return Err(denied());
    }
    let grant = catalog
        .grants
        .iter()
        .find(|grant| grant.resource_id == resource.id && permits(grant, &operation, Some(input)))
        .ok_or_else(denied)?
        .clone();
    let connection = connections::get(s, &package.manifest.id, &resource.connection_id).await?;
    if !connection.enabled {
        return Err(denied());
    }
    let scopes = operation
        .scopes
        .iter()
        .map(|name| {
            let value = input[name]
                .as_str()
                .filter(|value| {
                    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
                })
                .ok_or_else(denied)?;
            Ok((name.clone(), value.to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let context = Context {
        connection: connection.clone(),
        manifest: package.manifest.clone(),
        event: Value::Null,
        link: None,
        draft: None,
        resource_grant: None,
    };
    let reference = resources::reference(s, &context, &resource.id).await?;
    let authorization = Authorization {
        connection,
        resource,
        operation,
        scopes,
        grant,
        reference,
        requester: super::requesters::for_run(s, run).await?,
    };
    recheck(s, &authorization).await?;
    Ok(Scope::ResourceAgent(Box::new(authorization)))
}

pub async fn recheck(s: &Service, authorization: &Authorization) -> Result<()> {
    if let Some(requester) = &authorization.requester {
        super::requesters::recheck(s, requester).await?;
    }
    let owner = crate::relay::task_author_policy(s).await?;
    if !owner.is_some_and(|owner| owner.owner.access_id == authorization.grant.owner_access_id) {
        return Err(Error::forbidden(
            "The resource grant changed or was revoked.",
        ));
    }
    let expected = authorization.connection.clone();
    let grant = authorization.grant.clone();
    let resource = authorization.resource.clone();
    s.store
        .read(move |db| {
            connections::authorize(db, &expected)?;
            if db.get("agents", &grant.agent_id)?.is_none()
                || !db
                    .get(KIND, &grant.id)?
                    .is_some_and(|value| value["revision"] == grant.revision)
            {
                return Err(Error::forbidden(
                    "The resource grant changed or was revoked.",
                ));
            }
            if !db
                .get(resources::KIND, &resource.id)?
                .is_some_and(|value| value["revision"] == resource.revision)
            {
                return Err(Error::forbidden(
                    "The resource changed during the operation.",
                ));
            }
            Ok(())
        })
        .await?;
    let context = Context {
        connection: authorization.connection.clone(),
        manifest: registry::active(s, &authorization.connection.plugin_id)
            .await?
            .manifest,
        event: Value::Null,
        link: None,
        draft: None,
        resource_grant: None,
    };
    resources::recheck(s, &context, &authorization.reference).await
}

fn request_permitted(authorization: &Authorization, args: &Value) -> bool {
    if args["auth"]["resource"] != authorization.resource.id {
        return false;
    }
    if args
        .get("operation")
        .is_some_and(|value| value != &authorization.operation.name)
    {
        return false;
    }
    let Ok(url) = url::Url::parse(text(args, "url")) else {
        return false;
    };
    let method = args["method"].as_str().unwrap_or("GET");
    authorization.operation.requests.iter().any(|rule| {
        if !rule.bindings.is_empty() {
            if args.get("body").is_some_and(|value| !value.is_null())
                || args.get("multipart").is_some_and(|value| !value.is_null())
            {
                return false;
            }
            let form = args.get("form").is_some_and(|value| !value.is_null());
            let json = args.get("json").is_some_and(|value| !value.is_null());
            if form && json {
                return false;
            }
            if form || json {
                let content_types = args["headers"]
                    .as_object()
                    .into_iter()
                    .flat_map(|headers| headers.iter())
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                    .map(|(_, value)| value.as_str().unwrap_or_default())
                    .collect::<Vec<_>>();
                if content_types.len() > 1
                    || content_types.first().is_some_and(|value| {
                        !value
                            .split(';')
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .eq_ignore_ascii_case(if form {
                                "application/x-www-form-urlencoded"
                            } else {
                                "application/json"
                            })
                    })
                {
                    return false;
                }
            }
        }
        let path = rule.path.strip_suffix('*').map_or_else(
            || url.path() == rule.path,
            |prefix| url.path().starts_with(prefix),
        );
        rule.method == method
            && url.host_str() == Some(rule.host.as_str())
            && path
            && rule.bindings.iter().all(|(field, scope)| {
                let mut values = url
                    .query_pairs()
                    .filter(|(name, _)| name == field)
                    .map(|(_, value)| value.into_owned())
                    .collect::<Vec<_>>();
                for body in ["json", "form"] {
                    if let Some(value) = args[body].get(field) {
                        let Some(value) = value.as_str() else {
                            return false;
                        };
                        values.push(value.to_owned());
                    }
                }
                let expected = if let Some(key) = scope.strip_prefix("resource.") {
                    authorization.resource.settings[key].as_str()
                } else {
                    authorization.scopes.get(scope).map(String::as_str)
                };
                values.len() == 1 && expected == values.first().map(String::as_str)
            })
    })
}

pub async fn call(
    s: &Service,
    package: &Package,
    authorization: &Authorization,
    method: &str,
    args: &Value,
) -> Result<Value> {
    let denied = || {
        Error::forbidden("This capability or request is outside the granted resource operation.")
    };
    let result = match method {
        "resources.get"
            if args["resource"] == authorization.resource.id
                && args["collection"] == authorization.resource.collection =>
        {
            serde_json::to_value(&authorization.resource)?
        }
        "resources.list" if args["collection"] == authorization.resource.collection => {
            json!({"items": [&authorization.resource], "nextCursor": null})
        }
        "http.request" if request_permitted(authorization, args) => {
            let context = Context {
                connection: authorization.connection.clone(),
                manifest: package.manifest.clone(),
                event: Value::Null,
                link: None,
                draft: None,
                resource_grant: Some(std::sync::Arc::new(authorization.clone())),
            };
            capabilities::effect(s, &context, method, args.clone()).await?
        }
        _ => return Err(denied()),
    };
    recheck(s, authorization).await?;
    if registry::active(s, &package.manifest.id).await?.digest != package.digest {
        return Err(Error::forbidden("The plugin approval changed."));
    }
    Ok(result)
}
