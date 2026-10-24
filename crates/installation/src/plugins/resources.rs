//! Instance-owned collections. Schema validation and encrypted fields are shared
//! with instance forms; neither management responses nor Luau can read secrets.
pub(crate) mod configuration;
pub(crate) mod forms;
pub(crate) mod target;

use super::connections::{self, Connection};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::{text, uuid},
};
use cairn_plugin_api::{Manifest, ResourceCollection};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use target::Target;

pub const KIND: &str = "plugin-resources";

#[derive(Clone)]
pub(crate) struct Reference {
    pub(crate) resource: Resource,
    pub(crate) revisions: Vec<(String, String)>,
}

/// Configuration references are constructed only from a host-bound owner form;
/// an operational caller cannot opt out of enabled-resource checks.
#[derive(Clone)]
pub(crate) enum Access {
    Operational(Reference),
    Configuration {
        form: Arc<forms::Form>,
        resource: Resource,
    },
}

impl Access {
    pub(crate) fn receipt_authority(&self) -> Value {
        match self {
            Self::Operational(reference) => json!(reference.revisions),
            Self::Configuration { resource, .. } => json!([[resource.id, resource.revision]]),
        }
    }

    pub(crate) fn resource(&self) -> &Resource {
        match self {
            Self::Operational(reference) => &reference.resource,
            Self::Configuration { resource, .. } => resource,
        }
    }

    pub(crate) fn setup(&self) -> Option<&super::setup::SetupDraft> {
        match self {
            Self::Operational(_) => None,
            Self::Configuration { form, .. } => form.target.setup_snapshot(),
        }
    }

    pub(crate) fn current(&self, db: &Db<'_>, connection: &Connection) -> Result<()> {
        match self {
            Self::Operational(reference) => recheck_in(db, connection, reference),
            Self::Configuration { form, .. } => form.current(db),
        }
    }

    pub(crate) async fn recheck(
        &self,
        s: &Service,
        context: &super::capabilities::Context,
    ) -> Result<()> {
        match self {
            Self::Operational(reference) => recheck(s, context, reference).await,
            Self::Configuration { form, .. } => form.recheck(s).await,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub id: String,
    pub connection_id: String,
    pub collection: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub enabled: bool,
    pub revision: String,
    pub settings: Value,
    pub secret_keys: Vec<String>,
    pub updated_at: i64,
}

pub fn secrets_key(id: &str) -> String {
    format!("plugin-resource-secrets:{id}")
}

pub(super) fn declared<'a>(manifest: &'a Manifest, name: &str) -> Result<&'a ResourceCollection> {
    manifest
        .resources
        .iter()
        .find(|collection| collection.name == name)
        .ok_or_else(|| Error::not_found("Resource collection not found."))
}

/// Configuration may be edited while the instance is paused; stale saves cannot
/// resurrect resources after the instance was removed or replaced.
fn current(db: &Db<'_>, expected: &Connection) -> Result<()> {
    let value = db.get(connections::KIND, &expected.id)?;
    if !value.is_some_and(|value| value["revision"] == expected.revision) {
        return Err(Error::conflict(
            "The plugin instance changed. Reload its configuration.",
        ));
    }
    Ok(())
}

fn parent(
    db: &Db<'_>,
    target: &Target,
    collection: &ResourceCollection,
    input: &Value,
) -> Result<Option<String>> {
    let parent = input["parentId"].as_str();
    match (&collection.parent, parent) {
        (None, None) => Ok(None),
        (Some(expected), Some(id)) => {
            uuid(id)?;
            let value = target
                .get(db, id)?
                .ok_or_else(|| Error::bad("Select a parent resource."))?;
            if value["connectionId"] != target.instance() || value["collection"] != *expected {
                return Err(Error::bad(
                    "The parent belongs to another instance or collection.",
                ));
            }
            Ok(Some(id.to_owned()))
        }
        _ => Err(Error::bad("Select the declared parent resource.")),
    }
}

pub async fn save(
    s: &Service,
    plugin: &str,
    instance: &str,
    name: &str,
    input: &Value,
    existing: Option<&str>,
) -> Result<Value> {
    let owner = Arc::new(super::operations::Owner::current(s).await?);
    save_owned(s, plugin, instance, name, input, existing, owner).await
}

pub(crate) async fn save_owned(
    s: &Service,
    plugin: &str,
    instance: &str,
    name: &str,
    input: &Value,
    existing: Option<&str>,
    owner: Arc<super::operations::Owner>,
) -> Result<Value> {
    let target = Target::active(s, plugin, instance).await?;
    save_for(s, target, name, input, existing, owner).await
}

pub(super) async fn save_for(
    s: &Service,
    target: Arc<Target>,
    name: &str,
    input: &Value,
    existing: Option<&str>,
    owner: Arc<super::operations::Owner>,
) -> Result<Value> {
    save_configured(
        s,
        target,
        name,
        input,
        existing,
        configuration::Configuration::owner(owner),
    )
    .await
}

pub(super) async fn save_configured(
    s: &Service,
    target: Arc<Target>,
    name: &str,
    input: &Value,
    existing: Option<&str>,
    mut configuration: configuration::Configuration,
) -> Result<Value> {
    target.require_revision(input)?;
    let manifest = target.manifest();
    let collection = declared(manifest, name)?.clone();
    let previous = match existing {
        Some(id) => Some(get_for(s, target.clone(), &collection.name, id).await?),
        None => None,
    };
    if let Some(previous) = &previous
        && input["revision"] != previous.revision
    {
        return Err(Error::conflict(
            "The resource changed. Reload before saving.",
        ));
    }
    let name = text(input, "name").trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(Error::bad(
            "Enter a resource name of at most 100 characters.",
        ));
    }
    if input.as_object().is_none_or(|object| {
        object.keys().any(|key| {
            ![
                "name", "settings", "secrets", "enabled", "parentId", "revision",
            ]
            .contains(&key.as_str())
                && !(target.is_setup() && key == "setupRevision")
        })
    }) {
        return Err(Error::bad("Invalid resource fields."));
    }
    let enabled = match input.get("enabled") {
        None => previous.as_ref().is_none_or(|resource| resource.enabled),
        Some(Value::Bool(enabled)) => *enabled,
        _ => return Err(Error::bad("Resource enabled must be a boolean.")),
    };
    let mut fields = collection.settings.clone();
    if target.is_setup() {
        for field in &mut fields {
            field["required"] = json!(false);
        }
    }
    configuration
        .credentials(s, &target, previous.as_slice(), false)
        .await?;
    let form = if collection.form {
        Some(
            forms::Form::load(
                s,
                target.clone(),
                &collection.name,
                existing,
                input,
                true,
                configuration.clone(),
            )
            .await?,
        )
    } else {
        None
    };
    let mut settings = input["settings"].clone();
    if let Some(form) = &form {
        let result = form
            .run(s, settings.clone(), &super::forms::Evaluation::submit())
            .await?;
        super::forms::require_valid(&collection.settings, &result)?;
        super::forms::merge_values(&mut settings, &result);
    }
    let settings = connections::settings_with(&fields, &settings)?;
    let resource_id = previous
        .as_ref()
        .map_or_else(id, |resource| resource.id.clone());
    let key = secrets_key(&resource_id);
    let saved = match &previous {
        Some(_) => s
            .vault
            .get(&key)
            .await?
            .and_then(|value| value["fields"].as_object().cloned())
            .unwrap_or_default(),
        None => Map::new(),
    };
    let secrets = connections::secret_fields(&fields, &input["secrets"], &saved)?;
    if serde_json::to_vec(&settings)?.len() > 65_536 || serde_json::to_vec(&secrets)?.len() > 65_536
    {
        return Err(Error::too_large(
            "Resource values and secrets are limited to 64 KiB each.",
        ));
    }
    let resource = Resource {
        id: resource_id,
        connection_id: target.instance().to_owned(),
        collection: collection.name.clone(),
        parent_id: None,
        name: name.to_owned(),
        enabled,
        revision: id(),
        settings,
        secret_keys: secrets.keys().cloned().collect(),
        updated_at: now(),
    };
    let encrypted = s.vault.encrypt(&key, &json!({ "fields": secrets }))?;
    configuration.recheck(s).await?;
    target.recheck_package(s).await?;
    let input = input.clone();
    s.store
        .transaction(move |db| {
            configuration.current(db)?;
            target.current(db)?;
            if let Some(form) = &form {
                form.current(db)?;
            }
            if let Some(previous) = &previous {
                let stored = target.get(db, &previous.id)?;
                if !stored.is_some_and(|stored| stored["revision"] == previous.revision) {
                    return Err(Error::conflict(
                        "The resource changed. Reload before saving.",
                    ));
                }
            }
            if previous.is_none() && target.list(db)?.len() >= 256 {
                return Err(Error::too_large(
                    "An instance can have at most 256 resources.",
                ));
            }
            let mut resource = resource;
            resource.parent_id = parent(db, &target, &collection, &input)?;
            db.set(
                &format!("mcp-secret:{key}"),
                &encrypted,
                target.expires_at(),
            )?;
            let value = serde_json::to_value(resource)?;
            target.put(db, &value)?;
            db.audit(
                "plugin.resource.saved",
                &json!({ "id": value["id"], "connectionId": target.instance() }),
            )?;
            Ok(value)
        })
        .await
}

pub async fn get(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
) -> Result<Resource> {
    let target = Target::active(s, plugin, instance).await?;
    get_for(s, target, collection, resource).await
}

pub(crate) async fn get_for(
    s: &Service,
    target: Arc<Target>,
    collection: &str,
    resource: &str,
) -> Result<Resource> {
    declared(target.manifest(), collection)?;
    uuid(resource)?;
    let (collection, resource) = (collection.to_owned(), resource.to_owned());
    s.store
        .read(move |db| {
            target.current(db)?;
            let value = target
                .get(db, &resource)?
                .ok_or_else(|| Error::not_found("Resource not found."))?;
            if value["connectionId"] != target.instance() || value["collection"] != collection {
                return Err(Error::not_found("Resource not found."));
            }
            Ok(serde_json::from_value(value)?)
        })
        .await
}

pub async fn remove(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    resource: &str,
) -> Result<Value> {
    let target = Target::active(s, plugin, instance).await?;
    remove_for(s, target, collection, resource, &Value::Null).await
}

async fn remove_for(
    s: &Service,
    target: Arc<Target>,
    collection: &str,
    resource: &str,
    input: &Value,
) -> Result<Value> {
    remove_configured(s, target, collection, resource, input, None).await
}

pub(super) async fn remove_configured(
    s: &Service,
    target: Arc<Target>,
    collection: &str,
    resource: &str,
    input: &Value,
    configuration: Option<configuration::Configuration>,
) -> Result<Value> {
    target.require_revision(input)?;
    let resource = get_for(s, target.clone(), collection, resource).await?;
    if target.is_setup() && input["revision"] != resource.revision {
        return Err(Error::conflict(
            "The resource changed. Reload before deleting.",
        ));
    }
    if let Some(configuration) = &configuration {
        configuration.recheck(s).await?;
    }
    s.store
        .transaction(move |db| {
            if let Some(configuration) = &configuration {
                configuration.current(db)?;
            }
            target.current(db)?;
            if target.is_setup()
                && target
                    .get(db, &resource.id)?
                    .is_none_or(|value| value["revision"] != resource.revision)
            {
                return Err(Error::conflict(
                    "The resource changed. Reload before deleting.",
                ));
            }
            let records = target.list(db)?;
            let mut ids = std::collections::HashSet::from([resource.id.clone()]);
            loop {
                let before = ids.len();
                for value in &records {
                    if value["connectionId"] == target.instance()
                        && value["parentId"]
                            .as_str()
                            .is_some_and(|parent| ids.contains(parent))
                    {
                        ids.insert(text(value, "id").to_owned());
                    }
                }
                if before == ids.len() {
                    break;
                }
            }
            for value in db.list(super::grants::KIND)? {
                if value["connectionId"] == target.instance()
                    && value["resourceId"]
                        .as_str()
                        .is_some_and(|id| ids.contains(id))
                {
                    db.remove(super::grants::KIND, text(&value, "id"))?;
                }
            }
            for id in ids {
                super::routes::remove_resource(db, &id)?;
                super::oauth::remove_resource(db, &id)?;
                db.delete(&format!("mcp-secret:{}", secrets_key(&id)))?;
                target.remove(db, &id)?;
            }
            db.audit(
                "plugin.resource.deleted",
                &json!({ "id": resource.id, "connectionId": target.instance() }),
            )?;
            Ok(json!({ "deleted": true }))
        })
        .await
}

pub async fn list(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    cursor: Option<&str>,
) -> Result<Value> {
    let target = Target::active(s, plugin, instance).await?;
    list_for(s, target, collection, cursor).await
}

/// A chosen collection's selector does not receive configuration or credential
/// field names. SQL applies the instance, collection and cursor before decoding.
pub async fn choices(
    s: &Service,
    plugin: &str,
    instance: &str,
    collection: &str,
    cursor: Option<&str>,
) -> Result<Value> {
    if let Some(cursor) = cursor {
        uuid(cursor)?;
    }
    let target = Target::active(s, plugin, instance).await?;
    declared(target.manifest(), collection)?;
    let collection = collection.to_owned();
    let cursor = cursor.unwrap_or("").to_owned();
    s.store.read(move |db| {
        let transaction = rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Deferred)?;
        let db = &Db(&transaction);
        target.current(db)?;
        let rows = db.json_rows(
            "SELECT json_object('id',id,'name',json_extract(data,'$.name'),
                'connectionId',json_extract(data,'$.connectionId'),
                'collection',json_extract(data,'$.collection'),'parentId',json_extract(data,'$.parentId'),
                'enabled',json(CASE WHEN json_extract(data,'$.enabled')=1 THEN 'true' ELSE 'false' END))
             FROM records WHERE kind='plugin-resources' AND json_extract(data,'$.connectionId')=?1
             AND json_extract(data,'$.collection')=?2 AND id>?3 ORDER BY id LIMIT ?4",
            rusqlite::params![target.instance(), collection, cursor, (super::paging::MAX_ITEMS + 1) as i64],
        )?;
        super::paging::ordered_page(rows, "id", super::paging::MAX_ITEMS, super::paging::MAX_BYTES)
    }).await
}

pub(super) async fn list_for(
    s: &Service,
    target: Arc<Target>,
    collection: &str,
    cursor: Option<&str>,
) -> Result<Value> {
    declared(target.manifest(), collection)?;
    let values = s
        .store
        .read(move |db| {
            target.current(db)?;
            target.list(db)
        })
        .await?
        .into_iter()
        .filter(|value| value["collection"] == collection)
        .collect::<Vec<_>>();
    // The same cursor page also crosses the worker's smaller capability frame.
    super::paging::page_with_bounds(values, cursor, 100, cairn_plugin_api::MAX_FRAME_BYTES / 2)
}

pub(crate) async fn call(
    s: &Service,
    context: &super::capabilities::Context,
    method: &str,
    args: &Value,
) -> Result<Value> {
    let connection = &context.connection;
    let collection = text(args, "collection");
    match method {
        "resources.get" => Ok(serde_json::to_value(
            get(
                s,
                &connection.plugin_id,
                &connection.id,
                collection,
                text(args, "resource"),
            )
            .await?,
        )?),
        "resources.list" => {
            list(
                s,
                &connection.plugin_id,
                &connection.id,
                collection,
                args["cursor"].as_str(),
            )
            .await
        }
        _ => Err(Error::forbidden("Unknown resource operation.")),
    }
}

pub(crate) async fn reference(
    s: &Service,
    context: &super::capabilities::Context,
    resource: &str,
) -> Result<Reference> {
    uuid(resource)?;
    let connection = context.connection.clone();
    let manifest = context.manifest.clone();
    let resource = resource.to_owned();
    s.store
        .read(move |db| reference_in(db, &connection, &manifest, &resource))
        .await
}

pub(crate) fn reference_in(
    db: &Db<'_>,
    connection: &Connection,
    manifest: &Manifest,
    resource: &str,
) -> Result<Reference> {
    current(db, connection)?;
    let denied = || Error::forbidden("The resource is unavailable or belongs to another instance.");
    let mut id = resource.to_owned();
    let mut revisions = Vec::new();
    let mut selected = None;
    let mut expected_collection = None;
    loop {
        if revisions.len() > 16 || revisions.iter().any(|(seen, _)| seen == &id) {
            return Err(denied());
        }
        let value = db.get(KIND, &id)?.ok_or_else(denied)?;
        let value: Resource = serde_json::from_value(value)?;
        if !value.enabled
            || value.connection_id != connection.id
            || expected_collection
                .as_ref()
                .is_some_and(|name| name != &value.collection)
        {
            return Err(denied());
        }
        let collection = declared(manifest, &value.collection)?;
        revisions.push((value.id.clone(), value.revision.clone()));
        if selected.is_none() {
            selected = Some(value.clone());
        }
        match (&collection.parent, &value.parent_id) {
            (None, None) => break,
            (Some(name), Some(parent)) => {
                expected_collection = Some(name.clone());
                id = parent.clone();
            }
            _ => return Err(denied()),
        }
    }
    Ok(Reference {
        resource: selected.unwrap(),
        revisions,
    })
}

pub(crate) async fn recheck(
    s: &Service,
    context: &super::capabilities::Context,
    reference: &Reference,
) -> Result<()> {
    let connection = context.connection.clone();
    let reference = reference.clone();
    s.store
        .read(move |db| recheck_in(db, &connection, &reference))
        .await
}

pub(crate) fn recheck_in(
    db: &Db<'_>,
    connection: &Connection,
    reference: &Reference,
) -> Result<()> {
    current(db, connection)?;
    for (id, revision) in &reference.revisions {
        if !db
            .get(KIND, id)?
            .is_some_and(|value| value["revision"] == *revision && value["enabled"] == true)
        {
            return Err(Error::forbidden("The resource changed during the request."));
        }
    }
    Ok(())
}

pub(crate) async fn credential(
    s: &Service,
    context: &super::capabilities::Context,
    resource: &str,
    key: &str,
) -> Result<String> {
    let reference = reference(s, context, resource).await?;
    let collection = declared(&context.manifest, &reference.resource.collection)?;
    if !collection
        .settings
        .iter()
        .any(|field| field["key"] == key && field["type"] == "secret")
    {
        return Err(Error::forbidden(
            "The resource has no such declared secret.",
        ));
    }
    let value = s
        .vault
        .get(&secrets_key(resource))
        .await?
        .and_then(|value| value["fields"][key].as_str().map(str::to_owned))
        .ok_or_else(|| Error::forbidden("The resource has no such secret."))?;
    recheck(s, context, &reference).await?;
    Ok(value)
}

/// Called inside the connection's deletion transaction, before its records vanish.
pub(crate) fn remove_owned(db: &Db<'_>, instance: &str) -> Result<()> {
    for value in db.list(KIND)? {
        if value["connectionId"] == instance {
            let id = text(&value, "id");
            super::routes::remove_resource(db, id)?;
            super::oauth::remove_resource(db, id)?;
            db.delete(&format!("mcp-secret:{}", secrets_key(id)))?;
            db.remove(KIND, id)?;
        }
    }
    Ok(())
}

/// Configuration adapter for the distinct owner-only setup API. Active resource
/// functions never resolve this target implicitly.
pub(crate) async fn setup_routes(
    s: &Service,
    input: &crate::http::Input,
    plugin: &str,
    draft: &str,
    collection: &str,
    resource: Option<&str>,
) -> Result<Value> {
    let account = input
        .identity
        .as_ref()
        .map_or("", |identity| identity.account_id.as_str());
    let target = Target::setup(s, plugin, draft, account).await?;
    match (input.method.as_str(), resource) {
        ("POST", Some("form")) => {
            forms::preview(
                s,
                target,
                collection,
                &input.body,
                Arc::new(super::operations::Owner::for_account(s, account).await?),
            )
            .await
        }
        ("GET", None) => {
            list_for(
                s,
                target,
                collection,
                input.query.get("cursor").map(String::as_str),
            )
            .await
        }
        ("POST", None) => {
            save_for(
                s,
                target,
                collection,
                &input.body,
                None,
                Arc::new(super::operations::Owner::for_account(s, account).await?),
            )
            .await
        }
        ("GET", Some(id)) => Ok(serde_json::to_value(
            get_for(s, target, collection, id).await?,
        )?),
        ("PUT", Some(id)) => {
            save_for(
                s,
                target,
                collection,
                &input.body,
                Some(id),
                Arc::new(super::operations::Owner::for_account(s, account).await?),
            )
            .await
        }
        ("DELETE", Some(id)) => remove_for(s, target, collection, id, &input.body).await,
        _ => Err(Error::method_not_allowed("Method not allowed.")),
    }
}

pub(crate) fn remove_setup(db: &Db<'_>, instance: &str) -> Result<()> {
    for (key, value) in db.keys(&target::setup_prefix(instance))? {
        let id = text(&value, "id");
        super::oauth::remove_resource(db, id)?;
        db.delete(&format!("mcp-secret:{}", secrets_key(id)))?;
        db.delete(&key)?;
    }
    Ok(())
}

/// Snapshot read for native OAuth callbacks; it supplies no operational authority.
pub(crate) fn setup_resource_in(
    db: &Db<'_>,
    draft: &super::setup::SetupDraft,
    resource: &str,
) -> Result<Value> {
    draft.recheck_in(db)?;
    let value = db
        .kv(&format!("{}{resource}", target::setup_prefix(&draft.id)))?
        .ok_or_else(|| Error::not_found("Setup resource not found."))?;
    if value["connectionId"] != draft.id {
        return Err(Error::forbidden(
            "The setup resource belongs to another draft.",
        ));
    }
    Ok(value)
}

/// Promote a complete, unchanged setup tree. All reads and writes belong to the
/// activation transaction, so incomplete fields cannot publish a partial app.
pub(crate) fn activate_setup_in(
    db: &Db<'_>,
    vault: &crate::vault::Vault,
    target: &Target,
    expected: &[Value],
    connection: &Connection,
) -> Result<()> {
    let draft = target
        .setup_snapshot()
        .ok_or_else(|| Error::forbidden("A setup target is required."))?;
    if target.list(db)? != expected {
        return Err(Error::conflict(
            "Setup resources changed. Reload before activating.",
        ));
    }
    let mut prepared = Vec::new();
    for value in expected {
        let mut resource: Resource = serde_json::from_value(value.clone())?;
        let collection = declared(target.manifest(), &resource.collection)?;
        if resource.connection_id != draft.id || db.get(KIND, &resource.id)?.is_some() {
            return Err(Error::conflict("Setup resource identity changed."));
        }
        let parent_id = parent(db, target, collection, value)?;
        if parent_id != resource.parent_id {
            return Err(Error::conflict("Setup resource parent changed."));
        }
        let mut schema = target.manifest().clone();
        schema.settings.clone_from(&collection.settings);
        resource.settings = connections::settings(&schema, &resource.settings)?;
        let key = secrets_key(&resource.id);
        let stored = format!("mcp-secret:{key}");
        let encrypted = db
            .kv(&stored)?
            .ok_or_else(|| Error::conflict("Setup resource secrets expired."))?;
        let saved = vault.decrypt(&key, &encrypted)?;
        let fields = saved["fields"]
            .as_object()
            .ok_or_else(|| Error::conflict("Setup resource secrets are unavailable."))?;
        let fields = connections::secrets(&schema, &Value::Null, fields)?;
        resource.secret_keys = fields.keys().cloned().collect();
        resource.revision = id();
        resource.updated_at = now();
        prepared.push((resource, stored, encrypted));
    }
    for (resource, stored, encrypted) in prepared {
        super::oauth::activate_setup_in(
            db,
            vault,
            target.manifest(),
            draft,
            connection,
            &resource,
        )?;
        db.set(&stored, &encrypted, None)?;
        db.put(KIND, &serde_json::to_value(&resource)?)?;
        target.remove(db, &resource.id)?;
    }
    Ok(())
}
