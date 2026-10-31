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
            service: self.service,
            live: self.live,
        }
    }

    pub(crate) fn authorize(&self, db: &Db<'_>) -> Result<()> {
        (self.local)(db)
    }

    /// Call after preparation/lock waits and before each new effect/publication.
    /// Local checks remain inside the final transaction as well. A legacy
    /// one-shot callback is left for that transaction, never consumed here.
    pub(crate) async fn recheck(&self) -> Result<()> {
        let Some(service) = &self.service else {
            return Ok(());
        };
        let check = self.local.clone();
        service.store.read(move |db| check(db)).await?;
        if let Some(authority) = &self.live {
            authority.recheck(service).await?;
        }
        let check = self.local.clone();
        service.store.read(move |db| check(db)).await
    }
}
