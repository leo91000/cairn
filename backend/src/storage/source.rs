//! A prepared disk has no remote authority. Adoption binds it once, after its
//! authorization has been durably recorded, without replacing the open journal.
use super::{BlockSource, remote::RemoteSource};
use crate::error::{Error, Result};
use serde_json::Value;
use std::{
    io,
    sync::{Arc, Mutex, RwLock, TryLockError},
};
use tokio_util::sync::CancellationToken;

pub struct Source {
    binding: RwLock<Option<Arc<RemoteSource>>>,
    assignment: Mutex<()>,
    runtime: tokio::runtime::Handle,
    stop: CancellationToken,
}

impl Source {
    pub(crate) fn new(
        context: &Value,
        runtime: tokio::runtime::Handle,
        stop: CancellationToken,
    ) -> Result<Self> {
        if context["unassigned"] == true {
            return Err(Error::bad(
                "Conversation authorization cannot be marked unassigned.",
            ));
        }
        let bound = Arc::new(RemoteSource::new(context, runtime.clone(), stop.clone())?);
        Ok(Self {
            binding: RwLock::new(Some(bound)),
            assignment: Mutex::new(()),
            runtime,
            stop,
        })
    }

    pub(crate) fn prepared(runtime: tokio::runtime::Handle, stop: CancellationToken) -> Self {
        Self {
            binding: RwLock::new(None),
            assignment: Mutex::new(()),
            runtime,
            stop,
        }
    }

    fn binding(&self) -> io::Result<Option<Arc<RemoteSource>>> {
        Ok(self
            .binding
            .read()
            .map_err(|error| io::Error::other(error.to_string()))?
            .clone())
    }

    pub(crate) fn authorization(&self) -> Result<Option<String>> {
        Ok(self.binding()?.map(|source| source.grant_id()))
    }

    pub fn grant_id(&self) -> Result<String> {
        self.authorization()?
            .ok_or_else(|| Error::conflict("Prepared disk has no conversation authorization."))
    }

    pub fn waiting(&self) -> bool {
        self.binding().map_or(true, |binding| {
            binding.is_some_and(|source| source.waiting())
        })
    }

    /// Never rotate an existing disk grant: outstanding publication receipts
    /// and mounted base pins belong to that identity until acknowledgement.
    pub(crate) fn assign(
        &self,
        context: &Value,
        persist: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        if context["unassigned"] == true {
            return Err(Error::bad(
                "Conversation authorization cannot be marked unassigned.",
            ));
        }
        let _assignment = self.assignment.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => {
                Error::conflict("Disk authorization assignment is in progress.")
            }
            TryLockError::Poisoned(error) => Error::internal(error),
        })?;
        if self.binding()?.is_some() {
            return Err(Error::conflict(
                "Disk authorization has already been assigned.",
            ));
        }
        let bound = Arc::new(RemoteSource::new(
            context,
            self.runtime.clone(),
            self.stop.clone(),
        )?);
        // Health/control probes keep seeing an unassigned source throughout
        // slow fsync. Never hold their read lock across persistent disk I/O.
        persist()?;
        *self.binding.write().map_err(Error::internal)? = Some(bound);
        Ok(())
    }
}

impl BlockSource for Source {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        let bound = self.binding()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Prepared disk cannot read remote blocks",
            )
        })?;
        bound.fetch(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context() -> Value {
        json!({ "master": "http://127.0.0.1:1/", "grant": "scoped-fixture" })
    }

    #[tokio::test]
    async fn prepared_source_rejects_reads_and_publication_until_persistence_succeeds() {
        let source = Source::prepared(tokio::runtime::Handle::current(), CancellationToken::new());
        assert_eq!(
            source.fetch(&"a".repeat(64)).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(source.grant_id().is_err());
        assert!(!source.waiting());
        assert!(
            source
                .assign(&context(), || Err(Error::unavailable(
                    "fixture fsync failure"
                )))
                .is_err()
        );
        assert!(source.grant_id().is_err());
        source.assign(&context(), || Ok(())).unwrap();
        assert_eq!(
            source.grant_id().unwrap(),
            crate::auth::digest("scoped-fixture")
        );
        let other = json!({ "master": "http://127.0.0.1:2/", "grant": "another-owner" });
        assert!(
            source
                .assign(&other, || panic!("must not persist a second owner"))
                .is_err()
        );
        assert_eq!(
            source.grant_id().unwrap(),
            crate::auth::digest("scoped-fixture")
        );
    }

    #[tokio::test]
    async fn slow_authorization_fsync_does_not_block_health_or_grant_revocation_checks() {
        let source = Arc::new(Source::prepared(
            tokio::runtime::Handle::current(),
            CancellationToken::new(),
        ));
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (resume, gate) = std::sync::mpsc::channel();
        let assigning = {
            let source = source.clone();
            tokio::task::spawn_blocking(move || {
                source.assign(&context(), || {
                    entered.send(()).unwrap();
                    gate.recv_timeout(std::time::Duration::from_secs(3))
                        .map_err(Error::internal)?;
                    Ok(())
                })
            })
        };
        waiting.await.unwrap();
        let probing = {
            let source = source.clone();
            tokio::task::spawn_blocking(move || {
                assert!(!source.waiting());
                assert!(source.grant_id().is_err());
                assert_eq!(
                    source.fetch(&"a".repeat(64)).unwrap_err().kind(),
                    io::ErrorKind::PermissionDenied
                );
                assert!(
                    source
                        .assign(&context(), || panic!(
                            "second assignment must not enter persistence"
                        ))
                        .is_err()
                );
            })
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), probing).await;
        // Always release the simulated fsync, even if the probe was blocked.
        resume.send(()).unwrap();
        assigning.await.unwrap().unwrap();
        assert!(
            result.is_ok(),
            "control probe blocked behind authorization fsync"
        );
        result.unwrap().unwrap();
        assert!(source.grant_id().is_ok());
    }

    #[tokio::test]
    async fn assigned_source_uses_only_its_grant_and_preserves_cancellation() {
        use axum::{Router, http::HeaderMap, routing::get};
        let router = Router::new().route(
            "/internal/node-restore/{hash}",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer owner-fixture");
                "scoped block"
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
        let stop = CancellationToken::new();
        let source = Arc::new(Source::prepared(
            tokio::runtime::Handle::current(),
            stop.clone(),
        ));
        source
            .assign(
                &json!({ "master": origin, "grant": "owner-fixture" }),
                || Ok(()),
            )
            .unwrap();
        let read = |source: Arc<Source>| {
            tokio::task::spawn_blocking(move || source.fetch(&"a".repeat(64)))
        };
        assert_eq!(
            read(source.clone()).await.unwrap().unwrap(),
            b"scoped block"
        );
        stop.cancel();
        assert_eq!(
            read(source).await.unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        server.abort();
    }

    #[tokio::test]
    async fn normal_sources_still_require_a_real_authorization() {
        assert!(
            Source::new(
                &json!({"unassigned": true}),
                tokio::runtime::Handle::current(),
                CancellationToken::new()
            )
            .is_err()
        );
    }
}
