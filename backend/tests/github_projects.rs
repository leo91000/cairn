use leo_agent_manager::{config::Config, github_projects, service::Service};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, process::Command, sync::Arc};
use tempfile::TempDir;

async fn fixture() -> (TempDir, Arc<Service>) {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    for args in [
        vec!["init", "-b", "trunk"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "Fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&source)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    std::fs::write(
        home.join(".gitconfig"),
        format!(
            "[url \"{}\"]\n\tinsteadOf = https://github.com/fixture/repo.git\n",
            source.display()
        ),
    )
    .unwrap();
    let gh = root.path().join("gh");
    std::fs::write(&gh, r##"#!/bin/sh
case "$4" in
  user/repos*) printf '%s' '[{"full_name":"fixture/repo","name":"repo","description":null,"default_branch":"trunk","private":true,"fork":true,"owner":{"login":"fixture"},"language":"Rust","stargazers_count":42,"pushed_at":"2026-09-01T10:00:00Z"}]' ;;
  repos/fixture/repo) printf '%s' '{"name":"repo","description":"A fixture","default_branch":"trunk"}' ;;
  *) echo 'secret-must-not-leak' >&2; exit 1 ;;
esac
"##).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let s = Service::new(Config {
        data_dir: root.path().join("data"),
        home,
        workspace_roots: vec![root.path().to_owned()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        setup_token: "fixture".into(),
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        gh_bin: gh.to_string_lossy().into_owned(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    })
    .await
    .unwrap();
    (root, s)
}

#[tokio::test]
async fn lists_imports_default_branch_and_reuses_existing_project() {
    let (_root, s) = fixture().await;
    let page = github_projects::list(&s, 1).await.unwrap();
    assert_eq!(page["repositories"][0]["description"], "");
    assert_eq!(page["repositories"][0]["private"], true);
    assert_eq!(page["repositories"][0]["owner"], "fixture");
    assert_eq!(page["repositories"][0]["language"], "Rust");
    assert_eq!(page["repositories"][0]["stars"], 42);
    assert_eq!(page["repositories"][0]["fork"], true);
    assert_eq!(page["repositories"][0]["pushedAt"], "2026-09-01T10:00:00Z");
    assert_eq!(page["nextPage"], serde_json::Value::Null);
    let project = github_projects::import(
        &s,
        json!({
            "repository": "fixture/repo"
        }),
    )
    .await
    .unwrap();
    assert_eq!(project["baseBranch"], "trunk");
    assert!(
        std::path::Path::new(project["path"].as_str().unwrap())
            .join(".git")
            .is_dir()
    );
    assert_eq!(
        github_projects::list(&s, 1).await.unwrap()["repositories"][0]["imported"],
        true
    );
    assert_eq!(
        github_projects::import(
            &s,
            json!({
                "repository": "fixture/repo"
            })
        )
        .await
        .unwrap()["id"],
        project["id"]
    );
    assert_eq!(s.store.list("projects").await.unwrap().len(), 1);
}

#[tokio::test]
async fn failures_leave_no_project_or_partial_checkout_and_hide_cli_output() {
    let (root, s) = fixture().await;
    let error = github_projects::import(
        &s,
        json!({
            "repository": "fixture/missing"
        }),
    )
    .await
    .unwrap_err();
    assert!(!error.message.contains("secret-must-not-leak"));
    assert!(
        github_projects::import(
            &s,
            json!({
                "repository": "fixture/repo",
                "baseBranch": "missing"
            })
        )
        .await
        .is_err()
    );
    assert!(
        github_projects::import(
            &s,
            json!({
                "repository": "../repo"
            })
        )
        .await
        .is_err()
    );
    assert!(s.store.list("projects").await.unwrap().is_empty());
    assert!(!std::fs::read_dir(root.path()).unwrap().any(|p| {
        p.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("github-")
    }));
}
