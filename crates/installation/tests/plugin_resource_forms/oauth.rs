//! Root credential renewal through actual owner resource-form HTTP and Luau.
use super::*;
use axum::{extract::Form, response::IntoResponse};
use cairn_installation::plugins::oauth::credential;
use std::{collections::HashMap, sync::atomic::AtomicUsize};

struct OAuthFixture {
    _root: TempDir,
    service: Arc<Service>,
    app: Router,
    path: String,
    instance: Value,
    provider_origin: String,
    key: String,
    refreshes: Arc<AtomicUsize>,
    interrupt_refresh: Arc<AtomicBool>,
    requests: Arc<tokio::sync::Mutex<Vec<String>>>,
    hold: Arc<AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    owner_changed: Arc<AtomicBool>,
    owner_hold_at: Arc<AtomicUsize>,
    owner_entered: Arc<tokio::sync::Notify>,
    owner_release: Arc<tokio::sync::Notify>,
    servers: [tokio::task::JoinHandle<()>; 2],
}

impl Drop for OAuthFixture {
    fn drop(&mut self) {
        for server in &self.servers {
            server.abort();
        }
    }
}

impl OAuthFixture {
    async fn new() -> Self {
        Self::with_instance_form(false).await
    }

    async fn with_instance_form(instance_form: bool) -> Self {
        let root = TempDir::new().unwrap();
        let service = Service::new(common::config(root.path())).await.unwrap();
        common::relay_fixture::claimed(&service).await.unwrap();
        service
            .plugins
            .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let calls = refreshes.clone();
        let interrupt_refresh = Arc::new(AtomicBool::new(false));
        let interrupt = interrupt_refresh.clone();
        let reads = requests.clone();
        let hold = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (h, e, r) = (hold.clone(), entered.clone(), release.clone());
        let token_endpoint = move |Form(fields): Form<HashMap<String, String>>| {
            let calls = calls.clone();
            let interrupt = interrupt.clone();
            let (hold, entered, release) = (h.clone(), e.clone(), r.clone());
            async move {
                let refresh = fields["grant_type"] == "refresh_token";
                let reconnect = fields
                    .get("code")
                    .is_some_and(|code| code == "reconnect-code");
                if refresh {
                    assert_eq!(fields["refresh_token"], "root-refresh-fixture");
                    calls.fetch_add(1, Ordering::SeqCst);
                    if hold.load(Ordering::SeqCst) {
                        entered.notify_one();
                        release.notified().await;
                    }
                    if interrupt.swap(false, Ordering::SeqCst) {
                        return Response::new(Body::from_stream(futures_util::stream::iter([
                            Ok(b"{\"access_token\":".to_vec()),
                            Err(std::io::Error::other(
                                "Synthetic token response interrupted",
                            )),
                        ])));
                    }
                }
                axum::Json(json!({
                    "access_token": if refresh || reconnect { "root-renewed-fixture" } else { "root-expired-fixture" },
                    "refresh_token": if refresh || reconnect { "root-next-fixture" } else { "root-refresh-fixture" },
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "scope": "workspaces:read",
                })).into_response()
            }
        };
        let workspace_endpoint = move |headers: axum::http::HeaderMap| {
            let reads = reads.clone();
            async move {
                let token = headers
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                let valid = token == "Bearer root-renewed-fixture";
                reads.lock().await.push(token);
                axum::Json(json!({ "ok": valid }))
            }
        };
        let provider = Router::new()
            .route("/token", axum::routing::post(token_endpoint))
            .route("/workspaces", axum::routing::get(workspace_endpoint));
        let (provider_origin, server) = common::serve_locally(provider).await;
        service.plugins.route_http_to(Some(provider_origin.clone()));
        let package_input = json!({
            "manifest": {
                "id": "resourceoauthforms", "name": "Resource OAuth forms",
                "version": "1", "apiVersion": 2,
                "instanceForm": instance_form,
                "capabilities": ["http", "resources"], "hosts": ["api.example.test"],
                "credential": {
                    "label": "Root OAuth", "header": "authorization", "prefix": "Bearer ",
                    "oauth": {
                        "authorizationUrl": "https://api.example.test/authorize",
                        "tokenUrl": "https://api.example.test/token",
                        "clientIdSetting": "clientId",
                        "scopes": ["workspaces:read"], "requireRefreshToken": true,
                    },
                },
                "settings": [{
                    "key": "clientId", "title": "Client", "type": "string", "required": true,
                }],
                "resources": [{
                    "name": "workspace", "title": "Workspaces", "singular": "Workspace", "form": true,
                    "settings": [{
                        "key": "externalId", "title": "Workspace", "type": "select", "dynamic": true,
                    }],
                }],
            },
            "source": r#"return {form = function(event)
                if not event.resource then return nil end
                local response = cairn.http.request({url = 'https://api.example.test/workspaces', auth = true})
                return {options = {externalId = {{value = 'T1', label = response.json.ok and 'Verified' or 'Expired'}}}}
            end}"#,
        });
        let package = registry::install(&service, &package_input).await.unwrap();
        registry::approve(
            &service,
            "resourceoauthforms",
            "1",
            text(&package, "digest"),
        )
        .await
        .unwrap();
        let started = credential::start(
            &service,
            "resourceoauthforms",
            &json!({
                "settings": { "clientId": "root-client-fixture" },
            }),
            "cairn-account:fixture-owner",
        )
        .await
        .unwrap();
        credential::callback(
            &service,
            &json!({ "state": started["id"], "code": "fixture-code" }),
            "cairn-account:fixture-owner",
        )
        .await
        .unwrap();
        let instance = connections::save(
            &service,
            "resourceoauthforms",
            &json!({
                "name": "Paused OAuth app", "enabled": false,
                "settings": { "clientId": "root-client-fixture" }, "credentialFlow": started["id"],
            }),
            None,
        )
        .await
        .unwrap();
        let key = connections::credential_key(text(&instance, "id"));
        let mut saved = service.vault.get(&key).await.unwrap().unwrap();
        // An actual completed OAuth flow with prepared expiry, without a wall-clock wait.
        saved["oauth"]["expiresAt"] = json!(cairn_installation::config::now() - 1);
        service.vault.set(&key, &saved).await.unwrap();
        let app = cairn_installation::http::router(service.clone())
            .await
            .unwrap();
        let path = format!(
            "/api/plugins/resourceoauthforms/connections/{}/resources/workspace/form",
            text(&instance, "id"),
        );
        let owner_changed = Arc::new(AtomicBool::new(false));
        let changed = owner_changed.clone();
        let owner_calls = Arc::new(AtomicUsize::new(0));
        let owner_hold_at = Arc::new(AtomicUsize::new(usize::MAX));
        let owner_entered = Arc::new(tokio::sync::Notify::new());
        let owner_release = Arc::new(tokio::sync::Notify::new());
        let (gate, entering, releasing) = (
            owner_hold_at.clone(),
            owner_entered.clone(),
            owner_release.clone(),
        );
        let owner_authority = move || {
            let changed = changed.clone();
            let (calls, hold_at, entered, release) = (
                owner_calls.clone(),
                gate.clone(),
                entering.clone(),
                releasing.clone(),
            );
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) + 1 == hold_at.load(Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                axum::Json(json!({
                    "owner": {
                        "account_id": "fixture-owner",
                        "access_id": if changed.load(Ordering::SeqCst) { "replacement-access" } else { "fixture-owner" },
                    },
                    "members": [],
                }))
            }
        };
        let authority = Router::new().route(
            "/api/relay/00000000-0000-4000-8000-000000000055/task-authors",
            axum::routing::get(owner_authority),
        );
        let (origin, authority_server) = common::serve_locally(authority).await;
        let identity_path = service
            .config
            .data_dir
            .join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
        Self {
            _root: root,
            service,
            app,
            path,
            instance,
            provider_origin,
            key,
            refreshes,
            interrupt_refresh,
            requests,
            hold,
            entered,
            release,
            owner_changed,
            owner_hold_at,
            owner_entered,
            owner_release,
            servers: [server, authority_server],
        }
    }
}

#[tokio::test]
async fn a_paused_instances_resource_form_renews_its_expired_root_oauth_credential() {
    let f = OAuthFixture::new().await;
    let (first, second) = tokio::join!(
        request(
            &f.app,
            &f.path,
            "POST",
            json!({ "settings": {} }),
            InstallationRole::Owner
        ),
        request(
            &f.app,
            &f.path,
            "POST",
            json!({ "settings": {} }),
            InstallationRole::Owner
        ),
    );
    for response in [first, second] {
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        let result = common::read_json(response).await;
        assert_eq!(result["options"]["externalId"][0]["label"], "Verified");
        assert!(!result.to_string().contains("root-renewed-fixture"));
    }
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        *f.requests.lock().await,
        ["Bearer root-renewed-fixture", "Bearer root-renewed-fixture"]
    );
}

#[tokio::test]
async fn an_uncertain_root_refresh_requires_reconnection_without_replaying_its_token() {
    let f = OAuthFixture::new().await;
    f.interrupt_refresh.store(true, Ordering::SeqCst);
    let first = request(
        &f.app,
        &f.path,
        "POST",
        json!({ "settings": {} }),
        InstallationRole::Owner,
    )
    .await;
    assert!(first.status().is_server_error());
    let second = request(
        &f.app,
        &f.path,
        "POST",
        json!({ "settings": {} }),
        InstallationRole::Owner,
    )
    .await;
    assert_eq!(second.status(), StatusCode::UNAUTHORIZED);
    assert!(
        common::read_text(second)
            .await
            .to_lowercase()
            .contains("reconnect")
    );
    assert_eq!(
        f.refreshes.load(Ordering::SeqCst),
        1,
        "An uncertain refresh was replayed"
    );
    assert!(f.requests.lock().await.is_empty());
    // Reopen the actual native store with fresh in-memory OAuth locks/flows.
    // Keep the fixture servers alive; this is not a physical/process-restart claim.
    let reopened = Service::new(f.service.config.clone()).await.unwrap();
    reopened
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    reopened
        .plugins
        .route_http_to(Some(f.provider_origin.clone()));
    let app = cairn_installation::http::router(reopened.clone())
        .await
        .unwrap();
    let third = request(
        &app,
        &f.path,
        "POST",
        json!({ "settings": {} }),
        InstallationRole::Owner,
    )
    .await;
    assert_eq!(third.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);

    let flow = credential::start(
        &reopened,
        "resourceoauthforms",
        &json!({
            "instance": f.instance["id"], "settings": { "clientId": "root-client-fixture" },
        }),
        "cairn-account:fixture-owner",
    )
    .await
    .unwrap();
    credential::callback(
        &reopened,
        &json!({ "state": flow["id"], "code": "reconnect-code" }),
        "cairn-account:fixture-owner",
    )
    .await
    .unwrap();
    connections::save(
        &reopened,
        "resourceoauthforms",
        &json!({
            "name": "Reconnected app", "enabled": false,
            "settings": { "clientId": "root-client-fixture" }, "credentialFlow": flow["id"],
        }),
        Some(text(&f.instance, "id")),
    )
    .await
    .unwrap();
    let final_response = request(
        &app,
        &f.path,
        "POST",
        json!({ "settings": {} }),
        InstallationRole::Owner,
    )
    .await;
    assert_eq!(
        final_response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(final_response).await
    );
    assert_eq!(
        common::read_json(final_response).await["options"]["externalId"][0]["label"],
        "Verified"
    );
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(*f.requests.lock().await, ["Bearer root-renewed-fixture"]);
}

#[tokio::test]
async fn resource_form_renewal_does_not_publish_credentials_after_the_owners_access_changes() {
    renewal_after_change(Change::Owner).await;
}

#[tokio::test]
async fn resource_form_renewal_rechecks_instance_and_package_identity_before_publication() {
    for change in [Change::Edited, Change::Removed, Change::Approval] {
        renewal_after_change(change).await;
    }
}

#[tokio::test]
async fn a_waiting_form_cannot_adopt_a_replacement_approval_when_renewal_starts() {
    let f = OAuthFixture::new().await;
    let before = f.service.vault.get(&f.key).await.unwrap().unwrap();
    // Hold the authority lookup after the editor captured its Target approval,
    // before the renewal starts. This is an external HTTP gate, not a seeded result.
    f.owner_hold_at.store(3, Ordering::SeqCst);
    let (app, path) = (f.app.clone(), f.path.clone());
    let pending = tokio::spawn(async move {
        request(
            &app,
            &path,
            "POST",
            json!({ "settings": {} }),
            InstallationRole::Owner,
        )
        .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.owner_entered.notified(),
    )
    .await
    .unwrap();
    let old = registry::active(&f.service, "resourceoauthforms")
        .await
        .unwrap();
    let mut manifest = serde_json::to_value(&old.manifest).unwrap();
    manifest["version"] = json!("2");
    let installed = registry::install(
        &f.service,
        &json!({ "manifest": manifest, "source": old.source.main }),
    )
    .await
    .unwrap();
    registry::approve(
        &f.service,
        "resourceoauthforms",
        "2",
        text(&installed, "digest"),
    )
    .await
    .unwrap();
    f.owner_release.notify_one();
    let response = pending.await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "{}",
        common::read_text(response).await
    );
    assert_eq!(
        f.service.vault.get(&f.key).await.unwrap().unwrap(),
        before,
        "The old form adopted a replacement approval to publish a credential"
    );
    assert_eq!(f.refreshes.load(Ordering::SeqCst), 0);
    assert!(f.requests.lock().await.is_empty());
}

#[tokio::test]
async fn removing_or_editing_the_root_during_the_final_authority_lookup_prevents_refresh_io() {
    for remove in [false, true] {
        let f = OAuthFixture::new().await;
        // Consumption already happened; hold the last remote lookup before POST.
        f.owner_hold_at.store(4, Ordering::SeqCst);
        let (app, path) = (f.app.clone(), f.path.clone());
        let pending = tokio::spawn(async move {
            request(
                &app,
                &path,
                "POST",
                json!({ "settings": {} }),
                InstallationRole::Owner,
            )
            .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            f.owner_entered.notified(),
        )
        .await
        .unwrap();
        let response = request(&f.app, &format!(
            "/api/plugins/resourceoauthforms/connections/{}", text(&f.instance, "id"),
        ), if remove { "DELETE" } else { "PUT" },
            json!({ "name": "Changed root", "enabled": false, "settings": { "clientId": "root-client-fixture" } }),
            InstallationRole::Owner).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            common::read_text(response).await
        );
        f.owner_release.notify_one();
        let response = pending.await.unwrap();
        assert!(response.status().is_client_error());
        assert_eq!(
            f.refreshes.load(Ordering::SeqCst),
            0,
            "A retired root's token was sent to the provider"
        );
        assert!(f.requests.lock().await.is_empty());
    }
}

#[tokio::test]
async fn a_stale_root_save_cannot_refresh_the_successor_revisions_credentials() {
    let f = OAuthFixture::with_instance_form(true).await;
    let root_path = format!(
        "/api/plugins/resourceoauthforms/connections/{}",
        text(&f.instance, "id")
    );
    // Saving captured the old root before this task-author policy lookup.
    f.owner_hold_at.store(3, Ordering::SeqCst);
    let (app, path) = (f.app.clone(), root_path.clone());
    let pending = tokio::spawn(async move {
        request(&app, &path, "PUT", json!({
            "name": "Stale save", "enabled": false, "settings": { "clientId": "root-client-fixture" },
        }), InstallationRole::Owner).await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.owner_entered.notified(),
    )
    .await
    .unwrap();
    let flow = credential::start(
        &f.service,
        "resourceoauthforms",
        &json!({
            "instance": f.instance["id"], "settings": { "clientId": "root-client-fixture" },
        }),
        "cairn-account:fixture-owner",
    )
    .await
    .unwrap();
    credential::callback(
        &f.service,
        &json!({ "state": flow["id"], "code": "replacement-code" }),
        "cairn-account:fixture-owner",
    )
    .await
    .unwrap();
    let response = request(&f.app, &root_path, "PUT", json!({
        "name": "Successor", "enabled": false, "settings": { "clientId": "root-client-fixture" },
        "credentialFlow": flow["id"],
    }), InstallationRole::Owner).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::read_text(response).await
    );
    let mut successor = f.service.vault.get(&f.key).await.unwrap().unwrap();
    successor["oauth"]["expiresAt"] = json!(cairn_installation::config::now() - 1);
    f.service.vault.set(&f.key, &successor).await.unwrap();
    f.owner_release.notify_one();
    let response = pending.await.unwrap();
    assert!(response.status().is_client_error());
    assert_eq!(
        f.refreshes.load(Ordering::SeqCst),
        0,
        "The old editor refreshed its successor's OAuth token"
    );
    assert_eq!(
        f.service.vault.get(&f.key).await.unwrap().unwrap(),
        successor
    );
}

enum Change {
    Owner,
    Edited,
    Removed,
    Approval,
}

async fn renewal_after_change(change: Change) {
    let f = OAuthFixture::new().await;
    let before = f.service.vault.get(&f.key).await.unwrap().unwrap();
    f.hold.store(true, Ordering::SeqCst);
    let (app, path) = (f.app.clone(), f.path.clone());
    let pending = tokio::spawn(async move {
        request(
            &app,
            &path,
            "POST",
            json!({ "settings": {} }),
            InstallationRole::Owner,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    let expected = match change {
        Change::Owner => {
            f.owner_changed.store(true, Ordering::SeqCst);
            StatusCode::FORBIDDEN
        }
        Change::Edited | Change::Removed => {
            let response = request(&f.app, &format!(
                "/api/plugins/resourceoauthforms/connections/{}", text(&f.instance, "id"),
            ), if matches!(change, Change::Removed) { "DELETE" } else { "PUT" },
                json!({ "name": "Edited app", "enabled": false, "settings": { "clientId": "root-client-fixture" } }),
                InstallationRole::Owner).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{}",
                common::read_text(response).await
            );
            if matches!(change, Change::Removed) {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::FORBIDDEN
            }
        }
        Change::Approval => {
            let old = registry::active(&f.service, "resourceoauthforms")
                .await
                .unwrap();
            let mut manifest = serde_json::to_value(&old.manifest).unwrap();
            manifest["version"] = json!("2");
            let installed = registry::install(
                &f.service,
                &json!({
                    "manifest": manifest, "source": old.source.main,
                }),
            )
            .await
            .unwrap();
            registry::approve(
                &f.service,
                "resourceoauthforms",
                "2",
                text(&installed, "digest"),
            )
            .await
            .unwrap();
            StatusCode::CONFLICT
        }
    };
    f.release.notify_one();
    let response = pending.await.unwrap();
    assert_eq!(
        response.status(),
        expected,
        "{}",
        common::read_text(response).await
    );
    assert!(
        f.requests.lock().await.is_empty(),
        "A stale form reached the workspace provider"
    );
    let current = f.service.vault.get(&f.key).await.unwrap();
    if matches!(change, Change::Removed) {
        assert!(current.is_none(), "Renewal recreated a removed credential");
    } else {
        let current = current.unwrap();
        assert_eq!(
            current["token"], before["token"],
            "A stale form published the rotated access token"
        );
        assert_eq!(current["oauth"]["expiresAt"], before["oauth"]["expiresAt"]);
        assert_eq!(current["oauth"]["refreshing"], true);
        assert!(
            current["oauth"].get("refreshToken").is_none(),
            "A consumed token remained replayable"
        );
    }
}

#[tokio::test]
#[ignore = "local root OAuth encrypted-record comparison; run alone"]
async fn measure_root_credential_storage() {
    use cairn_installation::store::Store;
    let f = OAuthFixture::new().await;
    let old = f.service.vault.get(&f.key).await.unwrap().unwrap();
    f.hold.store(true, Ordering::SeqCst);
    let (app, path) = (f.app.clone(), f.path.clone());
    let pending = tokio::spawn(async move {
        request(
            &app,
            &path,
            "POST",
            json!({ "settings": {} }),
            InstallationRole::Owner,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    let consuming = f.service.vault.get(&f.key).await.unwrap().unwrap();
    assert_eq!(consuming["oauth"]["refreshing"], true);
    assert!(consuming["oauth"].get("refreshToken").is_none());
    f.release.notify_one();
    let response = pending.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let refreshed = f.service.vault.get(&f.key).await.unwrap().unwrap();
    let mut cases = Vec::new();
    for (name, value) in [
        ("expired reference", old),
        ("consumed intent", consuming),
        ("renewed candidate", refreshed),
    ] {
        let mut writes = Vec::new();
        for sample in 0..6 {
            let cpu = cpu_ms();
            let at = std::time::Instant::now();
            f.service.vault.set(&f.key, &value).await.unwrap();
            writes.push(json!({
                "sample": sample, "wallMs": at.elapsed().as_secs_f64() * 1000.0,
                "processCpuMs": cpu_ms() - cpu,
            }));
        }
        let key = format!("mcp-secret:{}", f.key);
        let encrypted = f.service.store.kv(&key).await.unwrap().unwrap();
        let bytes = serde_json::to_vec(&encrypted).unwrap().len();
        let reader = Store::open(&f.service.config.data_dir).unwrap();
        let reads = reader
            .read(move |db| {
                let mut reads = Vec::new();
                for sample in 0..6 {
                    let cpu = cpu_ms();
                    let at = std::time::Instant::now();
                    let value = db.kv(&key)?.unwrap();
                    reads.push(json!({
                        "sample": sample, "wallMs": at.elapsed().as_secs_f64() * 1000.0,
                        "processCpuMs": cpu_ms() - cpu,
                        "returnedEncryptedJsonBytes": serde_json::to_vec(&value)?.len(),
                    }));
                }
                Ok(reads)
            })
            .await
            .unwrap();
        cases.push(json!({
            "name": name, "credentialJsonBytes": serde_json::to_vec(&value).unwrap().len(),
            "encryptedValueJsonBytes": bytes, "reads": reads, "writes": writes,
        }));
    }
    eprintln!(
        "ROOT_FORM_CREDENTIAL_STORAGE {}",
        json!({
            "cases": cases,
            "workload": "same actual root OAuth record before/during/after one fixture-backed form renewal; six native vault writes and six encrypted KV reads per case",
            "cache": "one reopened reader per case; first connection-cold read plus five warm reads, OS caches retained",
            "limits": "JSON bytes exclude database/WAL/index pages; writes include encryption and actor scheduling; reads bypass authorization and decryption; OAuth HTTP, Luau, fixture setup and schema excluded; no production latency claim",
            "host": {
                "parallelism": std::thread::available_parallelism().ok().map(std::num::NonZero::get),
                "cpuMax": std::fs::read_to_string("/sys/fs/cgroup/cpu.max").ok(),
                "memoryMax": std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok(),
            },
        })
    );
}

fn cpu_ms() -> f64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut value) },
        0
    );
    value.tv_sec as f64 * 1000.0 + value.tv_nsec as f64 / 1_000_000.0
}
