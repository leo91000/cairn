//! Concise setup for other-domain tests. Publication goes through the current HTTP
//! capability; only fixture construction fills complete replacement settings.
#![allow(dead_code)]

use cairn_installation::{
    error::{Error, Result},
    service::Service,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub trait AgentFixture {
    fn agent_preview_fixture(
        &self,
        patch: Value,
        id: &str,
    ) -> impl std::future::Future<Output = Result<Value>>;

    fn merge_agents_fixture(
        &self,
        input: Value,
    ) -> impl std::future::Future<Output = Result<Value>>;

    fn agent_fixture(
        &self,
        patch: Value,
        existing: Option<&str>,
    ) -> impl std::future::Future<Output = Result<Value>>;
}

impl AgentFixture for Arc<Service> {
    async fn agent_preview_fixture(&self, patch: Value, id: &str) -> Result<Value> {
        let body = agent_command(self, patch, Some(id)).await?;
        Box::pin(cairn_installation::api::dispatch(
            self,
            &cairn_installation::http::Input {
                method: "POST".into(),
                path: format!("/api/agents/{id}/preview"),
                query: std::collections::HashMap::default(),
                headers: axum::http::HeaderMap::default(),
                body,
                identity: Some(cairn_installation::auth::InstallationIdentity::trusted(
                    cairn_installation::auth::InstallationRole::Owner,
                    "fixture-owner",
                )),
            },
        ))
        .await
    }

    async fn merge_agents_fixture(&self, input: Value) -> Result<Value> {
        Box::pin(cairn_installation::api::dispatch(
            self,
            &cairn_installation::http::Input {
                method: "POST".into(),
                path: format!(
                    "/api/agents/{}/merge",
                    input["sourceId"].as_str().unwrap_or("")
                ),
                query: std::collections::HashMap::default(),
                headers: axum::http::HeaderMap::default(),
                body: input,
                identity: Some(cairn_installation::auth::InstallationIdentity::trusted(
                    cairn_installation::auth::InstallationRole::Owner,
                    "fixture-owner",
                )),
            },
        ))
        .await
    }

    async fn agent_fixture(&self, patch: Value, existing: Option<&str>) -> Result<Value> {
        let body = agent_command(self, patch, existing).await?;
        Box::pin(cairn_installation::api::dispatch(
            self,
            &cairn_installation::http::Input {
                method: if existing.is_some() { "PUT" } else { "POST" }.into(),
                path: existing
                    .map_or_else(|| "/api/agents".into(), |id| format!("/api/agents/{id}")),
                query: std::collections::HashMap::default(),
                headers: axum::http::HeaderMap::default(),
                body,
                identity: Some(cairn_installation::auth::InstallationIdentity::trusted(
                    cairn_installation::auth::InstallationRole::Owner,
                    "fixture-owner",
                )),
            },
        ))
        .await
    }
}

pub trait StoreAgentFixture {
    fn put_agent_fixture(&self, value: Value) -> impl std::future::Future<Output = Result<Value>>;
}

impl StoreAgentFixture for cairn_installation::store::Store {
    async fn put_agent_fixture(&self, value: Value) -> Result<Value> {
        let agent = profile(value)?;
        self.transaction(move |db| Ok(serde_json::to_value(db.save_agent(&agent)?)?))
            .await
    }
}

pub fn profile(mut value: Value) -> Result<cairn_contracts::Agent> {
    let mut defaults = json!({ "name": "Fixture", "exclusiveNodes": [], "plugins": [], "access": cairn_contracts::AccessPolicy::default(), "revision": cairn_installation::config::id(), "createdAt": 0 });
    if let Some(access) = value.get_mut("access") {
        let mut complete = defaults["access"].clone();
        cairn_installation::store::merge(&mut complete, access);
        *access = complete;
    }
    cairn_installation::store::merge(&mut defaults, &value);
    let value = defaults;
    let agent: cairn_contracts::Agent = serde_json::from_value(value)?;
    Ok(agent)
}

pub async fn agent_command(
    s: &Arc<Service>,
    mut patch: Value,
    existing: Option<&str>,
) -> Result<Value> {
    let current = match existing {
        Some(id) => s.store.agent(id).await?,
        None => None,
    };
    let mut settings = match &current {
        Some(agent) => serde_json::to_value(&agent.settings)?,
        None => {
            json!({ "exclusiveNodes": [], "plugins": [], "access": cairn_contracts::AccessPolicy::default() })
        }
    };
    let fields = patch
        .as_object_mut()
        .ok_or_else(|| Error::bad("Expected fixture object"))?;
    let creation = fields.remove("creationId");
    let revision = fields
        .remove("revision")
        .or_else(|| current.as_ref().map(|agent| json!(agent.revision)));
    let review = json!({
        "nativeNodeReviews": fields.remove("nativeNodeReviews").unwrap_or_else(|| json!([])),
        "reapproveDelegations": fields.remove("reapproveDelegations").unwrap_or_else(|| json!([])),
    });
    for key in ["id", "avatar", "createdAt"] {
        fields.remove(key);
    }
    if let Some(access) = patch.get_mut("access") {
        let mut complete = settings["access"].clone();
        cairn_installation::store::merge(&mut complete, access);
        *access = complete;
    }
    cairn_installation::store::merge(&mut settings, &patch);
    // Decode the fixture settings to materialize all server defaults.
    let settings: cairn_contracts::AgentSettings =
        serde_json::from_value(settings).map_err(|error| Error::bad(error.to_string()))?;
    let body = if let Some(id) = existing {
        json!({ "agentId": id, "revision": revision.unwrap_or_else(|| json!("")), "settings": settings, "review": review })
    } else {
        let mut value = json!({ "settings": settings, "review": review });
        if let Some(creation) = creation {
            value["creationId"] = creation;
        }
        value
    };
    Ok(body)
}
