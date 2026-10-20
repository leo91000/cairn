//! Owner configuration is a separate authority from enabled operational resources.
//! A form retains one exact root, saved resource and selected parent/ancestor chain.
use super::{Resource, Target, declared};
use crate::{
    error::{Error, Result},
    plugins::{connections, operations::Owner},
    service::Service,
    store::Db,
    validation::uuid,
};
use cairn_plugin_api::ResourceCollection;
use serde_json::{Map, Value, json};
use std::sync::Arc;

pub(crate) struct Form {
    pub(crate) target: Arc<Target>,
    owner: Arc<Owner>,
    pub(crate) collection: ResourceCollection,
    pub(crate) existing: Option<Resource>,
    parent: Option<Resource>,
    revisions: Vec<(String, String)>,
    secrets: Map<String, Value>,
}

fn chain(db: &Db<'_>, target: &Target, start: &str) -> Result<Vec<Resource>> {
    let mut items: Vec<Resource> = Vec::new();
    let mut next = Some(start.to_owned());
    let mut expected = None;
    while let Some(id) = next {
        uuid(&id)?;
        if items.len() >= 16 || items.iter().any(|item| item.id == id) {
            return Err(Error::bad("Invalid resource parent chain."));
        }
        let resource: Resource = serde_json::from_value(
            target
                .get(db, &id)?
                .ok_or_else(|| Error::not_found("Configuration resource not found."))?,
        )?;
        if resource.connection_id != target.instance()
            || expected
                .as_ref()
                .is_some_and(|name| name != &resource.collection)
        {
            return Err(Error::forbidden(
                "The resource belongs to another configuration.",
            ));
        }
        let declaration = declared(target.manifest(), &resource.collection)?;
        if declaration.parent.is_some() != resource.parent_id.is_some() {
            return Err(Error::bad("Invalid resource parent chain."));
        }
        expected = declaration.parent.clone();
        next = resource.parent_id.clone();
        items.push(resource);
    }
    Ok(items)
}

impl Form {
    pub(crate) async fn load(
        s: &Service,
        target: Arc<Target>,
        collection: &str,
        existing: Option<&str>,
        input: &Value,
        submit: bool,
        owner: Arc<Owner>,
    ) -> Result<Arc<Self>> {
        target.require_revision(input)?;
        let declaration = declared(target.manifest(), collection)?.clone();
        if !declaration.form {
            return Err(Error::not_found("This collection has no package form."));
        }
        let id = existing.map(str::to_owned);
        let snapshot = input.clone();
        let bound = target.clone();
        let schema = declaration.clone();
        let (previous, parent, revisions) = s
            .store
            .read(move |db| {
                bound.current(db)?;
                let old = match &id {
                    Some(id) => chain(db, &bound, id)?,
                    None => Vec::new(),
                };
                let previous = old.first().cloned();
                if previous.as_ref().is_some_and(|item| {
                    item.collection != schema.name || snapshot["revision"] != item.revision
                }) {
                    return Err(Error::conflict("The resource changed. Reload its form."));
                }
                let selected = match (
                    &schema.parent,
                    snapshot.get("parentId").filter(|v| !v.is_null()),
                ) {
                    (Some(expected), Some(Value::String(id))) => {
                        let parents = chain(db, &bound, id)?;
                        if parents[0].collection != *expected {
                            return Err(Error::bad("Select the declared parent resource."));
                        }
                        parents
                    }
                    (None, None) => Vec::new(),
                    (Some(_), None) if !submit => Vec::new(),
                    _ => return Err(Error::bad("Select the declared parent resource.")),
                };
                let parent = selected.first().cloned();
                let revisions = old
                    .into_iter()
                    .chain(selected)
                    .map(|r| (r.id, r.revision))
                    .collect();
                Ok((previous, parent, revisions))
            })
            .await?;
        let saved = match &previous {
            Some(item) => s
                .vault
                .get(&super::secrets_key(&item.id))
                .await?
                .and_then(|v| v["fields"].as_object().cloned())
                .unwrap_or_default(),
            None => Map::new(),
        };
        let mut fields = declaration.settings.clone();
        if !submit || target.is_setup() {
            for field in &mut fields {
                field["required"] = json!(false);
            }
        }
        let secrets = connections::secret_fields(&fields, &input["secrets"], &saved)?;
        let form = Arc::new(Self {
            target,
            owner,
            collection: declaration,
            existing: previous,
            parent,
            revisions,
            secrets,
        });
        form.recheck(s).await?;
        Ok(form)
    }

    pub(crate) fn current(&self, db: &Db<'_>) -> Result<()> {
        self.target.current(db)?;
        for (id, revision) in &self.revisions {
            if !self
                .target
                .get(db, id)?
                .is_some_and(|value| value["revision"] == *revision)
            {
                return Err(Error::conflict(
                    "The resource or its parent changed. Reload its form.",
                ));
            }
        }
        Ok(())
    }

    pub(crate) async fn recheck(self: &Arc<Self>, s: &Service) -> Result<()> {
        self.owner.recheck(s).await?;
        self.target.recheck_package(s).await?;
        let form = self.clone();
        s.store.read(move |db| form.current(db)).await
    }

    pub(crate) fn selected(&self, selector: &str) -> Result<Option<&Resource>> {
        match selector {
            "self" => Ok(self.existing.as_ref()),
            "parent" => self
                .parent
                .as_ref()
                .map(Some)
                .ok_or_else(|| Error::bad("Select a parent resource first.")),
            _ => Err(Error::bad(
                "Configuration credentials require self or parent.",
            )),
        }
    }

    pub(crate) async fn secret(
        self: &Arc<Self>,
        s: &Service,
        selector: &str,
        key: &str,
    ) -> Result<String> {
        let (collection, values) = if selector == "self" {
            (&self.collection, self.secrets.clone())
        } else {
            let resource = self
                .selected(selector)?
                .ok_or_else(|| Error::bad("Save this resource first."))?;
            let collection = declared(self.target.manifest(), &resource.collection)?;
            let values = s
                .vault
                .get(&super::secrets_key(&resource.id))
                .await?
                .and_then(|v| v["fields"].as_object().cloned())
                .unwrap_or_default();
            (collection, values)
        };
        if !collection
            .settings
            .iter()
            .any(|field| field["key"] == key && field["type"] == "secret")
        {
            return Err(Error::forbidden(
                "The configuration has no such declared secret.",
            ));
        }
        self.recheck(s).await?;
        values
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::forbidden("The configuration has no such secret."))
    }

    pub(crate) fn metadata(&self) -> Value {
        json!({
            "collection": self.collection.name,
            "id": self.existing.as_ref().map(|r| &r.id),
            "parent": self.parent,
            "secrets": self.secrets.keys().collect::<Vec<_>>(),
        })
    }

    pub(in crate::plugins) async fn run(
        self: &Arc<Self>,
        s: &Service,
        values: Value,
        evaluation: &super::super::forms::Evaluation,
    ) -> Result<Value> {
        let values = super::super::forms::partial_values(&self.collection.settings, &values)?;
        let connection = self.target.connection();
        let app = super::super::scope::summary(&connection);
        let draft = super::super::forms::Draft {
            credential: s
                .vault
                .get(&connections::credential_key(&connection.id))
                .await?
                .and_then(|v| v["token"].as_str().map(str::to_owned)),
            secrets: connections::secret_values(s, &connection.id).await?,
            setup: self.target.setup_snapshot().cloned().map(Box::new),
            urls: None,
            owner: None,
            resource: Some(self.clone()),
            root: None,
        };
        let scope = super::super::scope::Scope::Draft(Box::new(connection), Arc::new(draft));
        let mut payload = json!({
            "instance": self.target.instance(), "app": app, "resource": self.metadata(),
            "values": values,
        });
        payload
            .as_object_mut()
            .unwrap()
            .extend(evaluation.fields().as_object().unwrap().clone());
        let result =
            super::super::scope::invoke(s, self.target.package(), "form", payload, &scope).await?;
        let result = evaluation.finish(&self.collection.settings, &["form", "parentId"], result)?;
        self.recheck(s).await?;
        Ok(result)
    }
}

pub(crate) async fn preview(
    s: &Service,
    target: Arc<Target>,
    collection: &str,
    input: &Value,
    owner: Arc<Owner>,
) -> Result<Value> {
    let resource = match input.get("resource").filter(|v| !v.is_null()) {
        Some(Value::String(id)) => Some(id.as_str()),
        Some(_) => return Err(Error::bad("Name a saved resource.")),
        None => None,
    };
    let form = Form::load(s, target, collection, resource, input, false, owner).await?;
    let evaluation =
        super::super::forms::Evaluation::preview(&form.collection.settings, &["parentId"], input)?;
    let result = form.run(s, input["settings"].clone(), &evaluation).await?;
    Ok(super::super::forms::response(&result))
}
