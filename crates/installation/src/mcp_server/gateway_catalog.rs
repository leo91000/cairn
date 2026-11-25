//! The gateway advertises verified tool descriptors without a round trip to a
//! remote server on every agent startup. The catalogue itself confers no grant.
use crate::{
    authorization::Authorization,
    capabilities::plugin_packages::Authority,
    error::Result,
    mcp_client::Client,
    mcps::record::{GrantScope, McpServer},
    service::Service,
    validation::{encoded_size, text},
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const REFRESH_AFTER: Duration = Duration::from_secs(60);
const EXPIRES_AFTER: Duration = Duration::from_secs(300);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 32;
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const PRELOAD_CONCURRENCY: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    revision: u64,
    // Use the authenticated-encryption record, not the decrypted credential.
    // OAuth can replace it without changing the connection's configuration.
    credentials: Option<blake3::Hash>,
}

struct Entry {
    identity: Identity,
    tools: Arc<Vec<Value>>,
    discovered_at: Instant,
    refresh_at: Instant,
}

#[derive(Default)]
pub(crate) struct Catalogs(Mutex<HashMap<String, Entry>>);

impl Catalogs {
    fn get(&self, id: &str, identity: Identity) -> Option<(Arc<Vec<Value>>, bool)> {
        let now = Instant::now();
        let mut entries = self.0.lock().unwrap();
        let entry = entries.get_mut(id)?;
        if entry.identity != identity || now.duration_since(entry.discovered_at) >= EXPIRES_AFTER {
            return None;
        }

        let refresh = now >= entry.refresh_at;
        if refresh {
            // Coalesce concurrent refreshes and back off when the remote fails.
            entry.refresh_at = now + REFRESH_AFTER;
        }
        Some((entry.tools.clone(), refresh))
    }

    fn insert(&self, id: String, identity: Identity, tools: Arc<Vec<Value>>) {
        if !encoded_size(tools.as_ref()).is_ok_and(|bytes| bytes <= MAX_CATALOG_BYTES) {
            self.invalidate(&id);
            return;
        }

        let now = Instant::now();
        let mut entries = self.0.lock().unwrap();
        if entries.len() >= MAX_CONNECTIONS
            && !entries.contains_key(&id)
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.discovered_at)
                .map(|(key, _)| key.clone())
        {
            entries.remove(&oldest);
        }
        entries.insert(
            id,
            Entry {
                identity,
                tools,
                discovered_at: now,
                refresh_at: now + REFRESH_AFTER,
            },
        );
    }

    pub(crate) fn invalidate(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }

    pub(crate) fn remember_management_discovery(
        &self,
        server: McpServer,
        credentials: Option<blake3::Hash>,
    ) {
        if validate(&server.tools).is_ok() {
            self.insert(
                server.id,
                Identity {
                    revision: server.revision,
                    credentials,
                },
                Arc::new(server.tools),
            );
        } else {
            self.invalidate(&server.id);
        }
    }
}

fn validate(tools: &[Value]) -> Result<()> {
    for tool in tools {
        super::model_result::validate_descriptor(tool)?;
    }
    Ok(())
}

struct Access {
    server: McpServer,
    scope: GrantScope,
    authority: Authority,
    authorize: Authorization,
}

impl Access {
    async fn for_grant(s: &Arc<Service>, id: &str, bearer: &str) -> Result<Self> {
        let (server, scope, authority) = s.mcps.grant_authorized(s, id, bearer).await?;
        let authorize = Authorization::live(s, authority.clone());
        Ok(Self {
            server,
            scope,
            authority,
            authorize,
        })
    }

    async fn identity(&self, s: &Service) -> Result<Identity> {
        self.authorize.recheck().await?;
        let id = self.server.id.clone();
        let revision = self.server.revision;
        let authorize = self.authorize.clone();
        s.store
            .read(move |db| {
                authorize.authorize(db)?;
                let credentials = db.mcp_credentials(&id)?.map(|bytes| blake3::hash(&bytes));
                Ok(Identity {
                    revision,
                    credentials,
                })
            })
            .await
    }

    fn listing(&self, tools: &[Value]) -> Value {
        let tools = tools
            .iter()
            .filter(|tool| self.scope.allows(text(tool, "name")))
            .collect::<Vec<_>>();
        json!({ "tools": tools })
    }
}

pub(super) async fn list(s: &Arc<Service>, id: &str, bearer: &str) -> Result<Value> {
    let (access, tools) = authorized_catalog(s, id, bearer).await?;
    Ok(access.listing(&tools))
}

async fn authorized_catalog(
    s: &Arc<Service>,
    id: &str,
    bearer: &str,
) -> Result<(Access, Arc<Vec<Value>>)> {
    let access = Access::for_grant(s, id, bearer).await?;
    let identity = access.identity(s).await?;
    if let Some((tools, refresh)) = s.mcps.gateway_catalogs.get(id, identity) {
        tracing::info!(target: "cairn_performance", operation = "mcp_gateway_catalog", connection_id = id, event = "hit", refresh);
        if refresh {
            refresh_in_background(s.clone(), id.to_owned(), bearer.to_owned(), identity);
        }
        return Ok((access, tools));
    }

    // Retain serialization for a cold discovery and reuse the result if another
    // authorized caller populated the catalogue while this one was queued.
    let _guard = s.mcps.lock(id).await;
    let access = Access::for_grant(s, id, bearer).await?;
    let identity = access.identity(s).await?;
    let tools = match s.mcps.gateway_catalogs.get(id, identity) {
        Some((tools, refresh)) => {
            if refresh {
                refresh_in_background(s.clone(), id.to_owned(), bearer.to_owned(), identity);
            }
            tools
        }
        None => discover(s, &access, identity).await?,
    };
    Ok((access, tools))
}

/// Start authorized cold discovery while the worker prepares the environment.
/// The running turn still owns the bearer and all discovery rechecks.
pub(crate) fn preload(s: &Service, bearer: &str, stop: &CancellationToken) {
    let s = Arc::new(s.clone());
    let (bearer, stop) = (bearer.to_owned(), stop.clone());
    tokio::spawn(async move {
        let preload = async {
            let key = crate::mcps::grant_key(&bearer);
            let Ok(Some(grant)) = s.store.mcp_run_grant(&key).await else {
                return;
            };
            let s = &s;
            let bearer = &bearer;
            stream::iter(grant.servers)
                .for_each_concurrent(PRELOAD_CONCURRENCY, |(id, scope)| async move {
                    if scope.tools.as_ref().is_none_or(|tools| !tools.is_empty()) {
                        let _ = authorized_catalog(s, &id, bearer).await;
                    }
                })
                .await;
        };
        tokio::select! {
            () = s.shutdown.cancelled() => {}
            () = stop.cancelled() => {}
            _ = tokio::time::timeout(REFRESH_TIMEOUT, preload) => {}
        }
    });
}

fn refresh_in_background(s: Arc<Service>, id: String, bearer: String, expected: Identity) {
    tokio::spawn(async move {
        let refresh = async {
            let _guard = s.mcps.lock(&id).await;
            let access = Access::for_grant(&s, &id, &bearer).await?;
            let identity = access.identity(&s).await?;
            if identity == expected {
                discover(&s, &access, identity).await?;
            }
            Ok::<_, crate::error::Error>(())
        };
        tokio::select! {
            () = s.shutdown.cancelled() => {}
            _ = tokio::time::timeout(REFRESH_TIMEOUT, refresh) => {}
        }
    });
}

async fn discover(
    s: &Arc<Service>,
    access: &Access,
    identity: Identity,
) -> Result<Arc<Vec<Value>>> {
    let mut timing =
        crate::performance::Operation::new("mcp_gateway_discovery", &access.server.id, "connect");
    let result = async {
        let mut client =
            Client::connect_server_live(s, &access.server, access.authority.clone()).await?;
        timing.next("list");
        let tools = client.discover().await;
        timing.next("close");
        client.close().await;

        timing.next("validate");
        let tools = tools?;
        validate(&tools)?;
        Ok::<_, crate::error::Error>(tools)
    }
    .await;
    timing.next("recheck");
    let current = access.identity(s).await?;
    match result {
        Ok(tools) => {
            let tools = Arc::new(tools);
            if current == identity {
                s.mcps
                    .gateway_catalogs
                    .insert(access.server.id.clone(), identity, tools.clone());
            }
            timing.finish();
            Ok(tools)
        }
        Err(error) => {
            s.mcps
                .record_discovery_live(
                    s,
                    access.server.clone(),
                    &Err(error.clone()),
                    access.authorize.clone(),
                )
                .await?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_is_coalesced_and_expiry_never_serves_an_old_credential() {
        let catalogs = Catalogs::default();
        let identity = Identity {
            revision: 1,
            credentials: Some(blake3::hash(b"fixture credential record")),
        };
        let tools = Arc::new(vec![json!({ "name": "read" })]);
        catalogs.insert("fixture".into(), identity, tools.clone());

        let (cached, refresh) = catalogs.get("fixture", identity).unwrap();
        assert!(Arc::ptr_eq(&cached, &tools));
        assert!(!refresh);

        catalogs
            .0
            .lock()
            .unwrap()
            .get_mut("fixture")
            .unwrap()
            .refresh_at = Instant::now();
        assert!(catalogs.get("fixture", identity).unwrap().1);
        assert!(!catalogs.get("fixture", identity).unwrap().1);

        assert!(
            catalogs
                .get(
                    "fixture",
                    Identity {
                        credentials: None,
                        ..identity
                    }
                )
                .is_none()
        );
        assert!(
            catalogs
                .get(
                    "fixture",
                    Identity {
                        revision: 2,
                        ..identity
                    }
                )
                .is_none()
        );
        catalogs
            .0
            .lock()
            .unwrap()
            .get_mut("fixture")
            .unwrap()
            .discovered_at -= EXPIRES_AFTER;
        assert!(catalogs.get("fixture", identity).is_none());
    }
}
