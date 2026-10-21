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
    let value = common::read_json(response).await;
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
async fn compact_package_pages_keep_exact_version_review_separate() {
    let (_directory, service, app) = fixture().await;
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
    let (_directory, service, app) = fixture().await;
    for n in 0..40 {
        copied_version(&service, &format!("catalogue-{n:02}"), "1");
    }
    for n in 2..=40 {
        copied_version(&service, "catalogue-00", &format!("{n:02}"));
    }
    let mut reads = Vec::new();
    for sample in 0..6 {
        // Alternate warm order. First reads are API-cold, not OS-cache cold.
        let mut paths = ["/api/plugins", "/api/plugins/packages"];
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
            "reference": "complete owner response, same binary and unchanged record format",
            "candidate": "first compact page; at most 32 cards",
            "cache": "first API read then five warm samples, OS caches retained",
            "writes": "existing approval path, identical for both read contracts",
            "reads": reads,
            "approvalWrites": writes,
        })
    );
}
