//! Fixture construction goes through the current mission commands, never records.
#![allow(dead_code)]

use cairn_installation::{
    auth::InstallationIdentity,
    error::{Error, Result},
    service::Service,
    store::Store,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub trait MissionFixture {
    fn task_fixture(
        &self,
        input: Value,
        existing: Option<&str>,
    ) -> impl std::future::Future<Output = Result<Value>>;

    fn task_as_fixture(
        &self,
        input: Value,
        existing: Option<&str>,
        identity: Option<&InstallationIdentity>,
    ) -> impl std::future::Future<Output = Result<Value>>;

    fn mission_snapshot_fixture(
        &self,
        id: &str,
    ) -> impl std::future::Future<Output = Result<Value>>;

    fn remove_mission_fixture(&self, id: &str) -> impl std::future::Future<Output = Result<()>>;
}

impl MissionFixture for Service {
    async fn task_fixture(&self, input: Value, existing: Option<&str>) -> Result<Value> {
        self.task_as_fixture(input, existing, None).await
    }

    async fn task_as_fixture(
        &self,
        mut input: Value,
        existing: Option<&str>,
        identity: Option<&InstallationIdentity>,
    ) -> Result<Value> {
        let current = if let Some(id) = existing {
            self.store.mission(id).await?
        } else {
            None
        };
        let fields = input
            .as_object_mut()
            .ok_or_else(|| Error::bad("Expected fixture object"))?;
        let creation = fields.remove("creationId");
        let revision = fields
            .remove("revision")
            .or_else(|| current.as_ref().map(|m| json!(m.revision)));
        for field in [
            "id",
            "authorId",
            "authorAccessId",
            "authorRemoved",
            "createdAt",
            "nextRun",
            "scheduleWaitReason",
            "scheduleFailure",
            "nativePrincipals",
            "nativeAuthorityNodes",
            "nativeParentRunId",
            "nativeParentAccess",
            "nativeProvenanceUnknown",
            "nativePluginGrants",
        ] {
            fields.remove(field);
        }
        let mut settings = current
            .map(|m| serde_json::to_value(m.settings))
            .transpose()?
            .unwrap_or_else(|| json!({}));
        cairn_installation::store::merge(&mut settings, &input);
        let settings: cairn_contracts::MissionSettings =
            serde_json::from_value(settings).map_err(|e| Error::bad(e.to_string()))?;
        let mut body = if let Some(id) = existing {
            json!({"taskId":id,"revision":revision.unwrap_or_else(||json!("")),"settings":settings})
        } else {
            json!({"settings":settings})
        };
        if existing.is_none()
            && let Some(creation) = creation
        {
            body["creationId"] = creation;
        }
        let identity = identity.cloned().unwrap_or_else(|| {
            let mut identity = InstallationIdentity::trusted(
                cairn_installation::auth::InstallationRole::Owner,
                "fixture-owner",
            );
            identity.mcp_scopes = Some(vec!["manage".into()]);
            identity
        });
        Box::pin(cairn_installation::api::dispatch(
            &Arc::new(self.clone()),
            &cairn_installation::http::Input {
                method: if existing.is_some() { "PUT" } else { "POST" }.into(),
                path: existing.map_or_else(|| "/api/tasks".into(), |id| format!("/api/tasks/{id}")),
                body,
                query: std::collections::HashMap::default(),
                headers: axum::http::HeaderMap::default(),
                identity: Some(identity),
            },
        ))
        .await
    }

    async fn mission_snapshot_fixture(&self, id: &str) -> Result<Value> {
        Ok(serde_json::to_value(cairn_installation::error::required(
            self.store.mission(id).await?,
            "Mission not found",
        )?)?)
    }

    async fn remove_mission_fixture(&self, id: &str) -> Result<()> {
        self.store
            .transaction({
                let id = id.to_owned();
                move |db| db.delete_mission(&id)
            })
            .await
    }
}

pub trait StoreMissionFixture {
    fn put_mission_fixture(&self, value: Value)
    -> impl std::future::Future<Output = Result<Value>>;

    fn mission_values_fixture(&self) -> impl std::future::Future<Output = Result<Vec<Value>>>;
}

impl StoreMissionFixture for Store {
    async fn put_mission_fixture(&self, value: Value) -> Result<Value> {
        let mission: cairn_contracts::Mission = serde_json::from_value(value)?;
        self.transaction(move |db| {
            db.save_mission(&mission)?;
            Ok(serde_json::to_value(mission)?)
        })
        .await
    }

    async fn mission_values_fixture(&self) -> Result<Vec<Value>> {
        Ok(self
            .missions()
            .await?
            .into_iter()
            .map(|m| serde_json::to_value(m).unwrap())
            .collect())
    }
}
