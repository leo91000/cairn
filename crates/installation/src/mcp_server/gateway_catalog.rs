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
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::{Instant, timeout_at};

const REFRESH_AFTER: Duration = Duration::from_secs(60);
const EXPIRES_AFTER: Duration = Duration::from_secs(300);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 32;
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_REFRESH_PAGES: usize = 64;
const MAX_REFRESHES: usize = 8;

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
    credential_expires_at: Option<i64>,
}

#[derive(Default)]
pub(crate) struct Catalogs {
    entries: Mutex<HashMap<String, Entry>>,
    refreshing: Arc<Mutex<HashSet<String>>>,
}

struct RefreshLease {
    refreshing: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Drop for RefreshLease {
    fn drop(&mut self) {
        self.refreshing.lock().unwrap().remove(&self.id);
    }
}

impl Catalogs {
    fn begin_refresh(&self, id: &str) -> Option<RefreshLease> {
        let mut active = self.refreshing.lock().unwrap();
        if active.len() >= MAX_REFRESHES || !active.insert(id.to_owned()) {
            return None;
        }
        Some(RefreshLease {
            refreshing: self.refreshing.clone(),
            id: id.to_owned(),
        })
    }

    fn get(&self, id: &str, identity: Identity) -> Option<(Arc<Vec<Value>>, bool)> {
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.get_mut(id)?;
        if entry.identity != identity
            || now.duration_since(entry.discovered_at) >= EXPIRES_AFTER
            || entry
                .credential_expires_at
                .is_some_and(|expires| expires <= crate::config::now())
        {
            return None;
        }

        let refresh = now >= entry.refresh_at;
        if refresh {
            // An in-flight discovery owns refresh until it ends or the
            // catalogue expires. A cold caller can then replace this entry.
            entry.refresh_at = entry.discovered_at + EXPIRES_AFTER;
        }
        Some((entry.tools.clone(), refresh))
    }

    fn insert(
        &self,
        id: String,
        identity: Identity,
        tools: Arc<Vec<Value>>,
        credential_expires_at: Option<i64>,
    ) {
        if !encoded_size(tools.as_ref()).is_ok_and(|bytes| bytes <= MAX_CATALOG_BYTES) {
            self.invalidate(&id);
            return;
        }

        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap();
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
                credential_expires_at,
            },
        );
    }

    pub(crate) fn invalidate(&self, id: &str) {
        self.entries.lock().unwrap().remove(id);
    }

    pub(crate) fn invalidate_matching(
        &self,
        id: &str,
        revision: u64,
        credentials: Option<blake3::Hash>,
    ) {
        let mut entries = self.entries.lock().unwrap();
        if entries.get(id).is_some_and(|entry| {
            entry.identity
                == Identity {
                    revision,
                    credentials,
                }
        }) {
            entries.remove(id);
        }
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
        let mut entries = self.entries.lock().unwrap();
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

    fn defer_refresh_retry(&self, id: &str, expected: Identity, previous: &Arc<Vec<Value>>) {
        if let Some(entry) = self.entries.lock().unwrap().get_mut(id)
            && entry.identity == expected
            && Arc::ptr_eq(previous, &entry.tools)
        {
            entry.refresh_at = Instant::now() + REFRESH_AFTER;
        }
    }

    pub(crate) fn remember_management_discovery(
        &self,
        server: McpServer,
        credentials: Option<blake3::Hash>,
        credential_expires_at: Option<i64>,
    ) {
        if validate(&server.tools).is_ok() {
            self.insert(
                server.id,
                Identity {
                    revision: server.revision,
                    credentials,
                },
                Arc::new(server.tools),
                credential_expires_at,
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
            refresh_in_background(s.clone(), access, identity, tools, REFRESH_TIMEOUT);
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
            refresh_in_background(s.clone(), access, identity, tools, REFRESH_TIMEOUT);
        }
        return Ok(result);
    }

    let tools = discover(s, &access).await?;
    Ok(access.listing(&tools))
}

fn refresh_in_background(
    s: Arc<Service>,
    access: Access,
    expected: Identity,
    previous: Arc<Vec<Value>>,
    refresh_timeout: Duration,
) {
    let Some(lease) = s.mcps.gateway_catalogs.begin_refresh(&access.server.id) else {
        s.mcps
            .gateway_catalogs
            .defer_refresh_retry(&access.server.id, expected, &previous);
        return;
    };
    let deadline = Instant::now() + refresh_timeout;
    tokio::spawn(async move {
        let _lease = lease;
        let id = &access.server.id;
        let refresh = async {
            let connection = id.clone();
            let authorize = access.authorize.with_guard(move |db| {
                let current = db
                    .mcp_credentials(&connection)?
                    .map(|bytes| blake3::hash(&bytes));
                if current != expected.credentials {
                    return Err(Error::conflict("MCP credentials changed during discovery."));
                }
                Ok(())
            });

            let mut timing =
                crate::performance::Operation::new("mcp_gateway_refresh", id, "connect");
            let mcps = Arc::clone(&s.mcps);
            let denied_id = id.clone();
            let denied_entry = previous.clone();
            let on_access_denied = Box::new(move || {
                // A remote denial is about this credential snapshot, even
                // when the initiating grant expires before the response.
                mcps.gateway_catalogs
                    .apply_refresh(&denied_id, expected, &denied_entry, None);
            });
            let session = Client::begin_catalog_refresh(
                &s,
                &access.server,
                authorize.clone(),
                deadline,
                MAX_CATALOG_BYTES,
                Some(on_access_denied),
            )
            .await;
            let (client, result) = match session {
                Ok(mut client) => {
                    let tools = timeout_at(deadline, async {
                        client.negotiate_catalog_refresh().await?;
                        timing.next("list");
                        client
                            .discover_catalog(MAX_CATALOG_BYTES, MAX_REFRESH_PAGES)
                            .await
                    })
                    .await
                    .unwrap_or_else(|_| Err(Error::timeout("MCP catalogue refresh timed out.")));
                    (Some(client), tools)
                }
                Err(error) => (None, Err(error)),
            };

            timing.next("publish");
            if let Ok(tools) = result
                && validate(&tools).is_ok()
                && timeout_at(deadline, authorize.recheck())
                    .await
                    .is_ok_and(|result| result.is_ok())
            {
                s.mcps
                    .gateway_catalogs
                    .apply_refresh(id, expected, &previous, Some(tools));
            }
            timing.next("close");
            if let Some(client) = client {
                // The client bounds legacy DELETE separately from discovery.
                client.close_catalog_refresh().await;
            }
            timing.finish();
            Ok::<_, Error>(())
        };
        tokio::select! {
            () = s.shutdown.cancelled() => {}
            _ = refresh => {}
        }
        s.mcps
            .gateway_catalogs
            .defer_refresh_retry(id, expected, &previous);
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

async fn discover(s: &Arc<Service>, access: &Access) -> Result<Arc<Vec<Value>>> {
    let mut timing =
        crate::performance::Operation::new("mcp_gateway_discovery", &access.server.id, "connect");
    let result = async {
        let client =
            Client::connect_server_live(s, &access.server, access.authority.clone()).await?;
        let credential = client.catalogue_credential();
        read_tools(client, &mut timing)
            .await
            .map(|tools| (tools, credential))
    }
    .await;
    timing.next("recheck");
    let current = access.identity(s).await?;
    match result {
        Ok((tools, credential)) => {
            let tools = Arc::new(tools);
            if let Some(credential) = credential
                && credential.fingerprint == current.credentials
            {
                s.mcps.gateway_catalogs.insert(
                    access.server.id.clone(),
                    current,
                    tools.clone(),
                    credential.expires_at,
                );
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
    fn refresh_slots_survive_entry_eviction_and_bound_total_work() {
        let catalogs = Catalogs::default();
        let mut leases = (0..MAX_REFRESHES)
            .map(|index| catalogs.begin_refresh(&format!("fixture-{index}")).unwrap())
            .collect::<Vec<_>>();
        catalogs.invalidate("fixture-0");
        assert!(catalogs.begin_refresh("fixture-0").is_none());
        assert!(catalogs.begin_refresh("another").is_none());

        drop(leases.swap_remove(0));
        assert!(catalogs.begin_refresh("fixture-0").is_some());
    }

    #[tokio::test]
    async fn background_catalogue_stops_at_cumulative_page_and_byte_limits() {
        use axum::{Json, Router, response::IntoResponse, routing::post};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let padded = Arc::new(AtomicBool::new(false));
        let router = Router::new().route(
            "/mcp",
            post({
                let (calls, padded) = (calls.clone(), padded.clone());
                move |Json(request): Json<Value>| {
                    let (calls, padded) = (calls.clone(), padded.clone());
                    async move {
                        let result = match text(&request, "method") {
                            "server/discover" => json!({
                                "supportedVersions": [crate::mcp_client::MODERN],
                                "capabilities": { "tools": {} },
                            }),
                            "tools/list" => {
                                let counter = calls.fetch_add(1, Ordering::Relaxed);
                                json!({
                                    "tools": [{
                                        "name": "fixture",
                                        "description": "x".repeat(160),
                                        "inputSchema": { "type": "object" },
                                    }],
                                    "nextCursor": counter.to_string(),
                                })
                            }
                            method => panic!("unexpected MCP method {method}"),
                        };
                        let body =
                            json!({ "jsonrpc": "2.0", "id": request["id"], "result": result });
                        if padded.load(Ordering::Relaxed) && request["method"] == "tools/list" {
                            return format!("{}{body}", " ".repeat(512)).into_response();
                        }
                        Json(body).into_response()
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
        let server: McpServer = serde_json::from_value(json!({
            "url": format!("http://{address}/mcp"),
            "allowPrivateNetwork": true,
        }))
        .unwrap();
        let mut client = Client::connect_server(&s, &server).await.unwrap();

        let page_limit = client.discover_catalog(4096, 1).await.unwrap_err();
        assert_eq!(page_limit.status, 413);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let byte_limit = client.discover_catalog(300, 4).await.unwrap_err();
        assert_eq!(byte_limit.status, 413);
        assert_eq!(calls.load(Ordering::Relaxed), 3);

        client.close().await;
        padded.store(true, Ordering::Relaxed);
        let mut bounded = Client::begin_catalog_refresh(
            &s,
            &server,
            Authorization::default(),
            Instant::now() + Duration::from_secs(3),
            400,
            None,
        )
        .await
        .unwrap();
        bounded.negotiate_catalog_refresh().await.unwrap();
        let wire_limit = bounded.discover_catalog(4096, 4).await.unwrap_err();
        assert_eq!(
            wire_limit.message,
            "MCP response exceeds the background byte limit."
        );
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        bounded.close_catalog_refresh().await;
        upstream.abort();
    }

    async fn wait_for_refresh_release(catalogs: &Catalogs, id: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while catalogs.refreshing.lock().unwrap().contains(id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the completed refresh must release its permit");
    }

    #[test]
    fn refresh_is_coalesced_and_expiry_never_serves_an_old_credential() {
        let catalogs = Catalogs::default();
        let identity = Identity {
            revision: 1,
            credentials: Some(blake3::hash(b"fixture credential record")),
        };
        let tools = Arc::new(vec![json!({ "name": "read" })]);
        catalogs.insert("fixture".into(), identity, tools.clone(), None);

        let (cached, refresh) = catalogs.get("fixture", identity).unwrap();
        assert!(Arc::ptr_eq(&cached, &tools));
        assert!(!refresh);

        catalogs
            .entries
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
            .entries
            .lock()
            .unwrap()
            .get_mut("fixture")
            .unwrap()
            .discovered_at -= EXPIRES_AFTER;
        assert!(catalogs.get("fixture", identity).is_none());
    }

    #[test]
    fn oauth_expiry_ends_cache_validity_without_a_credential_write() {
        let catalogs = Catalogs::default();
        let identity = Identity {
            revision: 1,
            credentials: Some(blake3::hash(b"fixture credential record")),
        };
        let tools = Arc::new(vec![json!({ "name": "read" })]);
        catalogs.insert(
            "fixture".into(),
            identity,
            tools,
            Some(crate::config::now()),
        );
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
        let revoked_entered = Arc::new(Notify::new());
        let revoked_release = Arc::new(Notify::new());
        let sessions = Arc::new(AtomicUsize::new(0));
        let router = Router::new().route(
            "/mcp",
            post({
                let (entered, release) = (entered.clone(), release.clone());
                let (revoked_entered, revoked_release) =
                    (revoked_entered.clone(), revoked_release.clone());
                move |incoming: HeaderMap, Json(request): Json<Value>| {
                    let (entered, release, sessions) =
                        (entered.clone(), release.clone(), sessions.clone());
                    let (revoked_entered, revoked_release) =
                        (revoked_entered.clone(), revoked_release.clone());
                    async move {
                        let mut headers = HeaderMap::new();
                        let session_id = incoming.get("mcp-session-id").and_then(|value| value.to_str().ok());
                        let result = match text(&request, "method") {
                            "server/discover" => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
                            "initialize" => {
                                let session = sessions.fetch_add(1, Ordering::Relaxed) + 1;
                                headers.insert("mcp-session-id", session.to_string().parse().unwrap());
                                if session == 5 {
                                    return (headers, " ".repeat(MAX_CATALOG_BYTES + 1)).into_response();
                                }
                                json!({
                                    "protocolVersion": crate::mcp_client::LEGACY,
                                    "capabilities": { "tools": {} },
                                })
                            }
                            "notifications/initialized" => {
                                if matches!(session_id, Some("6" | "9")) {
                                    return StatusCode::FORBIDDEN.into_response();
                                }
                                return StatusCode::ACCEPTED.into_response();
                            }
                            "tools/list" => {
                                if session_id == Some("11") {
                                    revoked_entered.notify_one();
                                    revoked_release.notified().await;
                                    return StatusCode::FORBIDDEN.into_response();
                                }
                                let name = if matches!(session_id, Some("2" | "4")) {
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
                            "tools/call" => {
                                if session_id == Some("8") {
                                    return StatusCode::FORBIDDEN.into_response();
                                }
                                json!({ "content": [{ "type": "text", "text": "live" }] })
                            }
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
                        let session = headers.get("mcp-session-id").and_then(|value| value.to_str().ok());
                        if matches!(session, Some("2" | "4" | "5" | "6" | "8" | "9")) {
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
        std::fs::create_dir(root.path().join("home")).unwrap();
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
            .entries
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
        wait_for_refresh_release(&s.mcps.gateway_catalogs, id).await;

        let access = Access::for_grant(&s, id, bearer).await.unwrap();
        let identity = access.identity(&s).await.unwrap();
        let (previous, _) = s.mcps.gateway_catalogs.get(id, identity).unwrap();
        refresh_in_background(
            s.clone(),
            access,
            identity,
            previous,
            Duration::from_secs(1),
        );
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), close_entered.notified())
            .await
            .expect("the timed-out listing must close the legacy session");
        assert_eq!(
            list(&s, id, bearer).await.unwrap()["tools"][0]["name"],
            "updated_fixture"
        );
        close_release.notify_one();
        release.notify_one();
        wait_for_refresh_release(&s.mcps.gateway_catalogs, id).await;

        let access = Access::for_grant(&s, id, bearer).await.unwrap();
        let identity = access.identity(&s).await.unwrap();
        let (previous, _) = s.mcps.gateway_catalogs.get(id, identity).unwrap();
        refresh_in_background(
            s.clone(),
            access,
            identity,
            previous,
            Duration::from_secs(3),
        );
        tokio::time::timeout(Duration::from_secs(3), close_entered.notified())
            .await
            .expect("an oversize initialize body must still close the session from its headers");
        assert!(s.mcps.gateway_catalogs.get(id, identity).is_some());
        close_release.notify_one();
        wait_for_refresh_release(&s.mcps.gateway_catalogs, id).await;

        let access = Access::for_grant(&s, id, bearer).await.unwrap();
        let (previous, _) = s.mcps.gateway_catalogs.get(id, identity).unwrap();
        refresh_in_background(
            s.clone(),
            access,
            identity,
            previous,
            Duration::from_secs(3),
        );
        tokio::time::timeout(Duration::from_secs(3), close_entered.notified())
            .await
            .expect("the denied initialization must close its legacy session");
        assert!(s.mcps.gateway_catalogs.get(id, identity).is_none());
        close_release.notify_one();
        wait_for_refresh_release(&s.mcps.gateway_catalogs, id).await;

        assert_eq!(
            list(&s, id, bearer).await.unwrap()["tools"][0]["name"],
            "fixture"
        );
        let mut denied_live = std::pin::pin!(super::super::proxy(
            &s,
            id,
            bearer,
            "tools/call",
            json!({ "name": "fixture", "arguments": {} }),
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                () = close_entered.notified() => {}
                result = &mut denied_live => panic!("the denied call closed before the test could observe it: {result:?}"),
            }
        }).await.unwrap();
        assert!(s.mcps.gateway_catalogs.get(id, identity).is_none());
        close_release.notify_one();
        assert_eq!(
            denied_live.await.unwrap_err().message,
            "MCP request failed."
        );
        let rejected = tokio::time::timeout(Duration::from_secs(3), list(&s, id, bearer))
            .await
            .expect("a live initialize rejection must not wait for legacy cleanup");
        assert_eq!(rejected.unwrap_err().message, "MCP initialization failed.");
        assert_eq!(
            list(&s, id, bearer).await.unwrap()["tools"][0]["name"],
            "fixture"
        );

        let access = Access::for_grant(&s, id, bearer).await.unwrap();
        let (previous, _) = s.mcps.gateway_catalogs.get(id, identity).unwrap();
        refresh_in_background(
            s.clone(),
            access,
            identity,
            previous,
            Duration::from_secs(3),
        );
        tokio::time::timeout(Duration::from_secs(3), revoked_entered.notified())
            .await
            .unwrap();
        s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
        revoked_release.notify_one();
        wait_for_refresh_release(&s.mcps.gateway_catalogs, id).await;
        assert!(s.mcps.gateway_catalogs.get(id, identity).is_none());
        upstream.abort();
    }
}
