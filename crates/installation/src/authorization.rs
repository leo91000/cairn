//! Authorization carried through asynchronous management effects. Legacy callers
//! retain their local publication callback; installation callers also retain the
//! execution's live semantic delegation proof.
use crate::{
    capabilities::plugin_packages::Authority,
    error::{Error, Result},
    service::Service,
    store::Db,
};
use std::sync::{Arc, Mutex};

type LocalGuard = dyn Fn(&Db<'_>) -> Result<()> + Send + Sync;

mod jobs;

pub(crate) use jobs::{Accepted, Jobs};

#[derive(Clone)]
pub(crate) struct Authorization {
    local: Arc<LocalGuard>,
    admission: Option<Arc<LocalGuard>>,
    publication: Option<Arc<LocalGuard>>,
    service: Option<Arc<Service>>,
    live: Option<Authority>,
    job: Option<Arc<jobs::Accepted>>,
    job_bearer: Option<String>,
}

impl Default for Authorization {
    fn default() -> Self {
        Self::local(|_| Ok(()))
    }
}

impl Authorization {
    pub(crate) fn local<F>(authorize: F) -> Self
    where
        F: Fn(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let authorize = Mutex::new(authorize);
        Self {
            local: Arc::new(move |db| (authorize.lock().unwrap())(db)),
            admission: None,
            publication: None,
            service: None,
            live: None,
            job: None,
            job_bearer: None,
        }
    }

    pub(crate) fn new<F>(s: &Service, authorize: F) -> Self
    where
        F: Fn(&Db<'_>) -> Result<()> + Send + 'static,
    {
        Self::local(authorize).for_service(s)
    }

    pub(crate) fn for_service(mut self, s: &Service) -> Self {
        if self.service.is_none() {
            self.service = Some(Arc::new(s.clone()));
        }
        self
    }

    /// Preserve APIs whose existing publication callback is consumed once.
    pub(crate) fn once<F>(authorize: F) -> Self
    where
        F: FnOnce(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let callback = Mutex::new(Some(authorize));
        Self {
            admission: Some(Arc::new(move |db| {
                let authorize = callback.lock().unwrap().take().ok_or_else(|| {
                    Error::forbidden("The publication authorization was already consumed.")
                })?;
                authorize(db)
            })),
            ..Self::default()
        }
    }

    pub(crate) fn live(s: &Service, authority: Authority) -> Self {
        Self {
            local: Arc::new(|_| Ok(())),
            admission: None,
            publication: None,
            service: Some(Arc::new(s.clone())),
            live: Some(authority),
            job: None,
            job_bearer: None,
        }
    }

    pub(crate) fn installation_management(s: &Service, authority: Authority, bearer: &str) -> Self {
        let mut authorization = Self::live(s, authority);
        authorization.job_bearer = Some(bearer.to_owned());
        authorization
    }

    pub(crate) fn with_guard<F>(self, guard: F) -> Self
    where
        F: Fn(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let local = self.local;
        let guard = Mutex::new(guard);
        Self {
            local: Arc::new(move |db| {
                local(db)?;
                (guard.lock().unwrap())(db)
            }),
            admission: self.admission,
            publication: self.publication,
            service: self.service,
            live: self.live,
            job: self.job,
            job_bearer: self.job_bearer,
        }
    }

    /// Provenance recording belongs to the final write transaction. Unlike
    /// read-only guards, it must not run during asynchronous proof checks.
    pub(crate) fn with_publication<F>(mut self, publish: F) -> Self
    where
        F: Fn(&Db<'_>) -> Result<()> + Send + 'static,
    {
        let previous = self.publication.take();
        let publish = Mutex::new(publish);
        self.publication = Some(Arc::new(move |db| {
            if let Some(previous) = &previous {
                previous(db)?;
            }
            (publish.lock().unwrap())(db)
        }));
        self
    }

    pub(crate) fn authorize(&self, db: &Db<'_>) -> Result<()> {
        self.check_local(db)?;
        if let Some(admission) = &self.admission {
            admission(db)?;
        }
        if let Some(publish) = &self.publication {
            publish(db)?;
        }
        Ok(())
    }

    pub(crate) fn check_local(&self, db: &Db<'_>) -> Result<()> {
        if let Some(authority) = &self.live {
            authority.authorize(db)?;
        }
        if let Some(job) = &self.job {
            job.authorize(db)?;
        }
        (self.local)(db)?;
        Ok(())
    }

    /// Call after preparation/lock waits and before each new effect/publication.
    /// Local checks remain inside the final transaction as well. A legacy
    /// one-shot callback is left for that transaction, never consumed here.
    pub(crate) fn recheck(
        &self,
    ) -> futures_util::future::Either<
        futures_util::future::Ready<Result<()>>,
        futures_util::future::BoxFuture<'_, Result<()>>,
    > {
        use futures_util::future::{Either, ready};
        let Some(service) = &self.service else {
            return Either::Left(ready(Ok(())));
        };
        // Live checks may resolve nested resources. Keep that state off the
        // effect caller's future frame; trusted local callbacks remain ready.
        Either::Right(Box::pin(async move {
            let check = self.clone();
            service.store.read(move |db| check.check_local(db)).await?;
            if let Some(authority) = &self.live {
                authority.recheck(service).await?;
            }
            if let Some(job) = &self.job {
                job.recheck(service).await?;
            }
            let check = self.clone();
            service.store.read(move |db| check.check_local(db)).await
        }))
    }

    /// Only a concrete accepted-job handoff may detach from the turn's bearer.
    /// Extra guards and final-publication provenance remain attached.
    pub(crate) async fn accepted(mut self) -> Result<Self> {
        self.recheck().await?;
        if let Some(bearer) = &self.job_bearer {
            let service = self
                .service
                .as_ref()
                .ok_or_else(|| Error::forbidden("Missing initiating service."))?;
            let job = jobs::Accepted::capture(service, bearer).await?;
            // A cancellation racing registration must also invalidate admission.
            self.recheck().await?;
            self.live = None;
            self.job = Some(job);
            self.job_bearer = None;
        } else if self
            .live
            .as_ref()
            .is_some_and(|authority| authority.bearer.is_some())
        {
            return Err(Error::forbidden(
                "This authority cannot be retained as a management job.",
            ));
        }
        // The request's one-shot callback still runs in its admission transaction.
        // It is not a reusable background grant or a second publication callback.
        self.admission = None;
        Ok(self)
    }

    pub(crate) async fn cancelled(&self) {
        match &self.job {
            Some(job) => job.cancelled().await,
            None => futures_util::future::pending().await,
        }
    }

    pub(crate) fn has_async_checks(&self) -> bool {
        self.service.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_grant_fixture::Fixture;
    use crate::{mission_fixture::MissionFixture, validation::text};
    use serde_json::json;
    use std::sync::atomic::Ordering;

    async fn delegated() -> (Fixture, String) {
        Fixture::delegated_manager().await
    }

    #[tokio::test]
    async fn prepared_run_admission_retains_the_original_delegation_proof() {
        let (f, bearer) = delegated().await;
        let authorize = crate::installation_mcp::management_authorization(&f.s, Some(&bearer))
            .await
            .unwrap();
        let task =
            f.s.task_fixture(
                json!({
                    "name": "Managed task",
                    "agentId": crate::config::MAIN_AGENT_ID,
                    "prompt": "Run",
                }),
                None,
            )
            .await
            .unwrap();
        let prepared = f.s.prepare_run(text(&task, "id"), "mcp").await.unwrap();
        let id = text(&prepared, "id").to_owned();
        f.moved_parent.store(true, Ordering::SeqCst);
        let error =
            f.s.queue_run_live(prepared, None, Some(bearer), authorize)
                .await
                .unwrap_err();
        assert_eq!(error.status, 403);
        assert!(
            f.s.store
                .read(move |db| db.run(&id))
                .await
                .unwrap()
                .is_none()
        );
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn cancellation_rechecks_delegation_after_the_worker_lock() {
        let (f, bearer) = delegated().await;
        let checked = Arc::new(tokio::sync::Notify::new());
        let signal = checked.clone();
        let (_, authority) = crate::installation_mcp::authorize_with_authority(&f.s, &bearer)
            .await
            .unwrap();
        let authority = authority.with_recheck_guard(move |_| {
            let signal = signal.clone();
            Box::pin(async move {
                signal.notify_one();
                Ok(())
            })
        });
        let authorize = Authorization::live(&f.s, authority);
        let lock = f.s.worker.active.lock().await;
        let service = f.s.clone();
        let run = f.run.clone();
        let cancel =
            tokio::spawn(
                async move { service.worker.cancel_live(&service, &run, authorize).await },
            );
        tokio::time::timeout(std::time::Duration::from_secs(10), checked.notified())
            .await
            .unwrap();
        f.moved_parent.store(true, Ordering::SeqCst);
        drop(lock);
        let error = cancel.await.unwrap().unwrap_err();
        assert_eq!(error.status, 403);
        assert_eq!(f.s.store.run(&f.run).await.unwrap()["status"], "running");
        f.s.shutdown.cancel();
    }

    async fn oauth_instance(f: &Fixture) -> crate::plugins::connections::Connection {
        let id = crate::config::id();
        let mut connection = crate::plugins::connections::Connection::draft(
            &id,
            "gmail",
            json!({ "clientId": "fixture-client" }),
            Vec::new(),
        );
        connection.enabled = true;
        connection.name = "Fixture OAuth account".into();
        connection.revision = crate::config::id();
        let saved = connection.clone();
        f.s.store
            .transaction(move |db| db.save_plugin_instance(&saved))
            .await
            .unwrap();
        f.s.vault
            .set(
                &crate::plugins::connections::credential_key(&id),
                &json!({ "token": "synthetic-credential" }),
            )
            .await
            .unwrap();
        f.s.store.plugin_instance(&id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn plugin_lifecycle_mutations_recheck_live_delegation_before_publication() {
        for kind in ["enabled", "delete"] {
            let (f, bearer) = delegated().await;
            let connection = oauth_instance(&f).await;
            let before = serde_json::to_value(&connection).unwrap();
            let (_, authority) = crate::installation_mcp::authorize_with_authority(&f.s, &bearer)
                .await
                .unwrap();
            // The initial semantic check succeeds, then the external hierarchy
            // changes without a local grant, package or instance revision.
            let moved = f.moved_parent.clone();
            let authority = authority.with_recheck_guard(move |_| {
                moved.store(true, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            });
            let mut input = json!({
                "kind": kind,
                "pluginId": "gmail",
                "instanceId": connection.id,
            });
            if kind == "enabled" {
                input["data"] = json!({ "enabled": false });
            }
            let error = crate::plugins::management::invoke(
                &f.s,
                serde_json::from_value(input).unwrap(),
                Some(&bearer),
                false,
                Some(authority),
            )
            .await
            .unwrap_err();
            assert_eq!(error.status, 403);
            let after = f.s.store.plugin_instance(&connection.id).await.unwrap();
            assert_eq!(
                after.map(|v| serde_json::to_value(v).unwrap()),
                Some(before),
                "{kind} changed a plugin instance after delegation ceased to cover it"
            );
            assert!(
                f.s.vault
                    .get(&crate::plugins::connections::credential_key(&connection.id))
                    .await
                    .unwrap()
                    .is_some()
            );
            f.s.shutdown.cancel();
        }
    }

    #[tokio::test]
    async fn oauth_disconnect_rechecks_live_delegation_after_the_instance_lock() {
        let (f, bearer) = delegated().await;
        let connection = oauth_instance(&f).await;
        let before = serde_json::to_value(&connection).unwrap();
        let checked = Arc::new(tokio::sync::Notify::new());
        let completed = checked.clone();
        let (_, authority) = crate::installation_mcp::authorize_with_authority(&f.s, &bearer)
            .await
            .unwrap();
        let authority = authority.with_recheck_guard(move |_| {
            completed.notify_one();
            Box::pin(async { Ok(()) })
        });
        let instance_lock = f.s.plugins.oauth_flows.lock(&connection.id);
        let guard = instance_lock.lock().await;
        let service = f.s.clone();
        let input = serde_json::from_value(json!({
            "kind": "disconnect", "pluginId": "gmail", "instanceId": connection.id,
        }))
        .unwrap();
        let disconnect = tokio::spawn(async move {
            crate::plugins::management::invoke(
                &service,
                input,
                Some(&bearer),
                false,
                Some(authority),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), checked.notified())
            .await
            .unwrap();
        f.moved_parent.store(true, Ordering::SeqCst);
        drop(guard);
        let error = disconnect.await.unwrap().unwrap_err();
        assert_eq!(error.status, 403);
        let after = f.s.store.plugin_instance(&connection.id).await.unwrap();
        assert_eq!(
            after.map(|v| serde_json::to_value(v).unwrap()),
            Some(before),
            "disconnect changed the instance before its final authority refusal"
        );
        assert!(
            f.s.vault
                .get(&crate::plugins::connections::credential_key(&connection.id))
                .await
                .unwrap()
                .is_some(),
            "disconnect deleted the credential after losing delegation coverage"
        );
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn oauth_disconnect_keeps_the_instance_mutex_free_during_live_checks() {
        let (f, bearer) = delegated().await;
        let connection = oauth_instance(&f).await;
        let (_, authority) = crate::installation_mcp::authorize_with_authority(&f.s, &bearer)
            .await
            .unwrap();
        let instance = connection.id.clone();
        let authority = authority.with_recheck_guard(move |s| {
            let instance = instance.clone();
            Box::pin(async move {
                // A hierarchy resolver may need this same account's OAuth
                // token. It must be able to acquire the refresh mutex.
                let lock = s.plugins.oauth_flows.lock(&instance);
                let _guard = lock.lock().await;
                Ok(())
            })
        });
        let input = serde_json::from_value(json!({
            "kind": "disconnect", "pluginId": "gmail", "instanceId": connection.id,
        }))
        .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            crate::plugins::management::invoke(&f.s, input, Some(&bearer), false, Some(authority)),
        )
        .await
        .expect("live resource checks deadlocked on the OAuth mutex")
        .unwrap();
        assert!(
            !f.s.store
                .plugin_instance(&connection.id)
                .await
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(
            f.s.vault
                .get(&crate::plugins::connections::credential_key(&connection.id))
                .await
                .unwrap()
                .is_none()
        );
        f.s.shutdown.cancel();
    }

    #[tokio::test]
    async fn delegated_selected_oauth_disconnect_revokes_before_its_own_revision_change() {
        use axum::{
            Router,
            http::StatusCode,
            routing::{get, post},
        };
        use std::sync::atomic::AtomicUsize;
        let (f, bearer) = delegated().await;
        let connection = oauth_instance(&f).await;
        let run = crate::installation_mcp::authorize(&f.s, &bearer)
            .await
            .unwrap();
        for agent in [
            crate::config::MAIN_AGENT_ID,
            text(&run["snapshot"]["agent"], "id"),
        ] {
            let current = f.api("GET", json!({ "agentId": agent })).await.unwrap();
            let mut grants = current["grants"].as_array().unwrap().clone();
            grants.push(json!({ "instanceId": connection.id, "access": {
                "type": "selected", "contributions": [{ "kind": "instructions", "name": "", "resources": [] }],
            } }));
            f.api(
                "PUT",
                json!({ "agentId": agent, "revision": current["revision"],
                    "grants": grants, "runCredentialInstances": [],
                }),
            )
            .await
            .unwrap();
        }
        let revocations = Arc::new(AtomicUsize::new(0));
        let count = revocations.clone();
        let (origin, server) = crate::common::serve_locally(
            Router::new()
                .route("/effect", get(|| async { "ok" }))
                .route(
                    "/revoke",
                    post(move || {
                        let count = count.clone();
                        async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            StatusCode::OK
                        }
                    }),
                ),
        )
        .await;
        f.s.plugins.route_http_to(Some(origin));
        let (_, authority) = crate::installation_mcp::authorize_with_authority(&f.s, &bearer)
            .await
            .unwrap();
        let input = serde_json::from_value(json!({
            "kind": "disconnect",
            "pluginId": "gmail",
            "instanceId": connection.id,
        }))
        .unwrap();
        let result =
            crate::plugins::management::invoke(&f.s, input, Some(&bearer), false, Some(authority))
                .await
                .unwrap();
        assert_eq!(serde_json::to_value(result).unwrap()["revoked"], true);
        assert_eq!(revocations.load(Ordering::SeqCst), 1);
        assert!(
            !f.s.store
                .plugin_instance(&connection.id)
                .await
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(
            f.s.vault
                .get(&crate::plugins::connections::credential_key(&connection.id))
                .await
                .unwrap()
                .is_none()
        );
        server.abort();
        f.s.shutdown.cancel();
    }
}
