//! The gateway advertises verified tool descriptors without a round trip to a
//! remote server on every agent startup. The catalogue itself confers no grant.
use crate::{
    authorization::Authorization,
    capabilities::plugin_packages::Authority,
    error::{Error, Result},
    mcp_client::Client,
    mcps::record::{GrantScope, McpServer},
    service::Service,
    validation::{encoded_size, text},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

const REFRESH_AFTER: Duration = Duration::from_secs(60);
const EXPIRES_AFTER: Duration = Duration::from_secs(300);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 32;
const MAX_CATALOG_BYTES: usize = 1024 * 1024;

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

    /// Update the entry that requested refresh. A newer cold or management
    /// discovery and an invalidated entry both take precedence.
    fn apply_refresh(
        &self,
        id: &str,
        expected: Identity,
        previous: &Arc<Vec<Value>>,
        tools: Option<Vec<Value>>,
    ) {
        let tools =
            tools.filter(|tools| encoded_size(tools).is_ok_and(|bytes| bytes <= MAX_CATALOG_BYTES));
        let mut entries = self.0.lock().unwrap();
        let Some(current) = entries.get_mut(id) else {
            return;
        };
        if current.identity != expected || !Arc::ptr_eq(previous, &current.tools) {
            return;
        }

        if let Some(tools) = tools {
            let now = Instant::now();
            current.tools = Arc::new(tools);
            current.discovered_at = now;
            current.refresh_at = now + REFRESH_AFTER;
        } else {
            entries.remove(id);
        }
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
    let access = Access::for_grant(s, id, bearer).await?;
    let identity = access.identity(s).await?;
    if let Some((tools, refresh)) = s.mcps.gateway_catalogs.get(id, identity) {
        tracing::info!(target: "cairn_performance", operation = "mcp_gateway_catalog", connection_id = id, event = "hit", refresh);
        let result = access.listing(&tools);
        if refresh {
            refresh_in_background(s.clone(), access, identity, tools);
        }
        return Ok(result);
    }

    // Retain serialization for a cold discovery and reuse the result if another
    // authorized caller populated the catalogue while this one was queued.
    let _guard = s.mcps.lock(id).await;
    let access = Access::for_grant(s, id, bearer).await?;
    let identity = access.identity(s).await?;
    if let Some((tools, refresh)) = s.mcps.gateway_catalogs.get(id, identity) {
        let result = access.listing(&tools);
        if refresh {
            refresh_in_background(s.clone(), access, identity, tools);
        }
        return Ok(result);
    }

    let tools = discover(s, &access, identity).await?;
    Ok(access.listing(&tools))
}

fn refresh_in_background(
    s: Arc<Service>,
    access: Access,
    expected: Identity,
    previous: Arc<Vec<Value>>,
) {
    tokio::spawn(async move {
        let refresh = async {
            let id = access.server.id.clone();
            let authorize = access.authorize.with_guard(move |db| {
                let current = db.mcp_credentials(&id)?.map(|bytes| blake3::hash(&bytes));
                if current != expected.credentials {
                    return Err(Error::conflict("MCP credentials changed during discovery."));
                }
                Ok(())
            });

            let id = &access.server.id;
            let mut timing =
                crate::performance::Operation::new("mcp_gateway_refresh", id, "connect");
            let discovery = tokio::time::timeout(REFRESH_TIMEOUT, async {
                let mut client =
                    Client::connect_for_catalog_refresh(&s, &access.server, authorize.clone())
                        .await?;
                timing.next("list");
                let tools = client.discover().await;
                Ok::<_, Error>((client, tools))
            })
            .await;
            let (client, result) = match discovery {
                Ok(Ok((client, tools))) => (Some(client), tools),
                Ok(Err(error)) => (None, Err(error)),
                Err(_) => return Ok(()),
            };

            timing.next("publish");
            authorize.recheck().await?;
            match result {
                Ok(tools) if validate(&tools).is_ok() => {
                    s.mcps
                        .gateway_catalogs
                        .apply_refresh(id, expected, &previous, Some(tools));
                }
                Err(error) if crate::mcp_client::upstream_access_denied(&error) => {
                    s.mcps
                        .gateway_catalogs
                        .apply_refresh(id, expected, &previous, None);
                }
                _ => {}
            }
            timing.next("close");
            if let Some(client) = client {
                // The client bounds legacy DELETE separately from discovery.
                client.close().await;
            }
            timing.finish();
            Ok::<_, Error>(())
        };
        tokio::select! {
            () = s.shutdown.cancelled() => {}
            _ = refresh => {}
        }
    });
}

async fn read_tools(
    mut client: Client,
    timing: &mut crate::performance::Operation,
) -> Result<Vec<Value>> {
    timing.next("list");
    let tools = client.discover().await;
    timing.next("close");
    client.close().await;

    timing.next("validate");
    let tools = tools?;
    validate(&tools)?;
    Ok(tools)
}

async fn discover(
    s: &Arc<Service>,
    access: &Access,
    identity: Identity,
) -> Result<Arc<Vec<Value>>> {
    let mut timing =
        crate::performance::Operation::new("mcp_gateway_discovery", &access.server.id, "connect");
    let result = async {
        let client =
            Client::connect_server_live(s, &access.server, access.authority.clone()).await?;
        read_tools(client, &mut timing).await
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
    use crate::{
        common::{McpFixture, MissionFixture},
        config::MAIN_AGENT_ID,
        run_status::RunStatus,
        runs::RunChanges,
    };

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

    #[tokio::test]
    async fn stale_catalogue_refresh_runs_alongside_calls_and_publishes_before_close() {
        use axum::{
            Json, Router,
            http::{HeaderMap, StatusCode},
            response::IntoResponse,
            routing::post,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Notify;

        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let close_entered = Arc::new(Notify::new());
        let close_release = Arc::new(Notify::new());
        let listings = Arc::new(AtomicUsize::new(0));
        let sessions = Arc::new(AtomicUsize::new(0));
        let router = Router::new().route(
            "/mcp",
            post({
                let (entered, release) = (entered.clone(), release.clone());
                move |Json(request): Json<Value>| {
                    let (entered, release, listings, sessions) =
                        (entered.clone(), release.clone(), listings.clone(), sessions.clone());
                    async move {
                        let mut headers = HeaderMap::new();
                        let result = match text(&request, "method") {
                            "server/discover" => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
                            "initialize" => {
                                let session = sessions.fetch_add(1, Ordering::Relaxed) + 1;
                                headers.insert("mcp-session-id", session.to_string().parse().unwrap());
                                json!({
                                    "protocolVersion": crate::mcp_client::LEGACY,
                                    "capabilities": { "tools": {} },
                                })
                            }
                            "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
                            "tools/list" => {
                                let name = if listings.fetch_add(1, Ordering::Relaxed) > 0 {
                                    entered.notify_one();
                                    release.notified().await;
                                    "updated_fixture"
                                } else {
                                    "fixture"
                                };
                                json!({
                                    "tools": [{ "name": name, "inputSchema": { "type": "object" } }],
                                })
                            }
                            "tools/call" => json!({ "content": [{ "type": "text", "text": "live" }] }),
                            method => panic!("unexpected MCP method {method}"),
                        };
                        (headers, Json(json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }))).into_response()
                    }
                }
            }).delete({
                let (entered, release) = (close_entered.clone(), close_release.clone());
                move |headers: HeaderMap| {
                    let (entered, release) = (entered.clone(), release.clone());
                    async move {
                        if headers.get("mcp-session-id").is_some_and(|id| id == "2") {
                            entered.notify_one();
                            release.notified().await;
                        }
                        StatusCode::NO_CONTENT
                    }
                }
            }),
        );
        let (listener, address) = crate::common::bind().await;
        let upstream = crate::common::serve(listener, router);
        let root = tempfile::tempdir().unwrap();
        let s = Service::new(crate::common::config(root.path()))
            .await
            .unwrap();
        let connection = s
            .mcp_fixture(
                json!({
                    "name": "Gated discovery fixture",
                    "url": format!("http://{address}/mcp"),
                    "allowPrivateNetwork": true,
                }),
                None,
            )
            .await
            .unwrap();
        let id = text(&connection, "id");
        let task = s
            .task_fixture(
                json!({
                    "name": "Live gateway fixture",
                    "agentId": MAIN_AGENT_ID,
                    "prompt": "Fixture only",
                    "worktree": false,
                }),
                None,
            )
            .await
            .unwrap();
        let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
        s.store
            .change_run(
                text(&run, "id"),
                RunChanges {
                    status: Some(RunStatus::Running),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let configuration = s.mcps.run_configuration(&s, &run).await.unwrap();
        let bearer = text(&configuration["env"], crate::mcps::RUN_TOKEN_ENV);
        list(&s, id, bearer).await.unwrap();
        s.mcps
            .gateway_catalogs
            .0
            .lock()
            .unwrap()
            .get_mut(id)
            .unwrap()
            .refresh_at = Instant::now();
        assert_eq!(
            list(&s, id, bearer).await.unwrap()["tools"][0]["name"],
            "fixture"
        );
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            super::super::proxy(
                &s,
                id,
                bearer,
                "tools/call",
                json!({ "name": "fixture", "arguments": {} }),
            ),
        )
        .await
        .expect("background discovery must not own the live connection lock")
        .unwrap();
        assert_eq!(result["content"][0]["text"], "live");
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(3), close_entered.notified())
            .await
            .unwrap();
        assert_eq!(
            list(&s, id, bearer).await.unwrap()["tools"][0]["name"],
            "updated_fixture"
        );
        close_release.notify_one();
        upstream.abort();
    }
}
