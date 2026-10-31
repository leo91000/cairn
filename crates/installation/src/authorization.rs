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

#[derive(Clone)]
pub(crate) struct Authorization {
    local: Arc<LocalGuard>,
    publication: Option<Arc<LocalGuard>>,
    service: Option<Arc<Service>>,
    live: Option<Authority>,
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
            publication: None,
            service: None,
            live: None,
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
        Self::local(move |db| {
            let authorize = callback.lock().unwrap().take().ok_or_else(|| {
                Error::forbidden("The publication authorization was already consumed.")
            })?;
            authorize(db)
        })
    }

    pub(crate) fn live(s: &Service, authority: Authority) -> Self {
        let local = authority.clone();
        Self {
            local: Arc::new(move |db| local.authorize(db)),
            publication: None,
            service: Some(Arc::new(s.clone())),
            live: Some(authority),
        }
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
            publication: self.publication,
            service: self.service,
            live: self.live,
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
        (self.local)(db)?;
        if let Some(publish) = &self.publication {
            publish(db)?;
        }
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
            let check = self.local.clone();
            service.store.read(move |db| check(db)).await?;
            if let Some(authority) = &self.live {
                authority.recheck(service).await?;
            }
            let check = self.local.clone();
            service.store.read(move |db| check(db)).await
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_grant_fixture::{Fixture, contribution, support};
    use crate::{agent_fixture::AgentFixture, mission_fixture::MissionFixture, validation::text};
    use serde_json::json;
    use std::sync::atomic::Ordering;

    async fn delegated() -> (Fixture, String) {
        let f = Fixture::new().await;
        f.marker("live-parent").await;
        f.set(json!([contribution("tool", "grant_probe_post", support())]))
            .await;
        let child =
            f.s.agent_fixture(
                json!({ "name": "Child", "access": { "installation": true } }),
                None,
            )
            .await
            .unwrap();
        let child_id = text(&child, "id");
        f.set_for(
            child_id,
            json!([contribution(
                "tool",
                "grant_probe_post",
                json!([{ "resourceType": "thread", "id": "support-thread" }])
            )]),
        )
        .await;
        let (_, bearer) = f.delegated(crate::config::MAIN_AGENT_ID, child_id).await;
        (f, bearer)
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
}
