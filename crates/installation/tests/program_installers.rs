mod common;

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use cairn_installation::{
    plugins::{program_installation, registry},
    service::Service,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Cursor, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;
use tokio::sync::Mutex;

const MANIFEST: &str = r#"
id = "program-probe"
name = "Program probe"
version = "1.0.0"
apiVersion = 2
capabilities = ["http", "exec"]
hosts = ["downloads.example.com"]
[[programs]]
name = "probe"
installer = "programs/probe.luau"
"#;

const INSTALLER: &str = r#"
return {
    resolve = function(input)
        assert(input.platform.os == "linux")
        assert(input.instances == nil and input.secrets == nil)
        local response = cairn.http.request({ url = "https://downloads.example.com/latest" })
        assert(response.status == 200)
        return response.json
    end,
    install = function(input)
        local r = input.release
        local archive = cairn.download({ url = r.url, sha256 = r.sha256 })
        return cairn.archive.executable({ archive = archive.id, format = r.format, path = r.executable })
    end,
}
"#;

#[derive(Default)]
struct Publisher {
    release: Mutex<Value>,
    files: Mutex<HashMap<String, Vec<u8>>>,
    requests: AtomicUsize,
    download_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    metadata_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
}

async fn serve(
    State(publisher): State<Arc<Publisher>>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    assert!(headers.get("authorization").is_none());
    publisher.requests.fetch_add(1, Ordering::SeqCst);
    let path = request.uri().path();
    // The opt-in feed can advertise a prerelease even when a stable release exists.
    if path == "/check/1/0/CLI2/en/2.0.0/Y" {
        return Json(json!({ "available": "1", "version": "2.43.0-beta.02" })).into_response();
    }
    if path == "/latest" || path.starts_with("/check/") {
        if let Some(gate) = publisher.metadata_gate.lock().await.clone() {
            gate.notified().await;
        }
        return Json(publisher.release.lock().await.clone()).into_response();
    }
    if path == "/redirect" {
        return (
            StatusCode::FOUND,
            [("location", "https://evil.example.com/program")],
        )
            .into_response();
    }
    let gate = publisher.download_gate.lock().await.clone();
    if let Some(gate) = gate {
        gate.notified().await;
    }
    match publisher.files.lock().await.get(path).cloned() {
        Some(bytes) => Body::from(bytes).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn archive(name: &str, version: &str) -> Vec<u8> {
    archive_program(name, &format!("#!/bin/sh\nprintf '{version}'\n"))
}

fn archive_program(name: &str, program: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(name, zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(program.as_bytes()).unwrap();
    zip.finish().unwrap().into_inner()
}

fn symlink_archive() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.add_symlink(
        "probe",
        "/etc/passwd",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.finish().unwrap().into_inner()
}

impl Publisher {
    async fn release(&self, version: &str) {
        let bytes = archive("probe", version);
        let path = format!("/{version}.zip");
        *self.release.lock().await = json!({
            "version": version,
            "url": format!("https://downloads.example.com{path}"),
            "sha256": hex::encode(Sha256::digest(&bytes)),
            "format": "zip",
            "executable": "probe",
        });
        self.files.lock().await.insert(path, bytes);
    }
}

struct Fixture {
    root: TempDir,
    service: Arc<Service>,
    publisher: Arc<Publisher>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        let service = Service::new(common::config(root.path())).await.unwrap();
        common::relay_fixture::claimed(&service).await.unwrap();
        service
            .plugins
            .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
        let publisher = Arc::new(Publisher::default());
        publisher.release("1.0.0").await;
        let (origin, server) =
            common::serve_locally(Router::new().fallback(serve).with_state(publisher.clone()))
                .await;
        service.plugins.route_http_to(Some(origin));
        Self {
            root,
            service,
            publisher,
            server,
        }
    }

    async fn upload(
        &self,
        manifest: &str,
        installer: &str,
    ) -> cairn_installation::error::Result<Value> {
        common::package_install(
            &self.service,
            &json!({
                "manifest": manifest, "manifestFormat": "toml", "source": "return {}",
                "modules": { "programs/probe.luau": installer },
            }),
        )
        .await
    }

    async fn replace_owner_access(&self) -> tokio::task::JoinHandle<()> {
        let (origin, policy) = common::serve_locally(Router::new().fallback(|| async {
            Json(cairn_protocol::TaskAuthorPolicy {
                owner: cairn_protocol::TaskAuthorGrant {
                    account_id: "fixture-owner".into(),
                    access_id: "replacement-access".into(),
                },
                members: Vec::new(),
            })
        }))
        .await;
        let identity_path = self
            .service
            .config
            .data_dir
            .join("installation-relay/identity.json");
        let mut identity: Value =
            serde_json::from_slice(&std::fs::read(&identity_path).unwrap()).unwrap();
        identity["origin"] = json!(origin);
        std::fs::write(identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
        policy
    }

    async fn approve(&self) -> Value {
        let package = self.upload(MANIFEST, INSTALLER).await.unwrap();
        common::package_approve(
            &self.service,
            "program-probe",
            "1.0.0",
            package["digest"].as_str().unwrap(),
        )
        .await
        .unwrap()
    }

    async fn action(
        &self,
        action: &str,
        input: &Value,
    ) -> cairn_installation::error::Result<Value> {
        program_installation::action(&self.service, "program-probe", "probe", action, input).await
    }

    async fn output(&self) -> String {
        let package = registry::active(&self.service, "program-probe")
            .await
            .unwrap();
        let program = package.manifest.program("probe").unwrap();
        let path = program_installation::executable(&self.service, &package.manifest, program)
            .await
            .unwrap();
        let output = tokio::process::Command::new(path).output().await.unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }
}

#[tokio::test]
async fn toml_packages_prepare_only_after_approval_and_keep_exact_releases_across_updates_and_restart()
 {
    let f = Fixture::new().await;
    let upload = f.upload(MANIFEST, INSTALLER).await.unwrap();
    assert_eq!(f.publisher.requests.load(Ordering::SeqCst), 0);
    assert!(
        program_installation::list(&f.service, "program-probe")
            .await
            .is_err()
    );
    assert!(
        common::package_approve(&f.service, "program-probe", "1.0.0", "stale")
            .await
            .is_err()
    );
    assert_eq!(f.publisher.requests.load(Ordering::SeqCst), 0);
    common::package_approve(
        &f.service,
        "program-probe",
        "1.0.0",
        upload["digest"].as_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(f.output().await, "1.0.0");
    assert_eq!(f.publisher.requests.load(Ordering::SeqCst), 2);

    // JSON and TOML are normalized before hashing and persistence.
    let package = registry::active(&f.service, "program-probe").await.unwrap();
    let same = common::package_install(&f.service, &json!({
        "manifest": package.manifest, "source": "return {}", "modules": { "programs/probe.luau": INSTALLER },
    })).await.unwrap();
    assert_eq!(same["digest"], upload["digest"]);

    f.publisher.release("1.1.0").await;
    assert_eq!(f.output().await, "1.0.0");
    let reopened = Service::new(common::config(f.root.path())).await.unwrap();
    // No worker and no publisher routing: reusing the persisted executable needs neither.
    let path = program_installation::executable(
        &reopened,
        &package.manifest,
        package.manifest.program("probe").unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"#!/bin/sh\nprintf '1.0.0'\n");
    assert_eq!(f.publisher.requests.load(Ordering::SeqCst), 2);
    drop(reopened);

    let checked = f.action("check", &json!({})).await.unwrap();
    assert_eq!(checked["candidate"]["version"], "1.1.0");
    assert!(
        f.action("install", &json!({ "digest": "stale" }))
            .await
            .is_err()
    );
    assert_eq!(f.output().await, "1.0.0");
    let installed = f
        .action("install", &json!({ "digest": checked["digest"] }))
        .await
        .unwrap();
    assert_eq!(installed["previous"]["release"]["version"], "1.0.0");
    assert_eq!(f.output().await, "1.1.0");
    let requests = f.publisher.requests.load(Ordering::SeqCst);
    f.action("rollback", &json!({})).await.unwrap();
    assert_eq!(f.output().await, "1.0.0");
    assert_eq!(f.publisher.requests.load(Ordering::SeqCst), requests);
}

#[tokio::test]
async fn bad_checksums_unsafe_archives_and_redirects_preserve_the_installed_program() {
    let f = Fixture::new().await;
    f.approve().await;
    for (path, bytes, expected) in [
        ("/wrong.zip", archive("probe", "broken"), "0".repeat(64)),
        (
            "/traversal.zip",
            archive("../probe", "broken"),
            String::new(),
        ),
        ("/symlink.zip", symlink_archive(), String::new()),
        ("/redirect", Vec::new(), String::new()),
    ] {
        let checksum = if expected.is_empty() {
            hex::encode(Sha256::digest(&bytes))
        } else {
            expected
        };
        f.publisher.files.lock().await.insert(path.into(), bytes);
        *f.publisher.release.lock().await = json!({
            "version": "broken", "url": format!("https://downloads.example.com{path}"),
            "sha256": checksum, "format": "zip", "executable": "probe",
        });
        let checked = f.action("check", &json!({})).await.unwrap();
        assert!(
            f.action("install", &json!({ "digest": checked["digest"] }))
                .await
                .is_err()
        );
        assert_eq!(f.output().await, "1.0.0");
    }
}

#[tokio::test]
async fn installers_cannot_request_instance_secrets_or_other_capabilities() {
    let f = Fixture::new().await;
    for forbidden in [
        "cairn.storage.get('token')",
        "cairn.exec.run({program='probe'})",
        "cairn.http.request({url='https://downloads.example.com/latest',auth=true})",
        "cairn.http.request({url='https://evil.example.com/latest'})",
        "cairn.download({url='https://downloads.example.com/file'})",
    ] {
        let installer = INSTALLER.replace("assert(input.platform.os == \"linux\")", forbidden);
        let digest = hex::encode(Sha256::digest(forbidden.as_bytes()));
        let name = format!("probe-{}", &digest[..8]);
        let manifest = MANIFEST.replace("program-probe", &name);
        let package = f.upload(&manifest, &installer).await.unwrap();
        assert!(
            common::package_approve(
                &f.service,
                &name,
                "1.0.0",
                package["digest"].as_str().unwrap()
            )
            .await
            .is_err(),
            "{forbidden}"
        );
        assert!(registry::active(&f.service, &name).await.is_err());
    }
    assert_eq!(f.publisher.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn failed_program_preparation_does_not_activate_a_new_package_version() {
    let f = Fixture::new().await;
    f.approve().await;
    let manifest = MANIFEST.replace("version = \"1.0.0\"", "version = \"2.0.0\"");
    let installer = INSTALLER.replace(
        "assert(input.platform.os == \"linux\")",
        "cairn.fail('Unsupported platform')",
    );
    let update = f.upload(&manifest, &installer).await.unwrap();
    let error = common::package_approve(
        &f.service,
        "program-probe",
        "2.0.0",
        update["digest"].as_str().unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, 502);
    assert_eq!(error.message, "Unsupported platform");
    assert_eq!(
        registry::active(&f.service, "program-probe")
            .await
            .unwrap()
            .manifest
            .version,
        "1.0.0"
    );
    assert_eq!(f.output().await, "1.0.0");
}

#[tokio::test]
async fn installed_binaries_are_checked_before_execution() {
    let f = Fixture::new().await;
    f.approve().await;
    let package = registry::active(&f.service, "program-probe").await.unwrap();
    let program = package.manifest.program("probe").unwrap();
    let path = program_installation::executable(&f.service, &package.manifest, program)
        .await
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(path, "tampered").unwrap();
    assert!(
        program_installation::executable(&f.service, &package.manifest, program)
            .await
            .is_err()
    );
    assert_eq!(f.publisher.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn installer_modules_must_exist_and_export_both_handlers() {
    let f = Fixture::new().await;
    assert!(
        f.upload(
            &MANIFEST.replace("programs/probe.luau", "missing.luau"),
            INSTALLER
        )
        .await
        .is_err()
    );
    assert!(
        f.upload(MANIFEST, "return { resolve = function() end }")
            .await
            .is_err()
    );
    assert_eq!(f.publisher.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn onepassword_discovers_compatible_releases_and_installs_its_own_cli() {
    let f = Fixture::new().await;
    *f.publisher.release.lock().await = json!({ "version": "2.41.0" });
    let arch = if std::env::consts::ARCH == "aarch64" {
        "arm64"
    } else {
        "amd64"
    };
    f.publisher.files.lock().await.insert(
        format!("/dist/1P/op2/pkg/v2.41.0/op_linux_{arch}_v2.41.0.zip"),
        archive("op", "2.41.0"),
    );
    let checked =
        program_installation::action(&f.service, "onepassword", "op", "check", &json!({}))
            .await
            .unwrap();
    assert_eq!(checked["candidate"]["version"], "2.41.0");
    let installed = program_installation::action(
        &f.service,
        "onepassword",
        "op",
        "install",
        &json!({ "digest": checked["digest"] }),
    )
    .await
    .unwrap();
    assert_eq!(installed["installed"]["release"]["version"], "2.41.0");
    assert_eq!(
        installed["installed"]["archiveSha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    for version in ["3.0.0", "2.38.0", "2.41.0-beta.1", "garbage"] {
        *f.publisher.release.lock().await = json!({ "version": version });
        assert!(
            program_installation::action(&f.service, "onepassword", "op", "check", &json!({}))
                .await
                .is_err(),
            "{version}"
        );
    }
}

#[tokio::test]
async fn preparing_an_update_does_not_block_the_installed_program() {
    let f = Fixture::new().await;
    f.approve().await;
    f.publisher.release("1.1.0").await;
    let checked = f.action("check", &json!({})).await.unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    *f.publisher.download_gate.lock().await = Some(gate.clone());
    let service = f.service.clone();
    let before = f.publisher.requests.load(Ordering::SeqCst);
    let update = tokio::spawn(async move {
        program_installation::action(
            &service,
            "program-probe",
            "probe",
            "install",
            &json!({ "digest": checked["digest"] }),
        )
        .await
    });
    common::eventually(
        std::time::Duration::from_secs(10),
        std::time::Duration::from_millis(10),
        async || (f.publisher.requests.load(Ordering::SeqCst) > before).then_some(()),
    )
    .await;
    let running = tokio::time::timeout(std::time::Duration::from_secs(5), f.output()).await;
    gate.notify_one();
    update.await.unwrap().unwrap();
    assert_eq!(running.unwrap(), "1.0.0");
    assert_eq!(f.output().await, "1.1.0");
}

#[tokio::test]
async fn release_metadata_larger_than_half_a_frame_is_not_duplicated() {
    let f = Fixture::new().await;
    let release = f.publisher.release.lock().await.clone();
    *f.publisher.release.lock().await =
        json!({ "release": release, "padding": "x".repeat(600_000) });
    let installer = INSTALLER.replace("return response.json", "return response.json.release");
    let package = f.upload(MANIFEST, &installer).await.unwrap();
    common::package_approve(
        &f.service,
        "program-probe",
        "1.0.0",
        package["digest"].as_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(f.output().await, "1.0.0");
}

#[tokio::test]
async fn first_use_can_finish_a_download_longer_than_the_normal_handler_deadline() {
    let f = Fixture::new().await;
    *f.publisher.release.lock().await = json!({ "version": "2.41.0" });
    let arch = if std::env::consts::ARCH == "aarch64" {
        "arm64"
    } else {
        "amd64"
    };
    // This first-use path exercises the bundled package, including its foreground
    // cache declaration. The downloaded fixture must implement both CLI roles.
    let program = r#"#!/usr/bin/python3
import os,socket,sys
if sys.argv[1:2] == ['daemon']:
    server=socket.socket(socket.AF_UNIX)
    server.bind(os.environ['OP_SOCK'])
    server.listen(128)
    while True:
        client,_=server.accept()
        client.close()
else:
    print('[]')
"#;
    f.publisher.files.lock().await.insert(
        format!("/dist/1P/op2/pkg/v2.41.0/op_linux_{arch}_v2.41.0.zip"),
        archive_program("op", program),
    );
    let account = common::save_plugin(
        &f.service,
        "onepassword",
        &json!({
            "name": "Fixture", "secrets": { "token": "ops_fixture" }, "enabled": true,
        }),
        None,
    )
    .await
    .unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    *f.publisher.download_gate.lock().await = Some(gate.clone());
    let path = format!(
        "/api/plugins/onepassword/connections/{}/test",
        account["id"].as_str().unwrap()
    );
    let accepted = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        common::owner_plugin_route(&f.service, "POST", &path, json!({})),
    )
    .await
    .unwrap()
    .unwrap();
    let operation = accepted["operation"].as_str().unwrap();
    common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || (f.publisher.requests.load(Ordering::SeqCst) >= 2).then_some(()),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_secs(36)).await;
    assert_eq!(
        f.service.plugins.operations.get(operation).unwrap()["state"],
        "running"
    );
    gate.notify_one();
    let completed = common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || {
            let state = f.service.plugins.operations.get(operation).unwrap();
            (state["state"] != "running").then_some(state)
        },
    )
    .await;
    assert_eq!(completed["state"], "completed", "{completed}");
    assert_eq!(completed["result"]["output"]["ok"], true);
}

/// Reproducible local I/O reference; deliberately excluded from normal tests.
#[tokio::test]
#[ignore = "local program-lock read/write measurements; run with --ignored --nocapture"]
async fn measure_program_lock_reads_and_installation() {
    use std::{os::fd::AsRawFd, time::Instant};

    fn cpu_ms() -> f64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: time points to a valid timespec; this clock takes no other inputs.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
            0
        );
        time.tv_sec as f64 * 1000.0 + time.tv_nsec as f64 / 1_000_000.0
    }
    fn advise_cold(path: &std::path::Path) {
        let file = std::fs::File::open(path).unwrap();
        file.sync_all().unwrap();
        // SAFETY: the descriptor is live and the range means the whole file.
        assert_eq!(
            unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) },
            0
        );
    }

    let f = Fixture::new().await;
    let mut bytes = vec![0_u8; 4 * 1024 * 1024];
    let mut random = 0x1234_5678_u32;
    for byte in &mut bytes {
        random ^= random << 13;
        random ^= random >> 17;
        random ^= random << 5;
        *byte = random as u8;
    }
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("probe", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&bytes).unwrap();
    let archive = zip.finish().unwrap().into_inner();
    let archive_bytes = archive.len();
    *f.publisher.release.lock().await = json!({
        "version": "measurement", "url": "https://downloads.example.com/measurement.zip",
        "sha256": hex::encode(Sha256::digest(&archive)), "format": "zip", "executable": "probe",
    });
    f.publisher
        .files
        .lock()
        .await
        .insert("/measurement.zip".into(), archive);
    let reference = f.root.path().join("reference-program");
    let start = Instant::now();
    let cpu = cpu_ms();
    let mut output = std::fs::File::create(&reference).unwrap();
    output.write_all(&bytes).unwrap();
    output.sync_all().unwrap();
    let reference_write =
        json!({ "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu });
    let start = Instant::now();
    let cpu = cpu_ms();
    f.approve().await;
    let installation =
        json!({ "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu });
    let package = registry::active(&f.service, "program-probe").await.unwrap();
    let managed = package.manifest.program("probe").unwrap();
    let path = program_installation::executable(&f.service, &package.manifest, managed)
        .await
        .unwrap();
    let lock_bytes = std::fs::metadata(path.parent().unwrap().join("lock.json"))
        .unwrap()
        .len();
    let mut direct = package.manifest.clone();
    direct.programs[0].installer.clear();
    direct.programs[0].path = reference.to_str().unwrap().into();
    let mut reads = Vec::new();
    for (name, manifest, target) in [
        ("reference", &direct, &reference),
        ("managed", &package.manifest, &path),
    ] {
        for (temperature, count) in [("cold-advised", 3), ("warm", 20)] {
            let mut samples = Vec::new();
            for _ in 0..count {
                if temperature == "cold-advised" {
                    advise_cold(target);
                }
                let start = Instant::now();
                let cpu = cpu_ms();
                program_installation::executable(&f.service, manifest, &manifest.programs[0])
                    .await
                    .unwrap();
                samples.push(json!({ "wallMs": start.elapsed().as_secs_f64() * 1000.0, "cpuMs": cpu_ms() - cpu }));
            }
            reads.push(json!({ "mode": name, "temperature": temperature, "samples": samples }));
        }
    }
    println!(
        "{}",
        json!({
            "workload": "4 MiB deterministic xorshift payload, ZIP over loopback, debug build, no executable spawn",
            "host": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH, "cpus": std::thread::available_parallelism().unwrap().get() },
            "executableBytes": bytes.len(), "archiveBytes": archive_bytes, "lockBytes": lock_bytes,
            "referenceWrite": reference_write, "installation": installation, "reads": reads,
        })
    );
}

#[tokio::test]
#[ignore = "downloads the current official 1Password CLI over public HTTPS"]
async fn smoke_install_official_onepassword_cli() {
    let f = Fixture::new().await;
    f.service.plugins.route_http_to(None);
    let candidate =
        program_installation::action(&f.service, "onepassword", "op", "check", &json!({}))
            .await
            .unwrap();
    let installed = program_installation::action(
        &f.service,
        "onepassword",
        "op",
        "install",
        &json!({ "digest": candidate["digest"] }),
    )
    .await
    .unwrap();
    let package = registry::active(&f.service, "onepassword").await.unwrap();
    let program = package.manifest.program("op").unwrap();
    let path = program_installation::executable(&f.service, &package.manifest, program)
        .await
        .unwrap();
    let output = tokio::process::Command::new(&path)
        .arg("--version")
        .env_clear()
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        candidate["candidate"]["version"].as_str().unwrap()
    );
    println!(
        "{}",
        json!({
            "version": installed["installed"]["release"]["version"],
            "archiveSha256": installed["installed"]["archiveSha256"],
            "binarySha256": installed["installed"]["binarySha256"],
            "executableBytes": std::fs::metadata(path).unwrap().len(),
        })
    );
}

#[tokio::test]
async fn owner_preparation_is_tracked_without_holding_a_relay_request_open() {
    let f = Fixture::new().await;
    let package = f.upload(MANIFEST, INSTALLER).await.unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    *f.publisher.download_gate.lock().await = Some(gate.clone());
    let accepted = common::owner_plugin_route(
        &f.service,
        "POST",
        "/api/plugins/packages/program-probe/approve",
        json!({ "version": "1.0.0", "digest": package["digest"] }),
    )
    .await
    .unwrap();
    let operation = accepted["operation"].as_str().unwrap();
    assert_eq!(
        f.service.plugins.operations.get(operation).unwrap()["state"],
        "running"
    );
    common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || (f.publisher.requests.load(Ordering::SeqCst) >= 2).then_some(()),
    )
    .await;
    assert!(registry::active(&f.service, "program-probe").await.is_err());
    gate.notify_one();
    *f.publisher.download_gate.lock().await = None;
    let completed = common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || {
            let state = f.service.plugins.operations.get(operation).unwrap();
            (state["state"] != "running").then_some(state)
        },
    )
    .await;
    assert_eq!(completed["state"], "completed", "{completed}");
    assert_eq!(f.output().await, "1.0.0");

    f.publisher.release("1.1.0").await;
    f.publisher.release.lock().await["sha256"] = json!("0".repeat(64));
    let candidate = f.action("check", &json!({})).await.unwrap();
    let service = f.service.clone();
    let accepted = f
        .service
        .plugins
        .operations
        .start(
            f.service.clone(),
            cairn_installation::plugins::operations::Audience::Owner,
            async move {
                let value = program_installation::action(
                    &service,
                    "program-probe",
                    "probe",
                    "install",
                    &json!({ "digest": candidate["digest"] }),
                )
                .await?;
                Ok(cairn_contracts::PluginOperationResult::Program {
                    program: serde_json::from_value(value)?,
                })
            },
        )
        .unwrap();
    let operation = accepted["operation"].as_str().unwrap();
    let failed = common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || {
            let state = f.service.plugins.operations.get(operation).unwrap();
            (state["state"] != "running").then_some(state)
        },
    )
    .await;
    assert_eq!(failed["state"], "failed");
    assert!(failed["error"].as_str().unwrap().contains("SHA-256"));
    assert_eq!(f.output().await, "1.0.0");
}

#[tokio::test]
async fn unarchived_binaries_can_be_installed_and_reused() {
    let f = Fixture::new().await;
    let bytes = b"#!/bin/sh\nprintf '2.0.0'\n".to_vec();
    *f.publisher.release.lock().await = json!({
        "version": "2.0.0", "url": "https://downloads.example.com/binary",
        "sha256": hex::encode(Sha256::digest(&bytes)), "format": "binary", "executable": "probe",
    });
    f.publisher
        .files
        .lock()
        .await
        .insert("/binary".into(), bytes);
    f.approve().await;
    assert_eq!(f.output().await, "2.0.0");
    assert_eq!(f.output().await, "2.0.0");
    assert_eq!(f.publisher.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancelling_owner_preparation_prevents_activation_and_restart_does_not_replay_it() {
    let f = Fixture::new().await;
    let package = f.upload(MANIFEST, INSTALLER).await.unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    *f.publisher.download_gate.lock().await = Some(gate.clone());
    let accepted = common::owner_plugin_route(
        &f.service,
        "POST",
        "/api/plugins/packages/program-probe/approve",
        json!({"version":"1.0.0","digest":package["digest"]}),
    )
    .await
    .unwrap();
    let id = accepted["operation"].as_str().unwrap();
    common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || (f.publisher.requests.load(Ordering::SeqCst) >= 2).then_some(()),
    )
    .await;
    let cancelled = common::owner_plugin_route(
        &f.service,
        "DELETE",
        &format!("/api/plugins/packages/operations/{id}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    gate.notify_waiters();
    assert!(registry::active(&f.service, "program-probe").await.is_err());
    let restarted = Service::new(common::config(f.root.path())).await.unwrap();
    assert_eq!(
        restarted.plugins.operations.get(id).unwrap_err().status,
        404
    );
    assert!(registry::active(&restarted, "program-probe").await.is_err());
}

#[tokio::test]
async fn concurrent_package_approvals_preserve_committed_authority() {
    let f = Fixture::new().await;
    let package = f.upload(MANIFEST, INSTALLER).await.unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    *f.publisher.download_gate.lock().await = Some(gate.clone());
    let start = || {
        common::owner_plugin_route(
            &f.service,
            "POST",
            "/api/plugins/packages/program-probe/approve",
            json!({"version":"1.0.0","digest":package["digest"]}),
        )
    };
    let one = start().await.unwrap();
    common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || (f.publisher.requests.load(Ordering::SeqCst) >= 2).then_some(()),
    )
    .await;
    let two = start().await.unwrap();
    // Both commands capture the current package revision before installer preparation.
    // Polling metadata twice proves the first reached its gate; the second can only
    // publish after that same preparation mutex is released.
    tokio::task::yield_now().await;
    gate.notify_waiters();
    let states = common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || {
            let one = f
                .service
                .plugins
                .operations
                .get(one["operation"].as_str().unwrap())
                .unwrap();
            let two = f
                .service
                .plugins
                .operations
                .get(two["operation"].as_str().unwrap())
                .unwrap();
            (one["state"] != "running" && two["state"] != "running").then_some((one, two))
        },
    )
    .await;
    assert_eq!(states.0["state"], "completed");
    if states.1["state"] == "failed" {
        assert!(
            states.1["error"]
                .as_str()
                .unwrap()
                .contains("package changed")
        );
    } else {
        assert_eq!(states.1["state"], "completed");
    }
    let state = f
        .service
        .store
        .read(|db| db.plugin_package("program-probe"))
        .await
        .unwrap();
    assert_eq!(state.active.as_deref(), Some("1.0.0"));
    assert_eq!(
        state.approvals.get("1.0.0").map(String::as_str),
        package["digest"].as_str()
    );
}

#[tokio::test]
async fn revoked_owner_cannot_publish_a_prepared_program_or_package() {
    fn has_lock(path: &std::path::Path) -> bool {
        path.is_dir()
            && std::fs::read_dir(path).unwrap().any(|entry| {
                let path = entry.unwrap().path();
                path.file_name().is_some_and(|name| name == "lock.json") || has_lock(&path)
            })
    }

    let f = Fixture::new().await;
    let package = f.upload(MANIFEST, INSTALLER).await.unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    *f.publisher.download_gate.lock().await = Some(gate.clone());
    let accepted = common::owner_plugin_route(
        &f.service,
        "POST",
        "/api/plugins/packages/program-probe/approve",
        json!({"version":"1.0.0","digest":package["digest"]}),
    )
    .await
    .unwrap();
    common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || (f.publisher.requests.load(Ordering::SeqCst) >= 2).then_some(()),
    )
    .await;
    // The same account with a new access ID must invalidate the old proof.
    let policy = f.replace_owner_access().await;
    gate.notify_waiters();
    let operation = common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || {
            let operation = f
                .service
                .plugins
                .operations
                .get(accepted["operation"].as_str().unwrap())
                .unwrap();
            (operation["state"] != "running").then_some(operation)
        },
    )
    .await;
    assert_eq!(operation["state"], "failed", "{operation}");
    assert!(
        operation["error"].as_str().unwrap().contains("owner"),
        "{operation}"
    );
    assert!(registry::active(&f.service, "program-probe").await.is_err());
    let programs = program_installation::list(&f.service, "program-probe").await;
    assert!(
        programs.is_err(),
        "An unapproved package must remain inactive"
    );
    // The binary and lock must not have been published before the final check.
    assert!(!has_lock(
        &f.service.config.data_dir.join("plugin-programs")
    ));
    policy.abort();
}

#[tokio::test]
async fn cold_instance_preparation_rechecks_owner_access_before_installing_and_returning() {
    for revoke in [false, true] {
        let f = Fixture::new().await;
        let manifest = MANIFEST.replace("[[programs]]", "instanceTest = true\n[[programs]]");
        let package = common::package_install(&f.service, &json!({
            "manifest": manifest, "manifestFormat": "toml",
            "source": "return { test = function() cairn.exec.run({program='probe'}) return {ok=true} end }",
            "modules": { "programs/probe.luau": INSTALLER },
        })).await.unwrap();
        common::package_approve(
            &f.service,
            "program-probe",
            "1.0.0",
            package["digest"].as_str().unwrap(),
        )
        .await
        .unwrap();
        let instance = cairn_installation::plugins::connections::save(
            &f.service,
            "program-probe",
            serde_json::from_value(json!({"name":"Probe","enabled":true,"settings":{}})).unwrap(),
            None,
        )
        .await
        .unwrap();
        // Model a missing local program after restart while retaining the approved package.
        std::fs::remove_dir_all(f.service.config.data_dir.join("plugin-programs")).unwrap();
        let gate = Arc::new(tokio::sync::Notify::new());
        *f.publisher.download_gate.lock().await = Some(gate.clone());
        let accepted = common::owner_plugin_route(
            &f.service,
            "POST",
            &format!(
                "/api/plugins/program-probe/connections/{}/test",
                instance.id
            ),
            json!({}),
        )
        .await
        .unwrap();
        common::eventually(
            std::time::Duration::from_secs(15),
            std::time::Duration::from_millis(10),
            async || (f.publisher.requests.load(Ordering::SeqCst) >= 4).then_some(()),
        )
        .await;
        let policy = if revoke {
            Some(f.replace_owner_access().await)
        } else {
            None
        };
        gate.notify_waiters();
        let operation = common::eventually(
            std::time::Duration::from_secs(15),
            std::time::Duration::from_millis(10),
            async || {
                let operation = f
                    .service
                    .plugins
                    .operations
                    .get(accepted["operation"].as_str().unwrap())
                    .unwrap();
                (operation["state"] != "running").then_some(operation)
            },
        )
        .await;
        let programs = program_installation::list(&f.service, "program-probe")
            .await
            .unwrap();
        if let Some(policy) = policy {
            assert_eq!(operation["state"], "failed", "{operation}");
            assert!(
                operation["error"].as_str().unwrap().contains("owner"),
                "{operation}"
            );
            assert!(programs["programs"][0]["installed"].is_null(), "{programs}");
            policy.abort();
        } else {
            assert_eq!(operation["state"], "completed", "{operation}");
            assert!(
                !programs["programs"][0]["installed"].is_null(),
                "{programs}"
            );
        }
    }
}

#[tokio::test]
async fn cancelling_an_operation_fences_its_queued_package_publication() {
    use std::future::Future;
    let f = Fixture::new().await;
    let package = f.upload(MANIFEST, INSTALLER).await.unwrap();
    let digest = package["digest"].as_str().unwrap().to_owned();
    let (release, wait) = std::sync::mpsc::channel();
    let occupied = Arc::new(tokio::sync::Notify::new());
    let blocker = tokio::spawn({
        let s = f.service.clone();
        let occupied = occupied.clone();
        async move {
            s.store
                .write(move |_| {
                    occupied.notify_one();
                    let _ = wait.recv_timeout(std::time::Duration::from_secs(30));
                    Ok(())
                })
                .await
                .unwrap();
        }
    });
    occupied.notified().await;
    let queued = Arc::new(tokio::sync::Notify::new());
    let started = f
        .service
        .plugins
        .operations
        .start(
            f.service.clone(),
            cairn_installation::plugins::operations::Audience::Owner,
            {
                let s = f.service.clone();
                let queued = queued.clone();
                async move {
                    let publication = s.store.transaction(move |db| {
                        db.activate_plugin_version("program-probe", "1.0.0", &digest, true)
                    });
                    tokio::pin!(publication);
                    std::future::poll_fn(|context| {
                        let result = publication.as_mut().poll(context);
                        if result.is_pending() {
                            queued.notify_one();
                        }
                        result
                    })
                    .await?;
                    Ok(cairn_contracts::PluginOperationResult::Package {
                        version: Box::new(serde_json::from_value(package).unwrap()),
                    })
                }
            },
        )
        .unwrap();
    queued.notified().await;
    let operation = started["operation"].as_str().unwrap();
    let cancelled = common::owner_plugin_route(
        &f.service,
        "DELETE",
        &format!("/api/plugins/packages/operations/{operation}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    release.send(()).unwrap();
    blocker.await.unwrap();
    f.service.store.write(|_| Ok(())).await.unwrap();
    let state = f
        .service
        .store
        .read(|db| db.plugin_package("program-probe"))
        .await
        .unwrap();
    assert!(state.active.is_none());
    assert!(state.approvals.is_empty());
    assert_eq!(
        f.service.plugins.operations.get(operation).unwrap()["state"],
        "cancelled"
    );
}

#[tokio::test]
async fn revoked_public_mcp_cannot_activate_an_approved_package_after_preparation() {
    let f = Fixture::new().await;
    f.approve().await;
    let previous = f
        .service
        .store
        .read(|db| db.plugin_package("program-probe"))
        .await
        .unwrap();
    std::fs::remove_dir_all(f.service.config.data_dir.join("plugin-programs")).unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    *f.publisher.download_gate.lock().await = Some(gate.clone());
    let token = common::relay_fixture::grant(&f.service, &["manage"]).await;
    let app = common::relay_fixture::router(f.service.clone())
        .await
        .unwrap();
    let arguments = common::mcp_arguments(
        "activate_plugin_package",
        &json!({ "pluginId": "program-probe", "version": "1.0.0" }),
    );
    let request = common::request("POST", "/mcp")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": arguments,
            })
            .to_string(),
        ))
        .unwrap();
    let response = common::read_json(common::send(&app, request).await).await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let operation = response["result"]["structuredContent"]["result"]["operation"]
        .as_str()
        .unwrap()
        .to_owned();
    common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || (f.publisher.requests.load(Ordering::SeqCst) >= 4).then_some(()),
    )
    .await;
    f.service
        .store
        .delete(&format!(
            "test-relay:{}",
            cairn_installation::auth::digest(&token)
        ))
        .await
        .unwrap();
    gate.notify_waiters();
    let result = common::eventually(
        std::time::Duration::from_secs(15),
        std::time::Duration::from_millis(10),
        async || {
            let result = f.service.plugins.operations.get(&operation).unwrap();
            (result["state"] != "running").then_some(result)
        },
    )
    .await;
    assert_eq!(result["state"], "failed", "{result}");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("MCP authorization"),
        "{result}"
    );
    let current = f
        .service
        .store
        .read(|db| db.plugin_package("program-probe"))
        .await
        .unwrap();
    assert_eq!(current.revision, previous.revision);
    assert_eq!(current.active, previous.active);
    let programs = program_installation::list(&f.service, "program-probe")
        .await
        .unwrap();
    assert!(programs["programs"][0]["installed"].is_null(), "{programs}");
}

#[tokio::test]
async fn revoked_agent_first_use_cannot_download_or_publish_a_program() {
    use cairn_installation::{config::MAIN_AGENT_ID, plugins::agents, validation::text};
    use common::{AgentFixture, MissionFixture};
    for metadata in [true, false] {
        let mut f = Fixture::new().await;
        common::reconfigure(&mut f.service, |config| {
            config.runner_url = "http://runner.invalid".into()
        })
        .await;
        *f.publisher.release.lock().await = json!({ "version": "2.41.0" });
        let arch = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            "amd64"
        };
        f.publisher.files.lock().await.insert(
            format!("/dist/1P/op2/pkg/v2.41.0/op_linux_{arch}_v2.41.0.zip"),
            archive_program("op", "#!/bin/sh\nprintf '[]'\n"),
        );
        let account = common::save_plugin(
            &f.service,
            "onepassword",
            &json!({
                "name": "Fixture", "secrets": { "token": "ops_fixture" }, "enabled": true,
            }),
            None,
        )
        .await
        .unwrap();
        let instance = text(&account, "id").to_owned();
        f.service
            .agent_fixture(json!({ "plugins": [instance] }), Some(MAIN_AGENT_ID))
            .await
            .unwrap();
        let task = f
            .service
            .task_fixture(
                json!({ "name": "Probe", "agentId": MAIN_AGENT_ID, "prompt": "Probe" }),
                None,
            )
            .await
            .unwrap();
        let run = f
            .service
            .enqueue(text(&task, "id"), "manual", None)
            .await
            .unwrap();
        let run_id = text(&run, "id").to_owned();
        f.service
            .store
            .change_run(
                &run_id,
                cairn_installation::runs::RunChanges {
                    status: Some(cairn_contracts::RunStatus::Running),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let run = f.service.store.run(&run_id).await.unwrap();
        let configuration = f
            .service
            .mcps
            .run_configuration(&f.service, &run)
            .await
            .unwrap();
        let bearer = text(&configuration["env"], "CAIRN_MCP_RUN_TOKEN").to_owned();
        let gate = Arc::new(tokio::sync::Notify::new());
        if metadata {
            *f.publisher.metadata_gate.lock().await = Some(gate.clone());
        } else {
            *f.publisher.download_gate.lock().await = Some(gate.clone());
        }
        let s = f.service.clone();
        let mut call = tokio::spawn(async move {
            if metadata {
                agents::call_tool(
                    &s,
                    &bearer,
                    "onepassword",
                    &json!({ "operation": "vaults", "accountId": instance }),
                )
                .await
                .map(|_| ())
            } else {
                agents::command(&s, &bearer, &json!({ "plugin": "onepassword", "command": "vaults", "args": ["--account", instance] })).await.map(|_| ())
            }
        });
        let requests = if metadata { 1 } else { 2 };
        tokio::select! {
            result = &mut call => panic!("First-use call ended before the installer gate: {result:?}"),
            () = common::eventually(std::time::Duration::from_secs(15), std::time::Duration::from_millis(10), async || {
                (f.publisher.requests.load(Ordering::SeqCst) == requests).then_some(())
            }) => {},
        }
        if metadata {
            f.service
                .agent_fixture(json!({ "plugins": [] }), Some(MAIN_AGENT_ID))
                .await
                .unwrap();
        } else {
            f.service
                .mcps
                .revoke_run(&f.service, &run_id)
                .await
                .unwrap();
        }
        gate.notify_one();
        assert!(call.await.unwrap().is_err());
        assert_eq!(
            f.publisher.requests.load(Ordering::SeqCst),
            requests,
            "revocation must prevent the next installer HTTP effect"
        );
        let package = registry::active(&f.service, "onepassword").await.unwrap();
        let directory = f
            .service
            .config
            .data_dir
            .join("plugin-programs/onepassword")
            .join(package.digest)
            .join("op")
            .join(format!(
                "{}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ));
        assert!(
            !directory.join("lock.json").exists(),
            "revoked execution published an installed release"
        );
    }
}
