mod common;

use axum::http::StatusCode;
use cairn_installation::{
    auth::{InstallationIdentity, InstallationRole},
    plugins::registry,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};
use tempfile::TempDir;

fn request(path: &str, role: InstallationRole) -> axum::extract::Request {
    let mut request = common::request("GET", path)
        .body(axum::body::Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(InstallationIdentity::trusted(role, "fixture-owner"));
    request
}

async fn fixture() -> (TempDir, Arc<Service>, axum::Router) {
    let directory = TempDir::new().unwrap();
    let service = Service::new(common::config(directory.path()))
        .await
        .unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    (directory, service, app)
}

fn copied_version(service: &Service, id: &str, version: &str) -> PathBuf {
    let directory = service
        .config
        .data_dir
        .join("plugins")
        .join(id)
        .join(version);
    std::fs::create_dir_all(directory.join("lib")).unwrap();
    // Owner-copied TOML is a documented installation path. Its valid 60 KB of
    // literal defaults expands to 120 KB of JSON before any versions accumulate.
    let mut manifest = format!(
        "id = '{id}'\nname = 'Package {id}'\nversion = '{version}'\napiVersion = 2\n\
         description = 'A copied owner package'\ncapabilities = []\n\
         [instances]\nsingular = 'Workspace'\nplural = 'Workspaces'\n\
         [brand]\ncolor = '#123456'\npath = 'M1 1L2 2'\n"
    );
    for n in 0..6 {
        manifest.push_str(&format!(
            "\n[[settings]]\nkey = 'field{n}'\ntitle = 'Field {n}'\ntype = 'text'\n\
             maxLength = 10000\ndefault = '{}'\n",
            "\"".repeat(10000)
        ));
    }
    assert!(manifest.len() <= 65_536);
    std::fs::write(directory.join("manifest.toml"), manifest).unwrap();
    std::fs::write(directory.join("main.luau"), "return require('./lib/value')").unwrap();
    std::fs::write(directory.join("lib/value.luau"), "return {}").unwrap();
    directory
}

async fn get(app: &axum::Router, path: &str) -> Value {
    let response = common::send(app, request(path, InstallationRole::Owner)).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), cairn_protocol::MAX_BODY / 2)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

async fn pages(app: &axum::Router, path: &str, key: &str) -> Vec<Value> {
    let mut cursor: Option<String> = None;
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    loop {
        let path = cursor.as_ref().map_or_else(
            || path.to_owned(),
            |cursor| format!("{path}?cursor={cursor}"),
        );
        let response = common::send(app, request(&path, InstallationRole::Owner)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(bytes.len() <= cairn_protocol::MAX_BODY / 2);
        let page: Value = serde_json::from_slice(&bytes).unwrap();
        let items = page["items"].as_array().unwrap();
        assert!(items.len() <= 32);
        for item in items {
            let id = text(item, key);
            assert!(seen.insert(id.to_owned()), "Repeated {key}: {id}");
            assert!(
                values
                    .last()
                    .is_none_or(|previous| text(previous, key) < id)
            );
            values.push(item.clone());
        }
        let next = page["nextCursor"].as_str().map(str::to_owned);
        if next.is_none() {
            break;
        }
        assert_eq!(next.as_deref(), items.last().map(|item| text(item, key)));
        assert_ne!(cursor, next);
        cursor = next;
    }
    values
}

#[tokio::test]
async fn instance_reads_are_bounded_exact_and_keep_unapproved_configuration_manageable() {
    use cairn_installation::{
        config::{id, now},
        plugins::connections::Connection,
    };
    let (_directory, service, app) = fixture().await;
    let plugin = "instance-reads";
    let directory = copied_version(&service, plugin, "1");
    let mut manifest = format!(
        "id='{plugin}'\nname='Instance reads'\nversion='1'\napiVersion=2\ncapabilities=[]\n"
    );
    for n in 0..8 {
        manifest.push_str(&format!(
            "\n[[settings]]\nkey='field{n}'\ntitle='Field {n}'\ntype='text'\nmaxLength=10000\n"
        ));
    }
    std::fs::write(directory.join("manifest.toml"), manifest).unwrap();
    let agent = id();
    let resource_agent = id();
    let mut instances = Vec::new();
    for n in 0..40 {
        let settings = json!(
            (0..8)
                .map(|n| (format!("field{n}"), json!("\"".repeat(10000))))
                .collect::<serde_json::Map<_, _>>()
        );
        let mut instance =
            Connection::draft(&id(), plugin, settings, vec!["credential".to_owned()]);
        instance.name = format!("Instance {n}");
        instance.revision = id();
        instance.enabled = n != 0;
        instance.agent_ids = vec![agent.clone()];
        instances.push(serde_json::to_value(instance).unwrap());
    }
    instances.sort_by(|a, b| text(a, "id").cmp(text(b, "id")));
    let target = text(&instances[0], "id").to_owned();
    let other = id();
    let seed = instances.clone();
    let (saved_target, saved_other, saved_agent, saved_resource_agent) = (
        target.clone(),
        other.clone(),
        agent.clone(),
        resource_agent.clone(),
    );
    let hour = now() / 3_600_000;
    service
        .store
        .transaction(move |db| {
            for instance in seed {
                db.put("plugin-connections", &instance)?;
            }
            for n in 0..3 {
                let mut worker = Connection::draft(&id(), "discord", json!({}), Vec::new());
                worker.enabled = n != 2;
                db.put("plugin-connections", &serde_json::to_value(&worker)?)?;
                db.set(
                    &format!("plugin-status:{}", worker.id),
                    &json!({ "state": "connected" }),
                    (n == 1).then(|| now() - 1),
                )?;
            }
            db.put(
                "plugin-connections",
                &json!({
                    "id": saved_other,
                    "pluginId": "other",
                    "settings": { "huge": "not selected" },
                }),
            )?;
            db.set(
                &format!("plugin-rate:{saved_target}:{hour}"),
                &json!(7),
                None,
            )?;
            db.set(
                &format!("plugin-status:{saved_target}"),
                &json!({ "state": "connected" }),
                None,
            )?;

            // A route request is still one instance request, not counted twice.
            for n in 0..2 {
                db.put(
                    "plugin-requests",
                    &json!({
                        "id": id(),
                        "connectionId": saved_target,
                        "routeId": if n == 0 { Value::Null } else { json!(id()) },
                        "status": "pending",
                        "updatedAt": n,
                    }),
                )?;
            }
            db.put(
                "plugin-requests",
                &json!({
                    "id": id(), "connectionId": saved_other, "status": "pending",
                }),
            )?;
            db.put(
                "plugin-requests",
                &json!({
                    "id": id(), "connectionId": saved_target, "status": "approved",
                }),
            )?;
            for n in 0..12 {
                db.put(
                    "plugin-activity",
                    &json!({
                        "id": id(), "connectionId": saved_target, "createdAt": n,
                    }),
                )?;
            }
            for agent in [
                saved_agent.clone(),
                saved_resource_agent.clone(),
                saved_resource_agent,
            ] {
                db.put(
                    "plugin-resource-grants",
                    &json!({
                        "id": id(), "connectionId": saved_target, "agentId": agent,
                    }),
                )?;
            }
            db.put(
                "plugin-resource-grants",
                &json!({
                    "id": id(), "connectionId": saved_other, "agentId": id(),
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let root = format!("/api/plugins/packages/{plugin}/connections");
    let first = get(&app, &root).await;
    assert_eq!(first["instanceReads"], true);
    assert!(
        first["items"].as_array().unwrap().len() < 32,
        "Byte bound must select fewer than the count limit"
    );
    let listed = pages(&app, &root, "id").await;
    assert_eq!(listed.len(), 40);
    let exact = get(&app, &format!("{root}/{target}")).await;
    assert_eq!(exact["agentIds"], json!([agent]));
    assert_eq!(exact["secretKeys"], json!(["credential"]));
    assert!(exact["credential"].is_null());
    assert_eq!(exact["usage"]["hour"], 7);
    assert_eq!(exact["requests"].as_array().unwrap().len(), 2);
    let activity = exact["activity"].as_array().unwrap();
    assert_eq!(activity.len(), 8);
    assert_eq!(activity[0]["createdAt"], 11);
    assert_eq!(activity[7]["createdAt"], 4);
    let cards = pages(&app, "/api/plugins/packages", "id").await;
    let summary = &cards.iter().find(|card| card["id"] == plugin).unwrap()["summary"];
    assert_eq!(summary["instances"], 40);
    assert_eq!(summary["states"]["ready"], 39);
    assert_eq!(summary["states"]["paused"], 1);
    assert_eq!(summary["pendingRequests"], 2);
    assert_eq!(summary["authorizedAgents"], 2);
    assert!(summary["observedAt"].as_i64().unwrap() > 0);
    let worker_summary = &cards.iter().find(|card| card["id"] == "discord").unwrap()["summary"];
    assert_eq!(worker_summary["instances"], 3);
    assert_eq!(worker_summary["states"]["connected"], 1);
    assert_eq!(worker_summary["states"]["disconnected"], 1);
    assert_eq!(worker_summary["states"]["paused"], 1);
    let access = pages(&app, "/api/plugins/instance-access", "id").await;
    assert_eq!(access.len(), 44);
    let saved = access.iter().find(|row| row["id"] == target).unwrap();
    assert_eq!(saved["agentIds"], json!([agent]));
    let resource_ids = saved["resourceAgentIds"].as_array().unwrap();
    assert_eq!(resource_ids.len(), 2);
    assert!(resource_ids.contains(&json!(resource_agent)));
    assert_eq!(saved["enabled"], exact["enabled"]);
    assert!(
        access
            .iter()
            .any(|row| row["pluginId"] == plugin && row["enabled"] == false)
    );
    assert_eq!(saved["pluginName"], plugin);
    assert!(access.iter().any(|row| row["pluginName"] == "Discord"));
    for row in &access {
        for key in ["settings", "secretKeys", "usage", "requests", "activity"] {
            assert!(row.get(key).is_none(), "Access metadata must omit {key}");
        }
    }
    let detail = get(&app, &format!("/api/plugins/packages/{plugin}")).await;
    assert_eq!(detail["accessReads"], true);
    assert_eq!(detail["instanceReads"], true);
    assert!(detail["capabilities"].is_array());
    // Losing approval or even all source files never hides saved configuration.
    std::fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    assert_eq!(
        get(&app, &format!("{root}/{target}")).await["agentIds"],
        exact["agentIds"]
    );
    assert_eq!(pages(&app, &root, "id").await.len(), 40);
    assert_eq!(
        pages(&app, "/api/plugins/instance-access", "id").await,
        access
    );
    for path in [
        "/api/plugins/instance-access".to_owned(),
        root.clone(),
        format!("{root}/{target}"),
    ] {
        assert_eq!(
            common::send(&app, request(&path, InstallationRole::Member))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    for path in [
        "/api/plugins/instance-access?cursor=invalid".to_owned(),
        format!("{root}?cursor=invalid"),
        format!("{root}/invalid"),
    ] {
        assert_eq!(
            common::send(&app, request(&path, InstallationRole::Owner))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        common::send(
            &app,
            request(&format!("{root}/{other}"), InstallationRole::Owner)
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn compact_package_pages_keep_exact_version_review_separate() {
    let (_directory, service, app) = fixture().await;
    assert_eq!(
        get(&app, "/api/plugins/packages").await["accessReads"],
        true
    );
    let bundled = pages(&app, "/api/plugins/packages", "id").await.len();
    for n in 0..40 {
        copied_version(&service, &format!("catalogue-{n:02}"), "1");
    }
    for n in 2..=40 {
        copied_version(&service, "catalogue-00", &format!("{n:02}"));
    }
    let invalid = copied_version(&service, "catalogue-00", "broken");
    std::fs::remove_file(invalid.join("manifest.toml")).unwrap();
    std::fs::write(invalid.join("manifest.json"), "{").unwrap();
    let review_path = "/api/plugins/packages/catalogue-00/versions/1";
    let review = get(&app, review_path).await;
    assert_eq!(review["approved"], false);
    assert_eq!(
        review["manifest"]["settings"][0]["default"],
        "\"".repeat(10000)
    );
    assert_eq!(review["files"].as_array().unwrap().len(), 2);
    registry::approve(&service, "catalogue-00", "1", text(&review, "digest"))
        .await
        .unwrap();

    let cards = pages(&app, "/api/plugins/packages", "id").await;
    assert_eq!(cards.len(), 40 + bundled);
    let card = cards
        .iter()
        .find(|item| item["id"] == "catalogue-00")
        .unwrap();
    assert_eq!(card["status"], "update");
    assert_eq!(card["active"], "1");
    assert_eq!(card["versionCount"], 41);
    assert_eq!(card["presentation"]["version"], "1");
    assert_eq!(card["presentation"]["instances"]["plural"], "Workspaces");
    assert!(card["manifest"].is_null() && card["versions"].is_null());
    assert!(
        cards
            .iter()
            .all(|item| item["presentation"]["settings"].is_null())
    );
    let first = get(&app, "/api/plugins/packages").await;
    assert_eq!(first["packageReads"], true);
    assert_eq!(first["setupDrafts"], true);
    assert!(!first["capabilities"].as_array().unwrap().is_empty());
    assert!(first["connections"].is_null() && first["resourceGrantCounts"].is_null());

    let detail = get(&app, "/api/plugins/packages/catalogue-00").await;
    assert_eq!(detail["manifest"], review["manifest"]);
    assert_eq!(detail["versionCount"], 41);
    assert!(detail["versions"].is_null());
    let versions = pages(
        &app,
        "/api/plugins/packages/catalogue-00/versions",
        "version",
    )
    .await;
    assert_eq!(versions.len(), 41);
    assert!(
        versions
            .iter()
            .all(|version| version["manifest"].is_null() && version["files"].is_null())
    );
    let selected = versions.iter().find(|item| item["version"] == "1").unwrap();
    assert_eq!(selected["digest"], review["digest"]);
    assert_eq!(selected["approved"], true);
    assert_eq!(selected["fileCount"], 2);
    let broken = versions
        .iter()
        .find(|item| item["version"] == "broken")
        .unwrap();
    assert!(broken["error"].as_str().unwrap().contains("JSON"));
    assert_eq!(
        get(&app, "/api/plugins/packages/catalogue-00/versions/broken").await,
        *broken
    );
    assert_eq!(get(&app, review_path).await["approved"], true);

    // Legacy callers still receive complete manifests and review history.
    let response = common::send(&app, request("/api/plugins", InstallationRole::Owner)).await;
    assert_eq!(response.status(), StatusCode::OK);
    // Measure the oversized reference intentionally; all new list reads above
    // enforce the actual relay budget rather than this fixture's 1 MB helper.
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let legacy: Value = serde_json::from_slice(&bytes).unwrap();
    let old = legacy["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "catalogue-00")
        .unwrap();
    for key in ["status", "active", "manifest"] {
        assert_eq!(old[key], detail[key]);
    }
    assert_eq!(old["versions"].as_array().unwrap().len(), 41);
    let old_bytes = legacy.to_string().len();
    let compact_bytes = first.to_string().len();
    assert!(old_bytes > cairn_protocol::MAX_BODY);
    assert!(compact_bytes < cairn_protocol::MAX_BODY / 2);
    eprintln!(
        "Catalogue fixture: complete response {old_bytes} bytes, compact first page {compact_bytes} bytes."
    );
}

#[tokio::test]
async fn exact_review_tracks_changed_bytes_and_remains_owner_only() {
    let (_directory, service, app) = fixture().await;
    let directory = copied_version(&service, "catalogue", "1");
    let root = "/api/plugins/packages/catalogue";
    let review = get(&app, &format!("{root}/versions/1")).await;
    registry::approve(&service, "catalogue", "1", text(&review, "digest"))
        .await
        .unwrap();
    assert_eq!(get(&app, root).await["status"], "active");
    std::fs::write(
        directory.join("lib/value.luau"),
        "return { changed = true }",
    )
    .unwrap();
    let changed = get(&app, &format!("{root}/versions/1")).await;
    assert_ne!(changed["digest"], review["digest"]);
    assert_eq!(changed["approved"], false);
    assert_eq!(get(&app, root).await["status"], "needs_approval");
    assert!(
        registry::approve(&service, "catalogue", "1", text(&review, "digest"))
            .await
            .is_err()
    );
    assert!(registry::active(&service, "catalogue").await.is_err());
    registry::approve(&service, "catalogue", "1", text(&changed, "digest"))
        .await
        .unwrap();
    assert_eq!(get(&app, root).await["status"], "active");

    for path in [
        "/api/plugins/packages",
        root,
        &format!("{root}/versions"),
        &format!("{root}/versions/1"),
    ] {
        assert_eq!(
            common::send(&app, request(path, InstallationRole::Member))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    for path in [
        "/api/plugins/packages?cursor=..",
        "/api/plugins/packages?cursor=a%2Fb",
        "/api/plugins/packages/catalogue/versions?cursor=..",
    ] {
        assert_eq!(
            common::send(&app, request(path, InstallationRole::Owner))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    for path in [
        "/api/plugins/packages/absent",
        "/api/plugins/packages/catalogue/versions/absent",
        "/api/plugins/packages/discord/versions/absent",
    ] {
        assert_eq!(
            common::send(&app, request(path, InstallationRole::Owner))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let bundled = get(&app, "/api/plugins/packages/discord").await;
    let bundled_review = get(
        &app,
        &format!(
            "/api/plugins/packages/discord/versions/{}",
            text(&bundled, "active")
        ),
    )
    .await;
    assert_eq!(bundled_review["approved"], true);
    assert_eq!(bundled_review["manifest"], bundled["manifest"]);
}

#[tokio::test]
async fn broken_and_empty_packages_do_not_hide_other_cards_or_invent_an_approval() {
    let (_directory, service, app) = fixture().await;
    let invalid = copied_version(&service, "invalid-package", "v1+build");
    std::fs::write(invalid.join("lib/value.luau"), [0xff, 0xfe]).unwrap();
    let empty = service.config.data_dir.join("plugins/empty-package");
    std::fs::create_dir_all(&empty).unwrap();
    let cards = pages(&app, "/api/plugins/packages", "id").await;
    for id in ["invalid-package", "empty-package"] {
        let card = cards.iter().find(|card| card["id"] == id).unwrap();
        assert_eq!(card["status"], "invalid");
        assert!(card["active"].is_null() && card["presentation"].is_null());
    }
    // Browser components encode package/version components independently. Decode
    // those components once, while refusing encoded traversal or separators.
    std::fs::create_dir_all(service.config.data_dir.join("plugins/Z+copied")).unwrap();
    assert_eq!(
        get(&app, "/api/plugins/packages/Z%2Bcopied").await["id"],
        "Z+copied"
    );
    let encoded_version = get(
        &app,
        "/api/plugins/packages/invalid-package/versions/v1%2Bbuild",
    )
    .await;
    assert!(encoded_version["error"].as_str().unwrap().contains("UTF-8"));
    for path in [
        "/api/plugins/packages/%2e%2e",
        "/api/plugins/packages/Z%2fcopied",
        "/api/plugins/packages/Z%252Bcopied",
        "/api/plugins/packages/%FF",
        "/api/plugins/packages/invalid-package/versions/%2e%2e",
        "/api/plugins/packages/invalid-package/versions/v1%2fbuild",
    ] {
        assert_eq!(
            common::send(&app, request(path, InstallationRole::Owner))
                .await
                .status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let versions = get(&app, "/api/plugins/packages/invalid-package/versions").await;
    assert_eq!(versions["items"].as_array().unwrap().len(), 1);
    assert_eq!(versions["items"][0]["version"], "v1+build");
    assert!(
        versions["items"][0]["error"]
            .as_str()
            .unwrap()
            .contains("UTF-8")
    );
    let after = get(
        &app,
        "/api/plugins/packages/invalid-package/versions?cursor=v1%2Bbuild",
    )
    .await;
    assert_eq!(after["items"], json!([]));
    assert!(after["nextCursor"].is_null());
    assert_eq!(
        get(&app, "/api/plugins/packages/empty-package/versions").await["items"],
        json!([])
    );
    assert!(registry::active(&service, "invalid-package").await.is_err());
}

#[cfg(target_os = "linux")]
fn process_cpu_ms() -> f64 {
    let mut clock = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut clock) },
        0
    );
    clock.tv_sec as f64 * 1000.0 + clock.tv_nsec as f64 / 1_000_000.0
}

/// Synthetic observation, run alone. The reference is the unchanged complete
/// response; the candidate is a compact first page. No timing assertion.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "local read/write measurement; run alone with --ignored --nocapture"]
async fn measure_catalogue_reads_and_unchanged_approval_writes() {
    use cairn_installation::{config::id, plugins::connections::Connection};
    let (_directory, service, app) = fixture().await;
    for n in 0..40 {
        copied_version(&service, &format!("catalogue-{n:02}"), "1");
    }
    for n in 2..=40 {
        copied_version(&service, "catalogue-00", &format!("{n:02}"));
    }
    let mut instance_ids = Vec::new();
    for n in 0..40 {
        let settings = json!(
            (0..6)
                .map(|n| (format!("field{n}"), json!("\"".repeat(10000))))
                .collect::<serde_json::Map<_, _>>()
        );
        let mut instance = Connection::draft(&id(), "catalogue-00", settings, Vec::new());
        instance.name = format!("Instance {n}");
        instance.revision = id();
        instance_ids.push(instance.id.clone());
        let value = serde_json::to_value(instance).unwrap();
        service
            .store
            .transaction(move |db| db.put("plugin-connections", &value))
            .await
            .unwrap();
    }
    let exact_path = format!(
        "/api/plugins/packages/catalogue-00/connections/{}",
        instance_ids[0]
    );
    let mut reads = Vec::new();
    for sample in 0..6 {
        // Alternate warm order. First reads are API-cold, not OS-cache cold.
        let mut paths = [
            "/api/plugins",
            "/api/plugins/packages",
            "/api/plugins/packages/catalogue-00/connections",
            "/api/plugins/instance-access",
            exact_path.as_str(),
        ];
        if sample % 2 == 1 {
            paths.reverse();
        }
        for path in paths {
            let cpu = process_cpu_ms();
            let started = std::time::Instant::now();
            let response = common::send(&app, request(path, InstallationRole::Owner)).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            reads.push(json!({
                "path": path,
                "sample": sample,
                "encodedBytes": bytes.len(),
                "wallMs": started.elapsed().as_secs_f64() * 1000.0,
                "processCpuMs": process_cpu_ms() - cpu,
            }));
        }
    }
    let review = get(&app, "/api/plugins/packages/catalogue-00/versions/1").await;
    let mut writes = Vec::new();
    for _ in 0..6 {
        let cpu = process_cpu_ms();
        let started = std::time::Instant::now();
        registry::approve(&service, "catalogue-00", "1", text(&review, "digest"))
            .await
            .unwrap();
        writes.push(json!({
            "wallMs": started.elapsed().as_secs_f64() * 1000.0,
            "processCpuMs": process_cpu_ms() - cpu,
        }));
    }
    eprintln!(
        "CATALOGUE_MEASUREMENT {}",
        json!({
            "packagesCopied": 40,
            "versionsCopied": 79,
            "instancesCopied": 40,
            "reference": "complete owner response, same binary and unchanged record format",
            "candidate": "first compact card page, first selected-package instance page, compact saved access and one exact instance; different response purposes",
            "cache": "first API read then five warm samples, OS caches retained",
            "writes": "existing approval path, identical for both read contracts",
            "reads": reads,
            "approvalWrites": writes,
        })
    );
}

#[tokio::test]
async fn agent_selector_reads_keep_complete_saved_metadata_without_configuration() {
    use cairn_installation::{
        config::{id, now},
        plugins::connections::Connection,
    };
    let (_directory, service, app) = fixture().await;
    let mut instance = Connection::draft(
        &id(),
        "missing-package",
        json!({ "private": "settings are not selector metadata" }),
        vec!["token".to_owned()],
    );
    instance.name = "Unavailable workspace".to_owned();
    instance.enabled = false;
    instance.agent_id = id();
    let root_agent = id();
    instance.agent_ids = vec![root_agent.clone()];
    let saved = serde_json::to_value(&instance).unwrap();
    let mut agents = (0..40).map(|_| id()).collect::<Vec<_>>();
    agents.sort();
    let expected = agents.clone();
    let instance_id = instance.id.clone();
    service
        .store
        .transaction(move |db| {
            db.put("plugin-connections", &saved)?;
            db.set(
                &format!("plugin-status:{instance_id}"),
                &json!({ "state": "needs_review", "message": "Access changed" }),
                None,
            )?;
            for (n, agent) in agents.into_iter().enumerate() {
                for _ in 0..=n % 3 {
                    db.put("plugin-resource-grants", &json!({
                    "id": id(), "connectionId": instance_id, "resourceId": id(),
                    "agentId": agent, "profile": null, "operations": ["read"],
                    "deniedOperations": [], "scopes": {}, "ownerAccessId": "previous-owner-access",
                    "revision": id(), "updatedAt": now(),
                }))?;
                }
            }
            // A retained rule with missing instance metadata still contributes to the saved total.
            db.put(
                "plugin-resource-grants",
                &json!({
                    "id": id(), "connectionId": id(), "resourceId": id(), "agentId": expected[0],
                    "profile": null, "operations": ["read"], "deniedOperations": [], "scopes": {},
                    "ownerAccessId": "previous-owner-access", "revision": id(), "updatedAt": now(),
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let cards = get(&app, "/api/plugins/packages").await;
    assert_eq!(cards["agentReads"], true);
    let onepassword = cards["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "onepassword")
        .unwrap();
    assert!(
        onepassword["presentation"]["agentAccessDescription"]
            .as_str()
            .unwrap()
            .contains("agent")
    );
    assert!(onepassword["presentation"]["contributions"]["operations"].is_number());
    let rows = pages(&app, "/api/plugins/instance-access", "id").await;
    let row = rows.iter().find(|v| v["id"] == instance.id).unwrap();
    assert_eq!(row["agentId"], instance.agent_id);
    assert_eq!(row["agentIds"], json!([root_agent]));
    assert_eq!(
        row["status"],
        json!({ "state": "needs_review", "message": "Access changed" })
    );
    for key in ["settings", "secretKeys", "usage", "requests", "activity"] {
        assert!(row.get(key).is_none(), "Selectors must not receive {key}");
    }
    let counts = pages(&app, "/api/plugins/agent-grant-counts", "id").await;
    assert_eq!(counts.len(), 40);
    for (n, count) in counts.iter().enumerate() {
        assert_eq!(count["count"], (1 + n % 3 + usize::from(n == 0)) as u64);
    }
    for path in [
        "/api/plugins/agent-grant-counts",
        "/api/plugins/instance-access",
    ] {
        assert_eq!(
            common::send(&app, request(path, InstallationRole::Member))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let bad = common::send(
        &app,
        request(
            "/api/plugins/agent-grant-counts?cursor=invalid",
            InstallationRole::Owner,
        ),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
}

/// Compare complete owner selector workloads, including every compact page and
/// JSON decoding. API-cold is not OS-cache cold; no timing assertions.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "local complete-selector read/write measurement; run alone"]
async fn measure_complete_agent_selector_reads() {
    use cairn_installation::{
        config::{id, now},
        plugins::connections::Connection,
    };
    let (_directory, service, app) = fixture().await;
    for n in 0..40 {
        copied_version(&service, &format!("selector-{n:02}"), "1");
    }
    for n in 2..=40 {
        copied_version(&service, "selector-00", &format!("{n:02}"));
    }
    let agents = (0..40).map(|_| id()).collect::<Vec<_>>();
    let mut instances = Vec::new();
    for n in 0..40 {
        let settings = json!(
            (0..6)
                .map(|n| (format!("field{n}"), json!("\"".repeat(10000))))
                .collect::<serde_json::Map<_, _>>()
        );
        let mut instance = Connection::draft(&id(), "selector-00", settings, Vec::new());
        instance.name = format!("Instance {n}");
        instance.agent_ids = agents.clone();
        instances.push(serde_json::to_value(instance).unwrap());
    }
    service.store.transaction(move |db| {
        for instance in &instances { db.put("plugin-connections", instance)?; }
        for n in 0..2000 {
            db.put("plugin-resource-grants", &json!({
                "id": id(), "connectionId": instances[n % 40]["id"], "resourceId": id(),
                "agentId": agents[n % 40], "profile": null, "operations": ["read"],
                "deniedOperations": [], "scopes": {}, "ownerAccessId": "held-access", "revision": id(), "updatedAt": now(),
            }))?;
        }
        Ok(())
    }).await.unwrap();
    let mut samples = Vec::new();
    for sample in 0..6 {
        let mut workloads = ["complete-catalogue", "complete-selector-directory"];
        if sample % 2 == 1 {
            workloads.reverse();
        }
        for workload in workloads {
            let cpu = process_cpu_ms();
            let started = std::time::Instant::now();
            let mut bytes = 0;
            let mut requests = 0;
            let paths: &[&str] = if workload == "complete-catalogue" {
                &["/api/plugins"]
            } else {
                &[
                    "/api/plugins/packages",
                    "/api/plugins/instance-access",
                    "/api/plugins/agent-grant-counts",
                ]
            };
            for path in paths {
                let mut cursor = None::<String>;
                let mut total = 0;
                loop {
                    let target = cursor.as_ref().map_or_else(
                        || path.to_string(),
                        |cursor| format!("{path}?cursor={cursor}"),
                    );
                    let response =
                        common::send(&app, request(&target, InstallationRole::Owner)).await;
                    assert_eq!(response.status(), StatusCode::OK);
                    let encoded = axum::body::to_bytes(response.into_body(), usize::MAX)
                        .await
                        .unwrap();
                    bytes += encoded.len();
                    requests += 1;
                    let value: Value = serde_json::from_slice(&encoded).unwrap();
                    if workload == "complete-catalogue" {
                        assert_eq!(value["connections"].as_array().unwrap().len(), 40);
                        assert_eq!(value["resourceGrantCounts"].as_object().unwrap().len(), 40);
                        break;
                    }
                    total += value["items"].as_array().unwrap().len();
                    cursor = value["nextCursor"].as_str().map(str::to_owned);
                    if cursor.is_none() {
                        break;
                    }
                }
                if workload != "complete-catalogue" {
                    assert_eq!(
                        total,
                        if *path == "/api/plugins/packages" {
                            42
                        } else {
                            40
                        }
                    );
                }
            }
            samples.push(
                json!({ "sample": sample, "workload": workload, "requests": requests,
                "encodedBytes": bytes, "wallMs": started.elapsed().as_secs_f64()*1000.0,
                "processCpuMs": process_cpu_ms()-cpu }),
            );
        }
    }
    let review = get(&app, "/api/plugins/packages/selector-00/versions/1").await;
    let mut writes = Vec::new();
    for _ in 0..6 {
        let cpu = process_cpu_ms();
        let started = std::time::Instant::now();
        registry::approve(&service, "selector-00", "1", text(&review, "digest"))
            .await
            .unwrap();
        writes.push(json!({"wallMs": started.elapsed().as_secs_f64()*1000.0,"processCpuMs":process_cpu_ms()-cpu}));
    }
    eprintln!(
        "SELECTOR_MEASUREMENT {}",
        json!({
            "packagesCopied":40,"versionsCopied":79,"instancesCopied":40,"agents":40,"savedRules":2000,
            "cache":"one API-cold and five warm samples; OS caches retained; local HTTP router in same Rust process",
            "reference":"complete catalogue decoded as JSON, including every manifest, version and instance setting",
            "candidate":"all package cards, all instance access pages and all grant-count pages decoded as JSON; no selected schema yet",
            "writes":"existing approval path unchanged, six samples after reads; no new persisted format",
            "samples":samples,"approvalWrites":writes,
        })
    );
}
