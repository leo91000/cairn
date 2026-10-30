use common::MissionFixture;

mod common;

use common::ProjectFixture;

use axum::{Router, body::Body, http::StatusCode};
use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_installation::plugins::run_credentials::RunCredentials;
use cairn_installation::{
    config::{Config, MAIN_AGENT_ID},
    service::Service,
    skills::zip,
    validation::text,
};
use common::{RelayContext, github::Github, relay_fixture};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tempfile::TempDir;

const SKILL: &str =
    "---\nname: review\ndescription: Review the current changes\n---\nRun scripts/check.sh.\n";

#[tokio::test]
async fn skill_usage_is_bounded_counts_delivered_mentions_once_and_is_shared_with_mcp() {
    let api = Api::new().await;
    for name in ["review", "deploy", "unused"] {
        api.ok(
            "PUT",
            &format!("/skills/global/{name}"),
            Some(json!({
                "content": SKILL.replace("name: review", &format!("name: {name}")),
            })),
        )
        .await;
    }
    api.service
        .store
        .write(|db| {
            db.save_conversation_boundary(&json!({ "id": "other-device" }))?;
            db.put_message(&json!({
                "id": "old",
                "chatId": "other-device",
                "text": "$unused",
                "status": "delivered",
                "createdAt": 0,
            }))?;
            for index in 0..1000 {
                let (status, message) = match index {
                    0 => ("delivered", "$review $review and `$deploy`"),
                    1 => ("delivered", "$review, $deploy and $unknown"),
                    2 => ("queued", "$unused"),
                    3 => ("sending", "$unused"),
                    4 => ("cancelled", "$unused"),
                    _ => (
                        "delivered",
                        "```\n$unused\n``` \\$unused a$unused $unused_extra",
                    ),
                };
                let message = json!({
                    "id": format!("message-{index}"),
                    "chatId": "other-device",
                    "text": message,
                    "status": status,
                    "createdAt": index,
                });
                db.put_message(&message)?;
                // Status updates/retries keep the same row and must not inflate usage.
                db.put_message(&message)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let listed = api.ok("GET", "/skills", None).await;
    let counts = |items: &Value| -> Vec<(String, u64)> {
        items
            .as_array()
            .unwrap()
            .iter()
            .map(|skill| {
                (
                    text(skill, "name").to_owned(),
                    skill["usageCount"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    assert_eq!(
        counts(&listed),
        vec![
            ("deploy".into(), 1),
            ("review".into(), 2),
            ("unused".into(), 0)
        ]
    );

    let token = relay_fixture::grant(&api.service, &["read"]).await;
    let request = common::request("POST", "/mcp")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": common::mcp_arguments("list_skills", &json!({})),
            })
            .to_string(),
        ))
        .unwrap();
    let response = common::send(&api.router, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = common::read_json(response).await;
    assert_eq!(
        counts(&response["result"]["structuredContent"]["result"]),
        counts(&listed)
    );
}

#[tokio::test]
async fn unusable_visible_files_mark_a_skill_invalid_in_the_library() {
    let api = Api::new().await;
    let directory = api.skill_dir("review");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("SKILL.md"), SKILL).unwrap();
    std::os::unix::fs::symlink("SKILL.md", directory.join("reference.md")).unwrap();

    let listed = api.ok("GET", "/skills", None).await;
    assert_eq!(listed[0]["valid"], false);
    assert!(
        listed[0]["error"]
            .as_str()
            .unwrap()
            .contains("symbolic links")
    );

    std::fs::remove_file(directory.join("reference.md")).unwrap();
    assert_eq!(api.ok("GET", "/skills", None).await[0]["valid"], true);
}

#[tokio::test]
async fn maximum_file_count_folder_moves_fit_the_atomic_batch_transport() {
    use cairn_installation::skills::bundle::Change;

    let api = Api::new().await;
    let old_folder = "a".repeat(231);
    let new_folder = "b".repeat(231);
    let mut writes = vec![Change::Write {
        path: "SKILL.md".into(),
        bytes: SKILL.as_bytes().to_vec(),
        executable: None,
    }];
    let mut moves = Vec::new();
    for index in 0..499 {
        let from = format!("{old_folder}/{index:03}.txt");
        let to = format!("{new_folder}/{index:03}.txt");
        writes.push(Change::Write {
            path: from.clone(),
            bytes: b"notes".to_vec(),
            executable: None,
        });
        moves.push(json!({
            "op": "move",
            "from": from,
            "to": to,
        }));
    }
    let saved = api
        .service
        .skills
        .apply("review", None, &writes, None)
        .await
        .unwrap();
    let payload = json!({
        "base": saved.revision,
        "changes": moves,
    });
    assert!(payload.to_string().len() > 150_000);

    let renamed = api
        .ok(
            "POST",
            "/skills/global/review/bundle",
            Some(payload.clone()),
        )
        .await;
    assert_eq!(renamed["files"].as_array().unwrap().len(), 500);
    assert_eq!(
        std::fs::read(
            api.skill_dir("review")
                .join(format!("{new_folder}/000.txt"))
        )
        .unwrap(),
        b"notes"
    );
    assert!(!api.skill_dir("review").join(old_folder).exists());

    // Only the atomic batch route accepts larger JSON; other skill routes retain their limit.
    let (status, _) = api
        .call("POST", "/skills/global/import", Some(payload))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

struct Api {
    _root: TempDir,
    service: Arc<Service>,
    router: Router,
    session: RelayContext,
}

impl Api {
    async fn new() -> Self {
        Self::with(|_| {}).await
    }

    async fn with(change: impl FnOnce(&mut Config)) -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let mut config = common::config(root.path());
        change(&mut config);
        let service = Service::new(config).await.unwrap();
        let router = relay_fixture::router(service.clone()).await.unwrap();
        let session = RelayContext::new(&relay_fixture::context(&service).await);
        Self {
            _root: root,
            service,
            router,
            session,
        }
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let request = self
            .session
            .authorize(common::request(method, &format!("/api{uri}")))
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .unwrap();
        let response = common::send(&self.router, request).await;
        let status = response.status();
        (status, common::read_json(response).await)
    }

    async fn ok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let (status, value) = self.call(method, uri, body).await;
        assert_eq!(status, StatusCode::OK, "{method} {uri}: {value}");
        value
    }

    fn skill_dir(&self, name: &str) -> PathBuf {
        self.service.config.home.join(".agents/skills").join(name)
    }

    async fn stage(&self, bytes: &[u8]) -> (StatusCode, Value) {
        let request = self
            .session
            .authorize(common::request("POST", "/api/skills/uploads"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes.to_vec()))
            .unwrap();
        let response = common::send(&self.router, request).await;
        let status = response.status();
        (status, common::read_json(response).await)
    }

    async fn upload(&self, bytes: &[u8]) -> String {
        let (status, staged) = self.stage(bytes).await;
        assert_eq!(status, StatusCode::OK, "{staged}");
        text(&staged, "id").to_owned()
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn paths(bundle: &Value) -> Vec<&str> {
    bundle["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn freezing_during_batch_saves_never_combines_versions() {
    use cairn_installation::skills::bundle::{self, Change};
    let api = Api::new().await;
    let changes = |version: usize| {
        let mut changes = vec![Change::Write {
            path: "SKILL.md".into(),
            bytes: SKILL.as_bytes().to_vec(),
            executable: None,
        }];
        for index in 0..64 {
            changes.push(Change::Write {
                path: format!("files/{index:03}.txt"),
                bytes: format!("version {version}\n").repeat(100).into_bytes(),
                executable: None,
            });
        }
        changes
    };
    api.service
        .skills
        .apply("review", None, &changes(0), None)
        .await
        .unwrap();
    let service = api.service.clone();
    let directory = api.skill_dir("review");
    let writer_directory = directory.clone();
    let writer = tokio::spawn(async move {
        for version in 1..=20 {
            let base = bundle::describe(&writer_directory).await.unwrap().revision;
            service
                .skills
                .apply("review", Some(&base), &changes(version), None)
                .await
                .unwrap();
        }
    });
    let mut reads = 0;
    while !writer.is_finished() || reads < 20 {
        let (revision, _) = api
            .service
            .skills
            .freeze(&directory.join("SKILL.md"))
            .await
            .unwrap();
        let frozen = cairn_installation::skills::frozen(&api.service.config, &revision).unwrap();
        let (_, files) = bundle::read_all(&frozen).await.unwrap();
        let supporting = files
            .iter()
            .filter(|(entry, _)| entry.path != "SKILL.md")
            .collect::<Vec<_>>();
        assert!(
            supporting
                .iter()
                .all(|(_, bytes)| bytes == &supporting[0].1),
            "Frozen files came from different saves"
        );
        reads += 1;
    }
    writer.await.unwrap();
}

#[tokio::test]
async fn old_frozen_bundles_are_retained_for_later_runs() {
    use cairn_installation::skills::bundle;
    let api = Api::new().await;
    api.service
        .skills
        .save("review", SKILL, None)
        .await
        .unwrap();
    let directory = api.skill_dir("review");
    let revision = bundle::describe(&directory).await.unwrap().revision;
    let frozen = cairn_installation::skills::frozen(&api.service.config, &revision).unwrap();
    bundle::copy_dir(&directory, &frozen).await.unwrap();
    let marker = frozen.with_file_name(format!("{revision}.used"));
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 24 * 60 * 60);
    std::fs::File::create(marker)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old))
        .unwrap();
    api.service
        .skills
        .save("review", &format!("{SKILL}New instructions\n"), None)
        .await
        .unwrap();
    api.service
        .skills
        .freeze(&directory.join("SKILL.md"))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(frozen.join("SKILL.md")).unwrap(),
        SKILL
    );
}

#[tokio::test]
#[ignore = "explicit local skill storage benchmark; run alone with --nocapture"]
async fn skill_bundle_storage_measurements() {
    use cairn_installation::skills::bundle::{self, Change};
    use std::{os::fd::AsRawFd, time::Instant};

    fn cpu_ms() -> f64 {
        let mut clock = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock points to a valid timespec and the clock ID is supported on Linux.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut clock) },
            0
        );
        clock.tv_sec as f64 * 1000.0 + clock.tv_nsec as f64 / 1_000_000.0
    }

    async fn reads(directory: &Path, names: &[String], bundled: bool, cold: bool) -> Value {
        let (mut wall, mut cpu) = (0.0, 0.0);
        for _ in 0..100 {
            if cold {
                for name in names {
                    let file = std::fs::File::open(directory.join(name)).unwrap();
                    // SAFETY: this is a live file descriptor; advice only affects its cached pages.
                    assert_eq!(
                        unsafe {
                            libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                        },
                        0
                    );
                }
            }
            let started_cpu = cpu_ms();
            let started = Instant::now();
            if bundled {
                std::hint::black_box(bundle::read_all(directory).await.unwrap());
            } else {
                for name in names {
                    std::hint::black_box(tokio::fs::read(directory.join(name)).await.unwrap());
                }
            }
            wall += started.elapsed().as_secs_f64() * 1000.0;
            cpu += cpu_ms() - started_cpu;
        }
        json!({
            "wallMs": wall / 100.0,
            "cpuMs": cpu / 100.0,
        })
    }

    let api = Api::new().await;
    let mut changes = vec![Change::Write {
        path: "SKILL.md".into(),
        bytes: SKILL.as_bytes().to_vec(),
        executable: None,
    }];
    for index in 0..64 {
        changes.push(Change::Write {
            path: format!("data/{index:03}.bin"),
            bytes: vec![index as u8; 4096],
            executable: Some(index % 8 == 0),
        });
    }
    api.service
        .skills
        .apply("review", None, &changes, None)
        .await
        .unwrap();
    let directory = api.skill_dir("review");
    let entries = bundle::scan(&directory).await.unwrap();
    let names = entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    let bytes = entries.iter().map(|entry| entry.size).sum::<u64>();
    let mut results = json!({
        "files": entries.len(),
        "payloadBytes": bytes,
        "frozenPayloadBytes": bytes,
    });
    for (label, bundled, cold) in [
        ("referenceWarmRead", false, false),
        ("bundleWarmRead", true, false),
        ("referenceColdRead", false, true),
        ("bundleColdRead", true, true),
    ] {
        results[label] = reads(&directory, &names, bundled, cold).await;
    }
    for bundled in [false, true] {
        let (mut wall, mut cpu) = (0.0, 0.0);
        for version in 0..20 {
            let input = vec![version; 4096];
            bundle::write_file(&directory.join("data/000.bin"), &input, true)
                .await
                .unwrap();
            let started_cpu = cpu_ms();
            let started = Instant::now();
            if bundled {
                api.service
                    .skills
                    .freeze(&directory.join("SKILL.md"))
                    .await
                    .unwrap();
            } else {
                bundle::copy_dir(
                    &directory,
                    &api._root.path().join(format!("reference-{version}")),
                )
                .await
                .unwrap();
            }
            wall += started.elapsed().as_secs_f64() * 1000.0;
            cpu += cpu_ms() - started_cpu;
        }
        results[if bundled {
            "freezeWrite"
        } else {
            "referenceCopyWrite"
        }] = json!({
            "wallMs": wall / 20.0,
            "cpuMs": cpu / 20.0,
        });
    }
    let task = api
        .service
        .task_fixture(
            json!({
                "name": "Storage measurement",
                "agentId": MAIN_AGENT_ID,
                "prompt": "Measure skill delivery",
                "skills": ["global/review"],
                "worktree": false,
            }),
            None,
        )
        .await
        .unwrap();
    let run = api.service.snapshot(task, "manual").await.unwrap();
    let (mut wall, mut cpu) = (0.0, 0.0);
    for _ in 0..20 {
        let started_cpu = cpu_ms();
        let started = Instant::now();
        let prepared = cairn_installation::execution::prepare(
            &run,
            &api.service.config,
            &RunCredentials::default(),
            None,
            None,
        )
        .await
        .unwrap();
        wall += started.elapsed().as_secs_f64() * 1000.0;
        cpu += cpu_ms() - started_cpu;
        let installed = Path::new(text(&prepared["skills"][0], "path"))
            .parent()
            .unwrap();
        let copied = bundle::scan(installed).await.unwrap();
        assert_eq!(copied.iter().map(|file| file.size).sum::<u64>(), bytes);
    }
    results["sharedRunPrepare"] = json!({
        "wallMs": wall / 20.0,
        "cpuMs": cpu / 20.0,
        "payloadBytes": bytes,
    });
    println!("{}", serde_json::to_string_pretty(&results).unwrap());
}

#[tokio::test]
async fn batches_save_every_file_at_once_with_modes_and_revisions() {
    let api = Api::new().await;
    let created = api
        .ok(
            "POST",
            "/skills/global/review/bundle",
            Some(json!({
                "base": null,
                "changes": [
                    { "op": "write", "path": "SKILL.md", "content": SKILL },
                    { "op": "write", "path": "scripts/check.sh", "content": "#!/bin/sh\necho ok\n" },
                    { "op": "write", "path": "references/notes.md", "content": "# Notes\n" },
                    { "op": "write", "path": "assets/logo.bin", "content": STANDARD.encode([0u8, 159, 146, 150]), "encoding": "base64" },
                ],
            })),
        )
        .await;
    assert_eq!(created["description"], "Review the current changes");
    assert_eq!(
        paths(&created),
        [
            "SKILL.md",
            "assets/logo.bin",
            "references/notes.md",
            "scripts/check.sh"
        ]
    );
    assert_eq!(created["files"][1]["text"], false);
    assert_eq!(created["files"][3]["executable"], true);
    assert_eq!(created["limits"]["files"], 500);
    let directory = api.skill_dir("review");
    assert_eq!(mode(&directory.join("scripts/check.sh")), 0o700);
    assert_eq!(mode(&directory.join("references/notes.md")), 0o600);

    let binary = api
        .ok(
            "GET",
            "/skills/global/review/file?path=assets/logo.bin",
            None,
        )
        .await;
    assert_eq!(binary["encoding"], "base64");
    assert_eq!(
        STANDARD.decode(text(&binary, "content")).unwrap(),
        [0u8, 159, 146, 150]
    );
    let script = api
        .ok(
            "GET",
            "/skills/global/review/file?path=scripts/check.sh",
            None,
        )
        .await;
    assert_eq!(
        script,
        json!({ "content": "#!/bin/sh\necho ok\n", "encoding": "utf-8", "size": 18, "executable": true })
    );

    // A create over an existing skill and an edit from a stale revision both conflict.
    let (status, _) = api
        .call(
            "POST",
            "/skills/global/review/bundle",
            Some(json!({ "base": null, "changes": [] })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, error) = api
        .call(
            "POST",
            "/skills/global/review/bundle",
            Some(json!({ "base": "0".repeat(64), "changes": [{ "op": "delete", "path": "SKILL.md" }] })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");

    let edited = api
        .ok(
            "POST",
            "/skills/global/review/bundle",
            Some(json!({
                "base": created["revision"],
                "changes": [
                    { "op": "move", "from": "references/notes.md", "to": "docs/notes.md" },
                    { "op": "delete", "path": "assets/logo.bin" },
                    { "op": "mode", "path": "scripts/check.sh", "executable": false },
                    { "op": "write", "path": "scripts/run.py", "content": "print('hi')\n", "executable": true },
                ],
            })),
        )
        .await;
    assert_ne!(edited["revision"], created["revision"]);
    assert_eq!(
        paths(&edited),
        [
            "SKILL.md",
            "docs/notes.md",
            "scripts/check.sh",
            "scripts/run.py"
        ]
    );
    assert!(!directory.join("assets").exists());
    assert!(!directory.join("references").exists());
    assert_eq!(mode(&directory.join("scripts/check.sh")), 0o600);
    assert_eq!(mode(&directory.join("scripts/run.py")), 0o700);

    // An invalid result changes nothing and leaves no staging folder.
    for changes in [
        json!([{ "op": "write", "path": "docs/new.md", "content": "x" }, { "op": "write", "path": "SKILL.md", "content": SKILL.replace("name: review", "name: other") }]),
        json!([{ "op": "delete", "path": "SKILL.md" }]),
        json!([{ "op": "write", "path": "../escape.md", "content": "x" }]),
        json!([{ "op": "write", "path": ".env", "content": "x" }]),
        json!([{ "op": "write", "path": "docs/notes.md/inner.md", "content": "x" }]),
        json!([{ "op": "move", "from": "docs/notes.md", "to": "SKILL.md" }]),
        json!([{ "op": "delete", "path": "missing.md" }]),
    ] {
        let (status, error) = api
            .call(
                "POST",
                "/skills/global/review/bundle",
                Some(json!({ "base": edited["revision"], "changes": changes })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{changes}: {error}");
    }
    let current = api.ok("GET", "/skills/global/review/bundle", None).await;
    assert_eq!(current["revision"], edited["revision"]);
    let root = api.service.config.home.join(".agents/skills");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);

    let listed = api.ok("GET", "/skills", None).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["files"], 4);
    assert_eq!(listed[0]["executables"], 1);
    assert!(listed[0]["source"].is_null());
    assert_eq!(
        api.ok("GET", "/skills/global/review/files", None).await,
        json!([
            "SKILL.md",
            "docs/notes.md",
            "scripts/check.sh",
            "scripts/run.py"
        ])
    );
}

#[tokio::test]
async fn immediate_manifest_writes_apply_explicit_modes() {
    let api = Api::new().await;
    for executable in [true, false] {
        let saved = api
            .ok(
                "PUT",
                "/skills/global/review/file",
                Some(json!({
                    "path": "SKILL.md",
                    "content": SKILL,
                    "executable": executable,
                })),
            )
            .await;
        assert_eq!(saved["name"], "review");
        assert_eq!(
            mode(&api.skill_dir("review").join("SKILL.md")),
            if executable { 0o700 } else { 0o600 }
        );
    }
}

#[tokio::test]
async fn older_single_file_writes_keep_working_and_respect_limits() {
    let api = Api::new().await;
    api.ok(
        "PUT",
        "/skills/global/review",
        Some(json!({ "content": SKILL })),
    )
    .await;
    api.ok(
        "PUT",
        "/skills/global/review/file",
        Some(json!({ "path": "scripts/check.sh", "content": "#!/bin/sh\n" })),
    )
    .await;
    assert_eq!(
        mode(&api.skill_dir("review").join("scripts/check.sh")),
        0o700
    );
    api.ok(
        "PUT",
        "/skills/global/review/file",
        Some(json!({ "path": "scripts/check.sh", "content": "#!/bin/sh\nexit 0\n", "executable": false })),
    )
    .await;
    assert_eq!(
        mode(&api.skill_dir("review").join("scripts/check.sh")),
        0o600
    );

    let (status, _) = api.stage(&vec![0; 5 * 1024 * 1024 + 1]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let large = vec![b'a'; 4 * 1024 * 1024 + 1];
    let upload = api.upload(&large).await;
    let revision = api.ok("GET", "/skills/global/review/bundle", None).await["revision"].clone();
    let (status, _) = api
        .call(
            "POST",
            "/skills/global/review/bundle",
            Some(json!({ "base": revision, "changes": [{ "op": "write", "path": "big.bin", "upload": upload }] })),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn staged_uploads_are_consumed_by_the_batch_that_uses_them() {
    let api = Api::new().await;
    let image = [137u8, 80, 78, 71, 13, 10, 26, 10, 0, 0];
    let upload = api.upload(&image).await;
    let body = json!({
        "base": null,
        "changes": [
            { "op": "write", "path": "SKILL.md", "content": SKILL },
            { "op": "write", "path": "assets/logo.png", "upload": upload },
        ],
    });
    api.ok("POST", "/skills/global/review/bundle", Some(body.clone()))
        .await;
    assert_eq!(
        std::fs::read(api.skill_dir("review").join("assets/logo.png")).unwrap(),
        image
    );
    let (status, error) = api
        .call("POST", "/skills/global/other/bundle", Some(body))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text(&error, "error").contains("expired"), "{error}");
}

async fn download(api: &Api, skill: &str) -> Vec<u8> {
    let mut archive = Vec::new();
    let mut revision = Value::Null;
    loop {
        let query = match revision.as_str() {
            Some(revision) => format!("offset={}&revision={revision}", archive.len()),
            None => "offset=0".to_owned(),
        };
        let chunk = api
            .ok(
                "GET",
                &format!("/skills/global/{skill}/archive?{query}"),
                None,
            )
            .await;
        assert_eq!(chunk["filename"], format!("{skill}.zip"));
        revision = chunk["revision"].clone();
        archive.extend(STANDARD.decode(text(&chunk, "content")).unwrap());
        if archive.len() as u64 == chunk["size"].as_u64().unwrap() {
            return archive;
        }
    }
}

#[tokio::test]
async fn archives_round_trip_and_imports_let_the_user_choose_a_folder() {
    let api = Api::new().await;
    api.ok(
        "POST",
        "/skills/global/review/bundle",
        Some(json!({
            "base": null,
            "changes": [
                { "op": "write", "path": "SKILL.md", "content": SKILL },
                { "op": "write", "path": "scripts/check.sh", "content": "#!/bin/sh\n" },
            ],
        })),
    )
    .await;

    let archive = download(&api, "review").await;
    let entries = zip::entries(&archive).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.executable))
            .collect::<Vec<_>>(),
        [
            ("review/SKILL.md", Some(false)),
            ("review/scripts/check.sh", Some(true))
        ]
    );

    // The exported archive imports back once the original is gone.
    let upload = api.upload(&archive).await;
    let (status, _) = api
        .call(
            "POST",
            "/skills/global/import",
            Some(json!({ "source": "archive", "upload": upload })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    api.ok("DELETE", "/skills/global/review", None).await;
    let imported = api
        .ok(
            "POST",
            "/skills/global/import",
            Some(json!({ "source": "archive", "upload": upload })),
        )
        .await;
    assert_eq!(imported["imported"]["name"], "review");
    assert_eq!(
        mode(&api.skill_dir("review").join("scripts/check.sh")),
        0o700
    );

    let triage = SKILL.replace("name: review", "name: triage");
    let collection = zip::write(&[
        zip::File {
            path: "skills/triage/SKILL.md",
            bytes: triage.as_bytes(),
            executable: false,
        },
        zip::File {
            path: "skills/triage/.hidden",
            bytes: b"x",
            executable: false,
        },
        zip::File {
            path: "skills/ship/SKILL.md",
            bytes: SKILL.replace("name: review", "name: ship").as_bytes(),
            executable: false,
        },
        zip::File {
            path: "__MACOSX/skills/._SKILL.md",
            bytes: b"x",
            executable: false,
        },
    ])
    .unwrap();
    let upload = api.upload(&collection).await;
    let choice = api
        .ok(
            "POST",
            "/skills/global/import",
            Some(json!({ "source": "archive", "upload": upload })),
        )
        .await;
    assert_eq!(
        choice,
        json!({ "imported": null, "candidates": ["skills/ship", "skills/triage"] })
    );
    let imported = api
        .ok(
            "POST",
            "/skills/global/import",
            Some(json!({ "source": "archive", "upload": upload, "path": "skills/triage" })),
        )
        .await;
    assert_eq!(paths(&imported["imported"]), ["SKILL.md"]);

    let upload = api.upload(b"not an archive").await;
    let (status, _) = api
        .call(
            "POST",
            "/skills/global/import",
            Some(json!({ "source": "archive", "upload": upload })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn runs_install_the_files_frozen_when_they_were_created() {
    let api = Api::new().await;
    let s = &api.service;
    api.ok(
        "POST",
        "/skills/global/review/bundle",
        Some(json!({
            "base": null,
            "changes": [
                { "op": "write", "path": "SKILL.md", "content": SKILL },
                { "op": "write", "path": "scripts/check.sh", "content": "#!/bin/sh\necho v1\n" },
            ],
        })),
    )
    .await;
    let task = s
        .task_fixture(
            json!({
                "name": "Review",
                "agentId": MAIN_AGENT_ID,
                "prompt": "Review",
                "skills": ["global/review"],
                "worktree": false,
            }),
            None,
        )
        .await
        .unwrap();
    let run = s.snapshot(task.clone(), "manual").await.unwrap();
    let selected = &run["snapshot"]["skills"][0];
    assert_eq!(selected["content"], SKILL);
    assert_eq!(selected["description"], "Review the current changes");
    let revision = text(selected, "bundle");
    let frozen = cairn_installation::skills::frozen(&s.config, revision).unwrap();
    assert_eq!(
        std::fs::read_to_string(frozen.join("scripts/check.sh")).unwrap(),
        "#!/bin/sh\necho v1\n"
    );
    assert_eq!(mode(&frozen.join("scripts/check.sh")), 0o700);

    // Later edits create another frozen revision and leave the first intact.
    let current = api.ok("GET", "/skills/global/review/bundle", None).await;
    assert_eq!(current["revision"], revision);
    api.ok(
        "POST",
        "/skills/global/review/bundle",
        Some(json!({
            "base": revision,
            "changes": [{ "op": "write", "path": "scripts/check.sh", "content": "#!/bin/sh\necho v2\n" }],
        })),
    )
    .await;
    let next = s.snapshot(task, "manual").await.unwrap();
    assert_ne!(next["snapshot"]["skills"][0]["bundle"], revision);
    assert_eq!(
        std::fs::read_to_string(frozen.join("scripts/check.sh")).unwrap(),
        "#!/bin/sh\necho v1\n"
    );
    assert!(cairn_installation::skills::frozen(&s.config, "../escape").is_none());

    // Shared runs get their own writable copy; scripts cannot mutate the canonical revision.
    let prepared = cairn_installation::execution::prepare(
        &next,
        &s.config,
        &RunCredentials::default(),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(prepared["isolated"], false);
    let installed = Path::new(text(&prepared["skills"][0], "path"));
    assert_eq!(
        std::fs::read_to_string(installed.with_file_name("scripts/check.sh")).unwrap(),
        "#!/bin/sh\necho v2\n"
    );
    let canonical = cairn_installation::skills::frozen(
        &s.config,
        text(&next["snapshot"]["skills"][0], "bundle"),
    )
    .unwrap();
    std::fs::write(
        installed.with_file_name("scripts/check.sh"),
        "script-generated output\n",
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(canonical.join("scripts/check.sh")).unwrap(),
        "#!/bin/sh\necho v2\n"
    );
    assert!(installed.starts_with(s.config.data_dir.join("runs").join(text(&next, "id"))));
    let mut another_run = next.clone();
    another_run["id"] = cairn_installation::config::id().into();
    for run in [&another_run, &next] {
        let restarted = cairn_installation::execution::prepare(
            run,
            &s.config,
            &RunCredentials::default(),
            None,
            None,
        )
        .await
        .unwrap();
        let path = Path::new(text(&restarted["skills"][0], "path"));
        assert_eq!(
            std::fs::read_to_string(path.with_file_name("scripts/check.sh")).unwrap(),
            "#!/bin/sh\necho v2\n"
        );
        assert_eq!(mode(&path.with_file_name("scripts/check.sh")), 0o700);
    }

    let mut malformed = next.clone();
    malformed["snapshot"]["skills"][0]["bundle"] = "../invalid".into();
    assert!(
        cairn_installation::execution::prepare(
            &malformed,
            &s.config,
            &RunCredentials::default(),
            None,
            None
        )
        .await
        .is_err()
    );

    let missing = cairn_installation::skills::frozen(
        &s.config,
        text(&next["snapshot"]["skills"][0], "bundle"),
    )
    .unwrap();
    std::fs::remove_dir_all(missing).unwrap();
    assert!(
        cairn_installation::execution::prepare(
            &next,
            &s.config,
            &RunCredentials::default(),
            None,
            None
        )
        .await
        .is_err()
    );

    // Missing revisions cannot silently read a live skill folder.
    let mut legacy = next;
    legacy["snapshot"]["skills"][0]
        .as_object_mut()
        .unwrap()
        .remove("bundle");
    assert!(
        cairn_installation::execution::prepare(
            &legacy,
            &s.config,
            &RunCredentials::default(),
            None,
            None,
        )
        .await
        .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshots_describe_the_manifest_version_that_was_frozen() {
    use cairn_installation::skills::bundle::{self, Base, Change};
    use std::os::unix::fs::OpenOptionsExt;

    let api = Api::new().await;
    let saved = api
        .ok(
            "POST",
            "/skills/global/review/bundle",
            Some(json!({
                "base": null,
                "changes": [{ "op": "write", "path": "SKILL.md", "content": SKILL }],
            })),
        )
        .await;
    let task = api
        .service
        .task_fixture(
            json!({
                "name": "Review",
                "agentId": MAIN_AGENT_ID,
                "prompt": "Review",
                "skills": ["global/review"],
                "worktree": false,
            }),
            None,
        )
        .await
        .unwrap();
    let project = api._root.path().join("project");
    let gate = project.join(".agents/skills/listing-gate/SKILL.md");
    std::fs::create_dir_all(gate.parent().unwrap()).unwrap();
    let gate_path = std::ffi::CString::new(gate.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(gate_path.as_ptr(), 0o600) }, 0);
    api.service
        .project_fixture(json!({ "name": "Listing trigger", "path": project }), None)
        .await
        .unwrap();
    let service = api.service.clone();
    let mut snapshot = tokio::spawn(async move { service.snapshot(task, "manual").await.unwrap() });
    // Hold the project listing inside its read lock after the global listing.
    // Opening the FIFO for writing blocks until the listing opens it to read.
    let writer = gate.clone();
    let mut opened =
        tokio::task::spawn_blocking(move || std::fs::OpenOptions::new().write(true).open(writer));
    let listing = tokio::select! {
        opened = &mut opened => opened.unwrap().expect("Unable to open listing gate"),
        finished = &mut snapshot => {
            // Release the blocked writer so the runtime can shut down.
            let _ = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&gate);
            panic!("The snapshot finished before listing the project: {finished:?}");
        }
    };
    let content = SKILL.replace(
        "Review the current changes",
        "Use the latest branch conventions",
    );
    let directory = api.skill_dir("review");
    let changes = [Change::Write {
        path: "SKILL.md".into(),
        bytes: content.as_bytes().to_vec(),
        executable: None,
    }];
    // The global skills root has its own lock, which the blocked project
    // listing does not hold: the writer commits between listing and freezing.
    // A shared lock would wait for the listing forever.
    let update = bundle::save(
        directory.parent().unwrap(),
        "review",
        Base::Edit(text(&saved, "revision")),
        &changes,
    );
    tokio::time::timeout(std::time::Duration::from_secs(60), update)
        .await
        .expect("A global skill save waited for a project listing")
        .unwrap();
    drop(listing);
    let run = snapshot.await.unwrap();
    let selected = &run["snapshot"]["skills"][0];
    assert_eq!(
        selected["content"], content,
        "writer must commit between listing and freezing"
    );
    assert_eq!(selected["description"], "Use the latest branch conventions");
}

/// A local GitHub serving `fixture/skills` through the bundled package and one instance.
async fn with_github() -> (Api, Github) {
    let api = Api::new().await;
    let github = Github::start(api._root.path()).await;
    github.attach(&api.service);
    common::github::instance(&api.service, "GitHub", "fixture-token", &[]).await;
    (api, github)
}

/// An import request for the repository source of the GitHub package.
fn import(location: &Value, folder: Option<&str>) -> Value {
    json!({
        "source": "plugin",
        "pluginId": "github",
        "skillSource": "repository",
        "location": location,
        "folder": folder,
    })
}

fn sha(seed: char) -> String {
    seed.to_string().repeat(40)
}

fn blob(github: &Github, id: &str, content: &str) {
    github.respond(
        &format!("repos/fixture/skills/git/blobs/{id}"),
        &json!({ "content": STANDARD.encode(content), "encoding": "base64" }),
    );
}

/// Commit `commit` whose tree holds `skills/review` with `files` and a sibling skill.
fn commit(github: &Github, commit: &str, tree: char, files: &[(&str, &str, &str)]) {
    let (top, skills, review) = (
        sha(tree),
        format!("{tree}1").repeat(20),
        format!("{tree}2").repeat(20),
    );
    github.respond(
        &format!("repos/fixture/skills/commits/{commit}"),
        &json!({ "sha": commit, "commit": { "tree": { "sha": top }, "message": format!("Release {commit}\n\nBody"), "committer": { "date": "2026-10-01T10:00:00Z" } } }),
    );
    github.respond(
        &format!("repos/fixture/skills/git/trees/{top}"),
        &json!({ "tree": [{ "path": "skills", "type": "tree", "sha": skills }] }),
    );
    github.respond(
        &format!("repos/fixture/skills/git/trees/{skills}"),
        &json!({ "tree": [{ "path": "review", "type": "tree", "sha": review }] }),
    );
    let entries = files
        .iter()
        .map(|(path, mode, id)| json!({ "path": path, "mode": mode, "type": "blob", "sha": id, "size": 20 }))
        .collect::<Vec<_>>();
    github.respond(
        &format!("repos/fixture/skills/git/trees/{review}?recursive=1"),
        &json!({ "tree": entries, "truncated": false }),
    );
    let mut everything = vec![
        json!({ "path": "skills", "type": "tree", "sha": skills }),
        json!({ "path": "skills/review", "type": "tree", "sha": review }),
        json!({ "path": "skills/triage/SKILL.md", "mode": "100644", "type": "blob", "sha": sha('f'), "size": 20 }),
    ];
    everything.extend(files.iter().map(|(path, mode, id)| {
        json!({ "path": format!("skills/review/{path}"), "mode": mode, "type": "blob", "sha": id, "size": 20 })
    }));
    github.respond(
        &format!("repos/fixture/skills/git/trees/{top}?recursive=1"),
        &json!({ "tree": everything, "truncated": false }),
    );
}

#[tokio::test]
async fn github_imports_record_their_source_and_preview_updates() {
    let (api, github) = with_github().await;
    let first = sha('1');
    let second = sha('2');
    github.respond("repos/fixture/skills", &json!({ "default_branch": "main" }));
    blob(&github, &sha('a'), SKILL);
    blob(&github, &sha('b'), "#!/bin/sh\necho v1\n");
    blob(&github, &sha('c'), "#!/bin/sh\necho v2\n");
    blob(&github, &sha('d'), "# New reference\n");
    commit(
        &github,
        &first,
        '3',
        &[
            ("SKILL.md", "100644", &sha('a')),
            ("scripts/check.sh", "100755", &sha('b')),
        ],
    );
    commit(
        &github,
        &second,
        '4',
        &[
            ("SKILL.md", "100644", &sha('a')),
            ("scripts/check.sh", "100755", &sha('c')),
            ("references/new.md", "100644", &sha('d')),
        ],
    );
    github.respond(
        "repos/fixture/skills/commits/main",
        &json!({ "sha": first, "commit": { "tree": { "sha": sha('3') }, "message": "Release 1", "committer": { "date": "2026-10-01T10:00:00Z" } } }),
    );

    let choice = api
        .ok(
            "POST",
            "/skills/global/import",
            Some(import(&json!({ "repository": "fixture/skills" }), None)),
        )
        .await;
    assert_eq!(
        choice["candidates"],
        json!(["skills/review", "skills/triage"])
    );

    let imported = api
        .ok(
            "POST",
            "/skills/global/import",
            Some(import(
                &json!({ "repository": "fixture/skills" }),
                Some("skills/review"),
            )),
        )
        .await["imported"]
        .clone();
    assert_eq!(imported["source"]["version"], first);
    assert_eq!(imported["source"]["location"]["ref"], "main");
    assert_eq!(imported["source"]["folder"], "skills/review");
    assert_eq!(imported["source"]["pluginId"], "github");
    assert_eq!(imported["source"]["modified"], false);
    assert_eq!(
        mode(&api.skill_dir("review").join("scripts/check.sh")),
        0o700
    );
    let status = api.ok("GET", "/skills/global/review/source", None).await;
    assert_eq!(status["current"], true);

    // The branch moves: the preview lists upstream changes with line diffs.
    github.respond(
        "repos/fixture/skills/commits/main",
        &json!({ "sha": second, "commit": { "tree": { "sha": sha('4') }, "message": "Release 2\n\nMore", "committer": { "date": "2026-10-02T10:00:00Z" } } }),
    );
    let status = api.ok("GET", "/skills/global/review/source", None).await;
    assert_eq!(status["current"], false);
    assert_eq!(status["latest"]["message"], "Release 2");
    assert_eq!(status["changes"][0]["path"], "references/new.md");
    assert_eq!(status["changes"][0]["status"], "added");
    assert_eq!(status["changes"][1]["path"], "scripts/check.sh");
    assert_eq!(
        status["changes"][1]["lines"],
        json!([
            { "kind": "context", "text": "#!/bin/sh" },
            { "kind": "removed", "text": "echo v1" },
            { "kind": "added", "text": "echo v2" },
        ])
    );

    // A local edit marks the source modified; updating from a stale base conflicts.
    let edited = api
        .ok(
            "POST",
            "/skills/global/review/bundle",
            Some(json!({ "base": imported["revision"], "changes": [{ "op": "write", "path": "local.md", "content": "mine" }] })),
        )
        .await;
    assert_eq!(edited["source"]["modified"], true);
    let (code, _) = api
        .call(
            "POST",
            "/skills/global/review/source/update",
            Some(json!({ "version": second, "base": imported["revision"] })),
        )
        .await;
    assert_eq!(code, StatusCode::CONFLICT);
    let updated = api
        .ok(
            "POST",
            "/skills/global/review/source/update",
            Some(json!({ "version": second, "base": edited["revision"] })),
        )
        .await;
    assert_eq!(
        paths(&updated),
        ["SKILL.md", "references/new.md", "scripts/check.sh"]
    );
    assert_eq!(updated["source"]["version"], second);
    assert_eq!(updated["source"]["modified"], false);
    let listed = api.ok("GET", "/skills", None).await;
    assert_eq!(
        listed[0]["source"]["location"]["repository"],
        "fixture/skills"
    );
    // Downloads carried the instance's token, never seen by the package.
    assert!(github.calls().iter().any(|call| {
        text(call, "path").contains("/git/blobs/") && call["token"] == "fixture-token"
    }));

    api.ok("DELETE", "/skills/global/review/source", None).await;
    let (code, _) = api.call("GET", "/skills/global/review/source", None).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert!(api.ok("GET", "/skills", None).await[0]["source"].is_null());

    let (code, error) = api
        .call(
            "POST",
            "/skills/global/import",
            Some(import(&json!({ "repository": "fixture/missing" }), None)),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_GATEWAY);
    assert!(error.to_string().contains("could not find"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_github_updates_cannot_restore_detached_or_reimported_sources() {
    use cairn_installation::skills::sources;

    for reimport in [false, true] {
        let (api, github) = with_github().await;
        let root = api._root.path().to_owned();
        let started = root.join("started");
        let released = root.join("released");
        let upstream_blob = sha('e');
        github.state(&json!({ "gate": {
            "path": format!("repos/fixture/skills/git/blobs/{upstream_blob}"),
            "started": started,
            "released": released,
        } }));
        let first = sha('a');
        let second = sha('b');
        let manifest_blob = sha('c');
        let original_blob = sha('d');
        blob(&github, &manifest_blob, SKILL);
        blob(&github, &original_blob, "Original\n");
        blob(&github, &upstream_blob, "Upstream\n");
        for (revision, tree, notes) in [
            (&first, '3', &original_blob),
            (&second, '4', &upstream_blob),
        ] {
            commit(
                &github,
                revision,
                tree,
                &[
                    ("SKILL.md", "100644", manifest_blob.as_str()),
                    ("notes.md", "100644", notes.as_str()),
                ],
            );
        }
        let request = import(
            &json!({ "repository": "fixture/skills", "ref": first }),
            Some("skills/review"),
        );
        let imported = api
            .ok("POST", "/skills/global/import", Some(request.clone()))
            .await;
        let service = api.service.clone();
        let base = imported["imported"]["revision"].clone();
        let updater = tokio::spawn(async move {
            sources::update(
                &service,
                "global",
                "review",
                &json!({
                    "base": base,
                    "version": second,
                }),
                None,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !started.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        if reimport {
            api.ok("DELETE", "/skills/global/review", None).await;
            api.ok("POST", "/skills/global/import", Some(request)).await;
        } else {
            api.ok("DELETE", "/skills/global/review/source", None).await;
        }
        let before = api.ok("GET", "/skills/global/review/bundle", None).await;
        std::fs::write(released, "release").unwrap();
        let result = updater.await.unwrap();
        let error = result.expect_err("a stale update succeeded after the source changed");
        assert_eq!(error.status, 409, "{error}");
        let after = api.ok("GET", "/skills/global/review/bundle", None).await;
        assert_eq!(after["revision"], before["revision"]);
        assert_eq!(after["source"], before["source"]);
        assert_eq!(
            std::fs::read_to_string(api.skill_dir("review").join("notes.md")).unwrap(),
            "Original\n"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_updates_record_the_upstream_revision_before_a_queued_local_edit() {
    use cairn_installation::skills::{
        bundle::{self, Change},
        sources,
    };
    let (api, github) = with_github().await;
    let root = api._root.path().to_owned();
    let first = sha('a');
    let second = sha('b');
    let manifest_blob = sha('c');
    let old_blob = sha('d');
    let upstream_blob = sha('e');
    blob(&github, &manifest_blob, SKILL);
    blob(&github, &old_blob, "Old\n");
    blob(&github, &upstream_blob, "Upstream\n");
    commit(
        &github,
        &first,
        '3',
        &[
            ("SKILL.md", "100644", manifest_blob.as_str()),
            ("notes/000.txt", "100644", old_blob.as_str()),
        ],
    );
    let names = (0..200)
        .map(|index| format!("notes/{index:03}.txt"))
        .collect::<Vec<_>>();
    let mut files = vec![("SKILL.md", "100644", manifest_blob.as_str())];
    files.extend(
        names
            .iter()
            .map(|name| (name.as_str(), "100644", upstream_blob.as_str())),
    );
    commit(&github, &second, '4', &files);
    let imported = sources::import(
        &api.service,
        "global",
        &import(
            &json!({ "repository": "fixture/skills", "ref": first }),
            Some("skills/review"),
        ),
        None,
    )
    .await
    .unwrap();

    let expected = root.join("expected");
    std::fs::create_dir_all(expected.join("notes")).unwrap();
    bundle::write_file(&expected.join("SKILL.md"), SKILL.as_bytes(), false)
        .await
        .unwrap();
    for name in &names {
        bundle::write_file(&expected.join(name), b"Upstream\n", false)
            .await
            .unwrap();
    }
    let upstream_revision = bundle::describe(&expected).await.unwrap().revision;
    let service = api.service.clone();
    let base = imported["imported"]["revision"].clone();
    let updater = tokio::spawn(async move {
        sources::update(
            &service,
            "global",
            "review",
            &json!({
                "base": base,
                "version": second,
            }),
            None,
        )
        .await
        .unwrap()
    });

    // Queue the local writer while replacement owns the write lock. FIFO lock
    // admission puts this edit ahead of any subsequent live-folder reread.
    let skills_root = api.skill_dir("review").parent().unwrap().to_owned();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let mut entries = tokio::fs::read_dir(&skills_root).await.unwrap();
            while let Some(entry) = entries.next_entry().await.unwrap() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".review.staging-")
                {
                    return;
                }
            }
            assert!(
                !updater.is_finished(),
                "Did not observe the replacement staging directory"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    api.service
        .skills
        .apply(
            "review",
            Some(&upstream_revision),
            &[Change::Write {
                path: "notes/000.txt".into(),
                bytes: b"Local\n".to_vec(),
                executable: None,
            }],
            None,
        )
        .await
        .unwrap();
    let receipt = updater.await.unwrap();
    let current = api.ok("GET", "/skills/global/review/bundle", None).await;
    assert_eq!(
        current["source"]["modified"], true,
        "Concurrent local edits must retain the overwrite warning"
    );
    assert_eq!(
        receipt["revision"], upstream_revision,
        "The update response must describe its own saved version"
    );
    assert_ne!(current["revision"], upstream_revision);
}

#[tokio::test]
async fn owner_mcp_clients_read_and_edit_multi_file_skills() {
    let api = Api::new().await;
    let token = relay_fixture::grant(&api.service, &["read", "manage"]).await;
    let mcp = |name: &str, arguments: Value| {
        let request = common::request("POST", "/mcp")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": common::mcp_arguments(name, &arguments),
                })
                .to_string(),
            ))
            .unwrap();
        let router = api.router.clone();
        async move {
            let response = common::send(&router, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            let response = common::read_json(response).await;
            assert!(response["error"].is_null(), "{response}");
            response["result"].clone()
        }
    };

    let created = mcp(
        "edit_skill_files",
        json!({
            "name": "release",
            "base": null,
            "changes": [
                { "op": "write", "path": "SKILL.md", "content": SKILL.replace("name: review", "name: release") },
                { "op": "write", "path": "scripts/release.sh", "content": "#!/bin/sh\necho release\n", "executable": true },
            ],
        }),
    )
    .await;
    let created = &created["structuredContent"]["result"];
    assert_eq!(created["files"][1]["executable"], true, "{created}");
    assert_eq!(
        mode(&api.skill_dir("release").join("scripts/release.sh")),
        0o700
    );

    let listed = mcp("read_skill", json!({ "name": "release" })).await;
    assert_eq!(
        listed["structuredContent"]["result"]["revision"], created["revision"],
        "{listed}"
    );
    let script = mcp(
        "read_skill",
        json!({ "name": "release", "path": "scripts/release.sh" }),
    )
    .await;
    assert_eq!(
        script["structuredContent"]["result"]["content"], "#!/bin/sh\necho release\n",
        "{script}"
    );
    let stale = mcp(
        "edit_skill_files",
        json!({ "name": "release", "base": "0".repeat(64), "changes": [{ "op": "delete", "path": "scripts/release.sh" }] }),
    )
    .await;
    assert_eq!(stale["isError"], true);
}

#[tokio::test]
async fn skills_written_outside_cairn_stay_editable() {
    let api = Api::new().await;
    let directory = api.skill_dir("review");
    std::fs::create_dir_all(directory.join(".venv/bin")).unwrap();
    std::fs::create_dir_all(directory.join("a")).unwrap();
    std::fs::write(directory.join("SKILL.md"), SKILL).unwrap();
    std::fs::write(directory.join("a/x.md"), "x").unwrap();
    std::fs::write(directory.join("b.md"), "b").unwrap();
    std::fs::write(directory.join("big.bin"), vec![0u8; 4 * 1024 * 1024 + 1]).unwrap();
    std::os::unix::fs::symlink("/usr/bin/python3", directory.join(".venv/bin/python")).unwrap();

    // Hidden links survive, the oversized legacy file does not block edits, and a
    // file may take the name of a folder that the same batch emptied.
    let base = api.ok("GET", "/skills/global/review/bundle", None).await["revision"].clone();
    let edited = api
        .ok(
            "POST",
            "/skills/global/review/bundle",
            Some(json!({
                "base": base,
                "changes": [
                    { "op": "move", "from": "a/x.md", "to": "z/x.md" },
                    { "op": "move", "from": "b.md", "to": "a" },
                ],
            })),
        )
        .await;
    assert_eq!(paths(&edited), ["SKILL.md", "a", "big.bin", "z/x.md"]);
    assert!(
        std::fs::symlink_metadata(directory.join(".venv/bin/python"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    api.ok(
        "PUT",
        "/skills/global/review",
        Some(json!({ "content": SKILL.replace("Run", "Then run") })),
    )
    .await;
    let (status, _) = api
        .call(
            "GET",
            "/skills/global/review/file?path=.venv/bin/python",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
