//! Owner-approved delegation for a resource. The host selects a route from source
//! context before storing content; packages never supply its agent or authority.
use super::{
    connections::{self, AccessMode, Connection, Label},
    resources::{self, Resource, target::Target},
};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use cairn_plugin_api::Manifest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const KIND: &str = "plugin-routes";

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Route {
    pub id: String,
    pub connection_id: String,
    pub resource_id: String,
    pub name: String,
    pub enabled: bool,
    #[serde(rename = "match")]
    pub selectors: BTreeMap<String, String>,
    pub agent_id: String,
    pub project_id: Option<String>,
    pub granted_access: Value,
    pub owner_access_id: String,
    pub access_mode: AccessMode,
    pub user_ids: Vec<String>,
    pub people: Vec<Label>,
    /// Generations only for currently listed people, bounded by the allowlist.
    /// People from the original route grant use its native revision.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    person_revisions: BTreeMap<String, String>,
    pub max_messages_per_hour: u32,
    pub max_per_person_per_hour: u32,
    pub revision: String,
    pub digest: String,
    pub updated_at: i64,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Binding {
    pub id: String,
    pub revision: String,
    pub resource: String,
    pub resources: Vec<(String, String)>,
    pub digest: String,
    pub context: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) person: Option<PersonGrant>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PersonGrant {
    id: String,
    revision: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    name: String,
    enabled: bool,
    #[serde(rename = "match", default)]
    selectors: BTreeMap<String, String>,
    agent_id: String,
    project_id: Option<String>,
    #[serde(default)]
    access_mode: AccessMode,
    #[serde(default)]
    user_ids: Vec<String>,
    #[serde(default)]
    people: Vec<Label>,
    #[serde(default = "hourly")]
    max_messages_per_hour: u32,
    #[serde(default = "personal_hourly")]
    max_per_person_per_hour: u32,
    revision: Option<String>,
}

impl Route {
    /// Management concurrency covers every field, including participants. The
    /// persisted revision remains the authority of already admitted work: removing
    /// Bob must not invalidate an unrelated request from Alice.
    fn edit_revision(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(self)?)))
    }

    fn owner_value(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        value.as_object_mut().unwrap().remove("personRevisions");
        value["revision"] = json!(self.edit_revision()?);
        Ok(value)
    }

    fn require_revision(&self, revision: Option<&str>) -> Result<()> {
        if revision != Some(self.edit_revision()?.as_str()) {
            return Err(Error::conflict("The route changed. Reload before editing."));
        }
        Ok(())
    }

    fn person_revision(&self, person: &str) -> &str {
        self.person_revisions
            .get(person)
            .map_or(self.revision.as_str(), String::as_str)
    }
}

/// Only the host stamps admitted work with a person's current grant. A fresh
/// approval never restores work carrying their retired grant generation.
pub(crate) fn admit_person(
    db: &Db<'_>,
    connection: &Connection,
    person: &str,
) -> Result<Option<Binding>> {
    let connection = connections::authorize(db, connection)?;
    let Some(mut binding) = connection.routing.clone() else {
        return Ok(None);
    };
    let route = route_in(db, &connection.id, &binding.resource, &binding.id)?;
    if route.revision != binding.revision || !connection.allows(person) {
        return Err(Error::forbidden("This person is no longer allowed."));
    }
    binding.person = Some(PersonGrant {
        id: person.to_owned(),
        revision: route.person_revision(person).to_owned(),
    });
    Ok(Some(binding))
}

/// Unbound transport/owner notifications cannot act for a person. Old draft
/// admissions without a generation fail closed instead of acquiring a new grant.
pub(crate) fn require_person(connection: &Connection, person: &str) -> Result<()> {
    if connection.routing.as_ref().is_some_and(|binding| {
        binding
            .person
            .as_ref()
            .is_none_or(|grant| grant.id != person)
    }) {
        return Err(Error::forbidden(
            "This person's admitted authority is unavailable.",
        ));
    }
    Ok(())
}

fn current_resource(db: &Db<'_>, target: &Target, resource: &Resource) -> Result<()> {
    target.current(db)?;
    if !target.get(db, &resource.id)?.is_some_and(|value| {
        value["connectionId"] == resource.connection_id
            && value["collection"] == resource.collection
            && value["revision"] == resource.revision
    }) {
        return Err(Error::conflict(
            "The resource changed. Reload its configuration.",
        ));
    }
    Ok(())
}

pub async fn eligibility(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    account: &str,
    query: &std::collections::HashMap<String, String>,
) -> Result<Value> {
    let target = Target::active(s, plugin, instance).await?;
    declared(target.manifest(), collection)?;
    let resource = resources::get_for(s, target.clone(), collection, resource).await?;
    let project = query.get("projectId").cloned();
    if let Some(project) = &project {
        uuid(project)?;
    }
    let mode: AccessMode = serde_json::from_value(json!(
        query.get("accessMode").map_or("allowlist", String::as_str)
    ))
    .map_err(|_| Error::bad("Choose a valid access mode."))?;
    super::operations::Owner::for_account(s, account).await?;
    s.store
        .transaction(move |db| {
            current_resource(db, &target, &resource)?;
            connections::delegation_eligibility(db, project.as_deref(), mode)
        })
        .await
}

fn hourly() -> u32 {
    20
}

fn personal_hourly() -> u32 {
    5
}

fn declared(manifest: &Manifest, collection: &str) -> Result<()> {
    if !manifest.permits_capability("conversations.submit")
        || !manifest
            .resources
            .iter()
            .any(|c| c.name == collection && c.delegation)
    {
        return Err(Error::bad(
            "This resource collection does not delegate conversations.",
        ));
    }
    Ok(())
}

fn matches(route: &Route, context: &Value) -> bool {
    route
        .selectors
        .iter()
        .all(|(key, value)| context[key].as_str() == Some(value))
}

fn selected(
    db: &Db<'_>,
    connection: &str,
    resource: &str,
    context: &Value,
) -> Result<Option<Route>> {
    let mut matching = db
        .list(KIND)?
        .into_iter()
        .filter(|r| r["connectionId"] == connection && r["resourceId"] == resource)
        .map(serde_json::from_value::<Route>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    matching.retain(|r| matches(r, context));
    matching.sort_by_key(|r| std::cmp::Reverse(r.selectors.len()));
    if matching.len() > 1 && matching[0].selectors.len() == matching[1].selectors.len() {
        return Err(Error::conflict(
            "Several routes match this event. Review their conditions.",
        ));
    }
    Ok(matching.into_iter().next())
}

#[allow(clippy::too_many_arguments)]
pub async fn save(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    account: &str,
    value: &Value,
    existing: Option<&str>,
) -> Result<Value> {
    let target = Target::active(s, plugin, instance).await?;
    declared(target.manifest(), collection)?;
    let resource = resources::get_for(s, target.clone(), collection, resource).await?;
    let owner = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::forbidden("The installation has no verified owner."))?
        .owner;
    if owner.account_id != account {
        return Err(Error::forbidden(
            "Only the current owner can configure a resource route.",
        ));
    }
    let input: Input =
        serde_json::from_value(value.clone()).map_err(|_| Error::bad("Invalid route fields."))?;
    uuid(&input.agent_id)?;
    if let Some(project) = &input.project_id {
        uuid(project)?;
    }
    let name = input.name.trim();
    if name.is_empty()
        || name.chars().count() > 100
        || name.chars().any(char::is_control)
        || input.selectors.len() > 8
        || !input.selectors.iter().all(|(key, value)| {
            !key.is_empty()
                && key.len() <= 40
                && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                && !value.is_empty()
                && value.len() <= 200
                && !value.chars().any(char::is_control)
        })
        || input.user_ids.len() > 100
        || input.user_ids.iter().any(|v| !connections::external_id(v))
        || (input.access_mode == AccessMode::Allowlist && input.user_ids.is_empty())
        || !(1..=100).contains(&input.max_messages_per_hour)
        || !(1..=50).contains(&input.max_per_person_per_hour)
    {
        return Err(Error::bad(
            "Choose a route name, source conditions, people and valid hourly limits.",
        ));
    }
    let route_id = existing.map_or_else(id, str::to_owned);
    uuid(&route_id)?;
    let people = input
        .user_ids
        .iter()
        .map(|user| Label {
            id: user.clone(),
            name: input
                .people
                .iter()
                .find(|p| &p.id == user)
                .map_or_else(String::new, |p| connections::label(&p.name)),
        })
        .collect();
    let mut route = Route {
        id: route_id,
        connection_id: instance.into(),
        resource_id: resource.id.clone(),
        name: name.into(),
        enabled: input.enabled,
        selectors: input.selectors,
        agent_id: input.agent_id,
        project_id: input.project_id,
        granted_access: Value::Null,
        owner_access_id: owner.access_id,
        access_mode: input.access_mode,
        user_ids: input.user_ids,
        people,
        person_revisions: BTreeMap::new(),
        max_messages_per_hour: input.max_messages_per_hour,
        max_per_person_per_hour: input.max_per_person_per_hour,
        revision: id(),
        digest: target.package().digest.clone(),
        updated_at: now(),
    };
    let previous_revision = input.revision;
    let updating = existing.is_some();
    target.recheck_package(s).await?;
    s.store
        .transaction(move |db| {
            current_resource(db, &target, &resource)?;

            if updating {
                route_in(db, &route.connection_id, &route.resource_id, &route.id)?
                    .require_revision(previous_revision.as_deref())?;
            } else {
                let count = db
                    .list(KIND)?
                    .iter()
                    .filter(|value| value["connectionId"] == route.connection_id)
                    .count();
                if count >= 256 {
                    return Err(Error::too_large("An instance can have at most 256 routes."));
                }
            }

            route.granted_access = connections::delegation_access(
                db,
                &route.agent_id,
                route.project_id.as_deref(),
                route.access_mode,
            )?;
            let value = serde_json::to_value(&route)?;
            db.put(KIND, &value)?;
            db.audit(
                "plugin.route.saved",
                &json!({
                    "id": route.id,
                    "connectionId": route.connection_id,
                    "resourceId": route.resource_id,
                }),
            )?;
            route.owner_value()
        })
        .await
}

pub async fn list(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    cursor: Option<&str>,
) -> Result<Value> {
    let target = Target::active(s, plugin, instance).await?;
    declared(target.manifest(), collection)?;
    let resource = resources::get_for(s, target.clone(), collection, resource).await?;
    let instance = instance.to_owned();
    let cursor = cursor.map(str::to_owned);
    s.store
        .transaction(move |db| {
            current_resource(db, &target, &resource)?;
            let hour = now() / 3_600_000;
            let mut pending = BTreeMap::<String, usize>::new();
            for request in db.list("plugin-requests")? {
                if request["connectionId"] == instance
                    && request["status"] == "pending"
                    && let Some(route) = request["routeId"].as_str()
                {
                    *pending.entry(route.to_owned()).or_default() += 1;
                }
            }
            let items = db
                .list(KIND)?
                .into_iter()
                .filter(|value| {
                    value["connectionId"] == instance && value["resourceId"] == resource.id
                })
                .map(|value| {
                    let route: Route = serde_json::from_value(value)?;
                    // Volatile owner information does not participate in edit
                    // concurrency or become authority for admitted work.
                    let mut value = route.owner_value()?;
                    let used = db
                        .kv(&format!("plugin-rate:{}:{hour}", route.id))?
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    value["summary"] = json!({
                        "hourStartedAt": hour * 3_600_000,
                        "messagesThisHour": used,
                        "pendingRequests": pending.get(&route.id).copied().unwrap_or(0),
                    });
                    Ok(value)
                })
                .collect::<Result<Vec<_>>>()?;
            super::paging::page(items, cursor.as_deref())
        })
        .await
}

#[allow(clippy::too_many_arguments)]
pub async fn remove(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    route: &str,
    account: &str,
    input: &Value,
) -> Result<Value> {
    let target = Target::active(s, plugin, instance).await?;
    let resource = resources::get_for(s, target.clone(), collection, resource).await?;
    let owner = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::forbidden("The installation has no verified owner."))?
        .owner;
    if owner.account_id != account {
        return Err(Error::forbidden(
            "Only the current owner can remove a route.",
        ));
    }
    uuid(route)?;
    let (instance, route, revision) = (
        instance.to_owned(),
        route.to_owned(),
        input["revision"].as_str().map(str::to_owned),
    );
    target.recheck_package(s).await?;
    s.store
        .transaction(move |db| {
            current_resource(db, &target, &resource)?;
            route_in(db, &instance, &resource.id, &route)?.require_revision(revision.as_deref())?;
            remove_in(db, &route)?;
            Ok(json!({ "deleted": true }))
        })
        .await
}

fn remove_in(db: &Db<'_>, id: &str) -> Result<()> {
    db.remove(KIND, id)?;
    for request in db.list("plugin-requests")? {
        if request["routeId"] == id {
            db.remove("plugin-requests", text(&request, "id"))?;
        }
    }
    db.audit("plugin.route.deleted", &json!({ "id": id }))
}

pub(crate) fn remove_resource(db: &Db<'_>, resource: &str) -> Result<()> {
    for r in db.list(KIND)? {
        if r["resourceId"] == resource {
            remove_in(db, text(&r, "id"))?;
        }
    }
    Ok(())
}

/// Select the most specific matching route, including disabled overrides. A
/// disabled or ambiguous override must never fall back to a broader delegation.
pub(crate) async fn select(
    s: &Service,
    root: &Connection,
    resource: &str,
    context: &Value,
) -> Result<Option<Connection>> {
    uuid(resource)?;
    let package = super::registry::active(s, &root.plugin_id).await?;
    let (root, resource, context) = (root.clone(), resource.to_owned(), context.clone());
    let routed = s
        .store
        .read(move |db| {
            let root = connections::authorize(db, &root)?;
            let reference = resources::reference_in(db, &root, &package.manifest, &resource)?;
            declared(&package.manifest, &reference.resource.collection)?;
            let Some(route) = selected(db, &root.id, &resource, &context)? else {
                return Ok(None);
            };
            if !route.enabled {
                return Ok(None);
            }
            if route.digest != package.digest {
                return Err(Error::forbidden(
                    "Review the route after the plugin version changed.",
                ));
            }
            let binding = Binding {
                id: route.id.clone(),
                revision: route.revision.clone(),
                resource,
                resources: reference.revisions,
                digest: route.digest.clone(),
                context,
                person: None,
            };
            apply(db, root, route, binding).map(Some)
        })
        .await?;
    if let Some(connection) = &routed {
        connections::ensure_owner(s, connection).await?;
    }
    Ok(routed)
}

fn apply(db: &Db<'_>, mut root: Connection, route: Route, binding: Binding) -> Result<Connection> {
    let access = connections::delegation_access(
        db,
        &route.agent_id,
        route.project_id.as_deref(),
        route.access_mode,
    )?;
    if access != route.granted_access {
        return Err(Error::forbidden(
            "Review the route after the agent's access changed.",
        ));
    }
    root.agent_id = route.agent_id;
    root.project_id = route.project_id;
    root.granted_access = route.granted_access;
    root.owner_access_id = Some(route.owner_access_id);
    root.access_mode = route.access_mode;
    root.user_ids = route.user_ids;
    root.people = route.people;
    root.max_messages_per_hour = route.max_messages_per_hour;
    root.max_per_person_per_hour = route.max_per_person_per_hour;
    root.routing = Some(binding);
    Ok(root)
}

pub(crate) fn authorize(
    db: &Db<'_>,
    mut root: Connection,
    binding: &Binding,
) -> Result<Connection> {
    let denied = || Error::forbidden("The resource route or its authority changed.");
    if binding.resources.is_empty()
        || binding.resources.len() > 17
        || !binding
            .resources
            .iter()
            .any(|(id, _)| id == &binding.resource)
    {
        return Err(denied());
    }
    root.routing = None;
    for (id, revision) in &binding.resources {
        if !db.get(resources::KIND, id)?.is_some_and(|r| {
            r["connectionId"] == root.id && r["revision"] == *revision && r["enabled"] == true
        }) {
            return Err(denied());
        }
    }
    // New admission reports ambiguity as a conflict. An admitted binding loses
    // authority instead, so its pending delivery cannot stall unrelated routes.
    let route = selected(db, &root.id, &binding.resource, &binding.context)
        .map_err(|error| if error.status == 409 { denied() } else { error })?
        .ok_or_else(denied)?;
    if !route.enabled
        || route.id != binding.id
        || route.revision != binding.revision
        || route.digest != binding.digest
    {
        return Err(denied());
    }
    if let Some(person) = &binding.person {
        let allowed =
            route.access_mode == AccessMode::Everyone || route.user_ids.contains(&person.id);
        if !allowed || route.person_revision(&person.id) != person.revision {
            return Err(denied());
        }
    }
    apply(db, root, route, binding.clone())
}

pub(crate) async fn bound(s: &Service, root: &Connection, value: &Value) -> Result<Connection> {
    if value.is_null() {
        return Ok(root.clone());
    }
    let binding: Binding = serde_json::from_value(value.clone())
        .map_err(|_| Error::forbidden("Invalid admitted route authority."))?;
    let mut root = root.clone();
    root.routing = Some(binding);
    let routed = s
        .store
        .read(move |db| connections::authorize(db, &root))
        .await?;
    connections::ensure_owner(s, &routed).await?;
    Ok(routed)
}

#[allow(clippy::too_many_arguments)]
pub async fn requests(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    route: &str,
) -> Result<Value> {
    resources::get(s, plugin, instance, collection, resource).await?;
    let (instance, resource, route) = (instance.to_owned(), resource.to_owned(), route.to_owned());
    s.store
        .read(move |db| {
            route_in(db, &instance, &resource, &route)?;
            let items = db
                .list("plugin-requests")?
                .into_iter()
                .filter(|value| value["routeId"] == route)
                .collect::<Vec<_>>();
            Ok(json!({"items": items}))
        })
        .await
}

fn route_in(db: &Db<'_>, instance: &str, resource: &str, route: &str) -> Result<Route> {
    let value = db
        .get(KIND, route)?
        .ok_or_else(|| Error::not_found("Route not found."))?;
    if value["connectionId"] != instance || value["resourceId"] != resource {
        return Err(Error::not_found("Route not found."));
    }
    serde_json::from_value(value).map_err(Error::from)
}

#[allow(clippy::too_many_arguments)]
pub async fn people(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
    route: &str,
    account: &str,
    user: &str,
    input: &Value,
) -> Result<Value> {
    let target = Target::active(s, plugin, instance).await?;
    let resource = resources::get_for(s, target.clone(), collection, resource).await?;
    let owner = crate::relay::task_author_policy(s)
        .await?
        .ok_or_else(|| Error::forbidden("The installation has no verified owner."))?
        .owner;
    if owner.account_id != account {
        return Err(Error::forbidden(
            "Only the current owner can manage this route.",
        ));
    }
    let action = text(input, "action");
    if !connections::external_id(user) || !matches!(action, "approve" | "deny" | "remove") {
        return Err(Error::bad("Choose a person and an access action."));
    }
    let revision = input["revision"].as_str().map(str::to_owned);
    let (instance, route, user, action) = (
        instance.to_owned(),
        route.to_owned(),
        user.to_owned(),
        action.to_owned(),
    );
    target.recheck_package(s).await?;
    s.store
        .transaction(move |db| {
            current_resource(db, &target, &resource)?;
            let mut route = route_in(db, &instance, &resource.id, &route)?;
            route.require_revision(revision.as_deref())?;
            if route.owner_access_id != owner.access_id {
                return Err(Error::forbidden("Review this route for the current owner."));
            }
            if route.access_mode == AccessMode::Everyone {
                return Err(Error::bad("Anyone access does not use a people list. Choose another access mode to restrict participation."));
            }
            let request = db
                .list("plugin-requests")?
                .into_iter()
                .find(|r| r["routeId"] == route.id && r["userId"] == user);
            match action.as_str() {
                "approve" => {
                    let request =
                        request.ok_or_else(|| Error::not_found("Access request not found."))?;
                    if !route.user_ids.contains(&user) {
                        if route.user_ids.len() >= 100 {
                            return Err(Error::bad("At most 100 people can use one route."));
                        }
                        route.user_ids.push(user.clone());
                        route.person_revisions.insert(user.clone(), id());
                        route.people.push(Label {
                            id: user.clone(),
                            name: connections::label(text(&request, "name")),
                        });
                    }
                    db.remove("plugin-requests", text(&request, "id"))?;
                }
                "deny" => {
                    let mut request =
                        request.ok_or_else(|| Error::not_found("Access request not found."))?;
                    request["status"] = json!("denied");
                    request["updatedAt"] = json!(now());
                    db.put("plugin-requests", &request)?;
                }
                _ => {
                    route.user_ids.retain(|id| id != &user);
                    route.people.retain(|p| p.id != user);
                    route.person_revisions.remove(&user);
                }
            }
            // A denied request changes outside the route itself. Advance its edit
            // token even when two management actions share one clock tick.
            route.updated_at = now().max(route.updated_at.saturating_add(1));
            let value = serde_json::to_value(&route)?;
            db.put(KIND, &value)?;
            db.audit(
                "plugin.route.people",
                &json!({ "id": route.id, "action": action }),
            )?;
            route.owner_value()
        })
        .await
}
