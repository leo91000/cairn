mod common;

use axum::{Router, body::Body, http::StatusCode};
use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_installation::{
    config::{Config, MAIN_AGENT_ID},
    service::Service,
    skills::zip,
    validation::text,
};
use common::{RelayContext, relay_fixture};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tempfile::TempDir;

const SKILL: &str =
    "---\nname: review\ndescription: Review the current changes\n---\nRun scripts/check.sh.\n";

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
        .task(
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
}

/// A `gh` that serves `fixture/skills` from JSON files named after endpoints.
fn fake_github(root: &Path) -> PathBuf {
    let responses = root.join("github");
    std::fs::create_dir_all(&responses).unwrap();
    let gh = root.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\nfile=\"{}/$(printf '%s' \"$4\" | tr '/?=%' '____')\"\nif [ -f \"$file\" ]; then cat \"$file\"; else echo 'gh: Not Found (HTTP 404)' >&2; exit 1; fi\n",
            responses.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    gh
}

fn respond(root: &Path, endpoint: &str, value: &Value) {
    let name = endpoint.replace(['/', '?', '=', '%'], "_");
    std::fs::write(root.join("github").join(name), value.to_string()).unwrap();
}

fn sha(seed: char) -> String {
    seed.to_string().repeat(40)
}

fn blob(root: &Path, id: &str, content: &str) {
    respond(
        root,
        &format!("repos/fixture/skills/git/blobs/{id}"),
        &json!({ "content": STANDARD.encode(content), "encoding": "base64" }),
    );
}

/// Commit `commit` whose tree holds `skills/review` with `files` and a sibling skill.
fn commit(root: &Path, commit: &str, tree: char, files: &[(&str, &str, &str)]) {
    let (top, skills, review) = (
        sha(tree),
        format!("{tree}1").repeat(20),
        format!("{tree}2").repeat(20),
    );
    respond(
        root,
        &format!("repos/fixture/skills/commits/{commit}"),
        &json!({ "sha": commit, "commit": { "tree": { "sha": top }, "message": format!("Release {commit}\n\nBody"), "committer": { "date": "2026-10-01T10:00:00Z" } } }),
    );
    respond(
        root,
        &format!("repos/fixture/skills/git/trees/{top}"),
        &json!({ "tree": [{ "path": "skills", "type": "tree", "sha": skills }] }),
    );
    respond(
        root,
        &format!("repos/fixture/skills/git/trees/{skills}"),
        &json!({ "tree": [{ "path": "review", "type": "tree", "sha": review }] }),
    );
    let entries = files
        .iter()
        .map(|(path, mode, id)| json!({ "path": path, "mode": mode, "type": "blob", "sha": id, "size": 20 }))
        .collect::<Vec<_>>();
    respond(
        root,
        &format!("repos/fixture/skills/git/trees/{review}_recursive_1"),
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
    respond(
        root,
        &format!("repos/fixture/skills/git/trees/{top}_recursive_1"),
        &json!({ "tree": everything, "truncated": false }),
    );
}

#[tokio::test]
async fn github_imports_record_their_source_and_preview_updates() {
    let root = TempDir::new().unwrap();
    let gh = fake_github(root.path());
    let api = Api::with(|config| config.gh_bin = gh.to_string_lossy().into_owned()).await;
    let first = sha('1');
    let second = sha('2');
    respond(
        root.path(),
        "repos/fixture/skills",
        &json!({ "default_branch": "main" }),
    );
    blob(root.path(), &sha('a'), SKILL);
    blob(root.path(), &sha('b'), "#!/bin/sh\necho v1\n");
    blob(root.path(), &sha('c'), "#!/bin/sh\necho v2\n");
    blob(root.path(), &sha('d'), "# New reference\n");
    commit(
        root.path(),
        &first,
        '3',
        &[
            ("SKILL.md", "100644", &sha('a')),
            ("scripts/check.sh", "100755", &sha('b')),
        ],
    );
    commit(
        root.path(),
        &second,
        '4',
        &[
            ("SKILL.md", "100644", &sha('a')),
            ("scripts/check.sh", "100755", &sha('c')),
            ("references/new.md", "100644", &sha('d')),
        ],
    );
    respond(
        root.path(),
        "repos/fixture/skills/commits/main",
        &json!({ "sha": first, "commit": { "tree": { "sha": sha('3') }, "message": "Release 1", "committer": { "date": "2026-10-01T10:00:00Z" } } }),
    );

    let choice = api
        .ok(
            "POST",
            "/skills/global/import",
            Some(json!({ "source": "github", "repository": "fixture/skills" })),
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
            Some(json!({ "source": "github", "repository": "fixture/skills", "path": "skills/review" })),
        )
        .await["imported"]
        .clone();
    assert_eq!(imported["source"]["commit"], first);
    assert_eq!(imported["source"]["ref"], "main");
    assert_eq!(imported["source"]["modified"], false);
    assert_eq!(
        mode(&api.skill_dir("review").join("scripts/check.sh")),
        0o700
    );
    let status = api.ok("GET", "/skills/global/review/source", None).await;
    assert_eq!(status["current"], true);

    // The branch moves: the preview lists upstream changes with line diffs.
    respond(
        root.path(),
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
            Some(json!({ "commit": second, "base": imported["revision"] })),
        )
        .await;
    assert_eq!(code, StatusCode::CONFLICT);
    let updated = api
        .ok(
            "POST",
            "/skills/global/review/source/update",
            Some(json!({ "commit": second, "base": edited["revision"] })),
        )
        .await;
    assert_eq!(
        paths(&updated),
        ["SKILL.md", "references/new.md", "scripts/check.sh"]
    );
    assert_eq!(updated["source"]["commit"], second);
    assert_eq!(updated["source"]["modified"], false);
    let listed = api.ok("GET", "/skills", None).await;
    assert_eq!(listed[0]["source"]["repository"], "fixture/skills");

    api.ok("DELETE", "/skills/global/review/source", None).await;
    let (code, _) = api.call("GET", "/skills/global/review/source", None).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert!(api.ok("GET", "/skills", None).await[0]["source"].is_null());

    let (code, _) = api
        .call(
            "POST",
            "/skills/global/import",
            Some(json!({ "source": "github", "repository": "fixture/missing" })),
        )
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
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
                    "params": { "name": name, "arguments": arguments },
                })
                .to_string(),
            ))
            .unwrap();
        let router = api.router.clone();
        async move {
            let response = common::send(&router, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            common::read_json(response).await["result"].clone()
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
        listed["structuredContent"]["result"]["revision"],
        created["revision"]
    );
    let script = mcp(
        "read_skill",
        json!({ "name": "release", "path": "scripts/release.sh" }),
    )
    .await;
    assert_eq!(
        script["structuredContent"]["result"]["content"],
        "#!/bin/sh\necho release\n"
    );
    let stale = mcp(
        "edit_skill_files",
        json!({ "name": "release", "base": "0".repeat(64), "changes": [{ "op": "delete", "path": "scripts/release.sh" }] }),
    )
    .await;
    assert_eq!(stale["isError"], true);
}
