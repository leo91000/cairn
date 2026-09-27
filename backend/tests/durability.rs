use leo_agent_manager::{
    auth::{Auth, digest},
    config::{Config, MAIN_AGENT_ID, id, now},
    service::Service,
    store::Store,
    validation::parse,
    vault::Vault,
};
use serde_json::{Value, json};
use tempfile::TempDir;
fn config(root: &TempDir) -> Config {
    Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().to_owned()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        setup_token: "fixture".into(),
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        gh_bin: "gh".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: String::new(),
    }
}

#[tokio::test]
async fn restart_restores_missing_chat_summaries_once_without_repeating_existing_answers() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let service = Service::new(config.clone()).await.unwrap();
    let ids = [id(), id(), id(), id(), id(), id(), id()];
    let fixtures = ids.clone();
    service.store.transaction(move |db| {
        for (index, run_id) in fixtures.iter().enumerate() {
            let status = match index { 3 => "running", 4 => "failed", _ => "succeeded" };
            db.add_run(&json!({"id":run_id,"taskId":id(),"projectId":null,"status":status,
                "createdAt":1,"finishedAt":2,"trigger":"chat","summary":if index == 5 { "x".repeat(100000) } else if index == 6 { " \n\t".into() } else { "The requested work is complete.".into() }}), None)?;
            if index == 1 || index == 2 {
                db.event(run_id, "item.completed", "The requested work is complete.", Some(&json!({
                    "type":"item.completed","item":{"id":"existing","type":"agent_message","text":"The requested work is complete.\n"}
                })))?;
            }
            if index == 5 {
                db.event(run_id, "item.completed", "", Some(&json!({"item":{"type":"agent_message","text":"x".repeat(100001)}})))?;
            }
            if index == 2 {
                db.event(run_id, "chat.user", "Check again", None)?;
            }
        }
        Ok(())
    }).await.unwrap();
    for _ in 0..2 {
        let restarted = Service::new(config.clone()).await.unwrap();
        restarted.worker.initialize(&restarted).await.unwrap();
        for (index, run_id) in ids.iter().enumerate() {
            let run_id = run_id.clone();
            let events = restarted
                .store
                .read(move |db| db.events(&run_id, 0, 100))
                .await
                .unwrap();
            let answers = events
                .iter()
                .filter(|e| e["payload"]["item"]["type"] == "agent_message")
                .count();
            assert_eq!(
                answers,
                [1, 1, 2, 0, 0, 1, 0][index],
                "case {index}: missing or duplicated summary"
            );
        }
    }
}

#[test]
fn weekly_schedules_and_dst_transitions_match_the_existing_scheduler() {
    let cases = json!([
        ["0 9 * * 1", "Europe/Paris", "2026-09-11T08:00:00Z"],
        ["30 2 * * *", "Europe/Paris", "2026-03-28T22:00:00Z"],
        ["30 2 * * *", "Europe/Paris", "2026-10-24T22:00:00Z"],
        ["0 * * * *", "America/New_York", "2026-11-01T04:00:00Z"],
        ["0 0 29 2 *", "UTC", "2026-01-01T00:00:00Z"],
        ["15 4 1 * MON", "UTC", "2026-09-11T08:00:00Z"]
    ]);
    let script = "import{CronExpressionParser}from'cron-parser';console.log(JSON.stringify(JSON.parse(process.argv[1]).map(([cron,tz,date])=>{const p=CronExpressionParser.parse(cron,{tz,currentDate:new Date(date)});return Array.from({length:5},()=>p.next().getTime())})))";
    let output = std::process::Command::new("node")
        .args(["--input-type=module", "-e", script, &cases.to_string()])
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Vec<i64>> = serde_json::from_slice(&output.stdout).unwrap();
    for (case, expected) in cases.as_array().unwrap().iter().zip(expected) {
        let time = chrono::DateTime::parse_from_rfc3339(case[2].as_str().unwrap())
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            leo_agent_manager::service::next_occurrences(
                case[0].as_str().unwrap(),
                case[1].as_str().unwrap(),
                time,
                5
            )
            .unwrap(),
            expected,
            "{case}"
        );
    }
}

#[tokio::test]
async fn restricted_agents_cannot_escalate_projects_skills_or_the_main_policy() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    tokio::fs::create_dir(&config.home).await.unwrap();
    let s = Service::new(config).await.unwrap();
    let project = s
        .project(json!({"name":"Allowed","path":root.path()}), None)
        .await
        .unwrap();
    let agent = s.agent(json!({"name":"Restricted","access":{"projects":[project["id"]],"skills":[],"mcps":[],"github":false,"sandbox":"read-only"}}),None).await.unwrap();
    assert!(leo_agent_manager::service::isolated(&agent));
    assert!(
        s.agent(
            json!({"name":"Escalation","access":{"projects":[],"github":true}}),
            None
        )
        .await
        .is_err()
    );
    assert!(
        s.agent(
            json!({"name":"Main","access":{"projects":[],"github":false}}),
            Some(MAIN_AGENT_ID)
        )
        .await
        .is_err()
    );
    assert!(
        s.task(
            json!({"name":"Foreign","agentId":agent["id"],"projectId":id(),"prompt":"no"}),
            None
        )
        .await
        .is_err()
    );
    let task = s
        .task(
            json!({"name":"Allowed","agentId":agent["id"],"prompt":"inspect","worktree":false}),
            None,
        )
        .await
        .unwrap();
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    assert!(
        leo_agent_manager::execution::prepare(&run, &s.config, None, None, None)
            .await
            .is_err(),
        "Restricted execution must never fall back without a runner"
    );
    assert!(
        leo_agent_manager::skills::parse(
            "---\nname: example\ndescription: Example\n---not-a-delimiter\n"
        )
        .is_err()
    );
    let outside = TempDir::new().unwrap();
    let link = root.path().join("escape");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    assert!(
        s.project(json!({"name":"Escape","path":link}), None)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn writes_serialize_and_failed_transactions_roll_back() {
    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    store.set("counter", json!(0), None).await.unwrap();
    let mut jobs = Vec::new();
    for _ in 0..100 {
        let store = store.clone();
        jobs.push(tokio::spawn(async move {
            store
                .transaction(|db| {
                    let n = db.kv("counter")?.unwrap().as_i64().unwrap();
                    db.set("counter", &json!(n + 1), None)
                })
                .await
                .unwrap();
        }));
    }
    for job in jobs {
        job.await.unwrap();
    }
    assert_eq!(store.kv("counter").await.unwrap(), Some(json!(100)));
    let result: leo_agent_manager::error::Result<()> = store
        .transaction(|db| {
            db.set("counter", &json!(0), None)?;
            Err(leo_agent_manager::error::Error::bad("rollback"))
        })
        .await;
    assert!(result.is_err());
    assert_eq!(store.kv("counter").await.unwrap(), Some(json!(100)));
    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(reopened.kv("counter").await.unwrap(), Some(json!(100)));
}
#[tokio::test]
async fn vault_accepts_node_ciphertext_and_authentication_rejects_tampering() {
    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    // This fixture is generated by Node's existing AES-GCM format, not by Rust.
    let script = r#"const{createCipheriv}=require('node:crypto');const{writeFileSync}=require('node:fs');const key=Buffer.alloc(32,7);writeFileSync(process.argv[1]+'/mcp-encryption-key',key);const cipher=createCipheriv('aes-256-gcm',key,Buffer.alloc(12,9));cipher.setAAD(Buffer.from('fixture'));const data=Buffer.concat([cipher.update(JSON.stringify({token:'test-only'})),cipher.final()]);process.stdout.write(Buffer.concat([Buffer.alloc(12,9),cipher.getAuthTag(),data]).toString('base64'));"#;
    let output = std::process::Command::new("node")
        .args(["-e", script, root.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let vault = Vault::new(store, root.path()).unwrap();
    let ciphertext = Value::String(String::from_utf8(output.stdout).unwrap());
    assert_eq!(
        vault.decrypt("fixture", &ciphertext).unwrap(),
        json!({"token":"test-only"})
    );
    assert!(vault.decrypt("another-record", &ciphertext).is_err());
    let mut encoded = ciphertext.as_str().unwrap().as_bytes().to_vec();
    encoded[50] = if encoded[50] == b'A' { b'B' } else { b'A' };
    assert!(
        vault
            .decrypt("fixture", &String::from_utf8(encoded).unwrap().into())
            .is_err()
    );
}
#[tokio::test]
async fn binary_vault_records_preserve_bytes_and_reject_wrong_scope_or_damage() {
    let root = TempDir::new().unwrap();
    let vault = Vault::new(Store::open(root.path()).unwrap(), root.path()).unwrap();
    for plain in [Vec::new(), (0..=255).collect::<Vec<u8>>()] {
        let encrypted = vault.encrypt_bytes("backup-block", &plain).unwrap();
        assert_eq!(encrypted.len(), plain.len() + 28);
        assert_eq!(
            vault.decrypt_bytes("backup-block", &encrypted).unwrap(),
            plain
        );
        assert!(vault.decrypt_bytes("other-block", &encrypted).is_err());
        assert_ne!(
            vault.encrypt_bytes("backup-block", &plain).unwrap(),
            encrypted
        );
        for length in [0, 12, 27, encrypted.len() - 1] {
            assert!(
                vault
                    .decrypt_bytes("backup-block", &encrypted[..length])
                    .is_err()
            );
        }
        let mut damaged = encrypted;
        *damaged.last_mut().unwrap() ^= 1;
        assert!(vault.decrypt_bytes("backup-block", &damaged).is_err());
    }
}
#[tokio::test]
async fn passwords_and_sessions_remain_compatible_with_node() {
    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    let script = "process.stdout.write(require('node:crypto').scryptSync('password-long-enough','salt-string',64).toString('hex'))";
    let output = std::process::Command::new("node")
        .args(["-e", script])
        .output()
        .unwrap();
    store
        .set(
            "admin",
            json!({"salt":"salt-string","hash":String::from_utf8(output.stdout).unwrap()}),
            None,
        )
        .await
        .unwrap();
    let auth = Auth::new(store.clone(), "http://localhost:4310".into());
    let session = auth.login("password-long-enough").await.unwrap();
    let token = session["value"].as_str().unwrap();
    assert_eq!(
        auth.read(token).await.unwrap().unwrap()["csrf"],
        session["csrf"]
    );
    assert!(auth.login("incorrect").await.is_err());
    auth.logout(token).await.unwrap();
    assert!(auth.read(token).await.unwrap().is_none());
}
#[tokio::test]
async fn refresh_rotation_and_reuse_revoke_the_entire_family() {
    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    let auth = Auth::new(store, "http://localhost:4310".into());
    let client = auth
        .register(json!({"redirect_uris":["http://localhost:9999/callback"]}))
        .await
        .unwrap();
    let verifier = "a".repeat(43);
    let authorization = json!({"client_id":client["client_id"],"redirect_uri":"http://localhost:9999/callback","response_type":"code","code_challenge_method":"S256","code_challenge":digest(&verifier),"scope":"read run"});
    let redirect = url::Url::parse(&auth.consent(authorization, true).await.unwrap()).unwrap();
    let code = redirect
        .query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap()
        .1
        .into_owned();
    let grant = auth.exchange(json!({"grant_type":"authorization_code","client_id":client["client_id"],"redirect_uri":"http://localhost:9999/callback","code":code,"code_verifier":verifier})).await.unwrap();
    let refresh = json!({"grant_type":"refresh_token","client_id":client["client_id"],"refresh_token":grant["refresh_token"],"scope":"read"});
    let rotated = auth.exchange(refresh.clone()).await.unwrap();
    assert!(
        auth.verify(rotated["access_token"].as_str().unwrap(), Some("read"))
            .await
            .is_ok()
    );
    assert!(
        auth.verify(rotated["access_token"].as_str().unwrap(), Some("run"))
            .await
            .is_err()
    );
    assert!(auth.exchange(refresh).await.is_err());
    assert!(
        auth.verify(rotated["access_token"].as_str().unwrap(), None)
            .await
            .is_err()
    );
    assert!(
        auth.verify(grant["access_token"].as_str().unwrap(), None)
            .await
            .is_err()
    );
}
#[test]
fn schemas_apply_defaults_but_reject_ambiguous_questions_and_bad_mcp() {
    let agent = parse("agent", json!({"name":" Alice ","unknown":true})).unwrap();
    assert_eq!(agent["name"], "Alice");
    assert!(agent.get("unknown").is_none());
    assert_eq!(agent["access"]["sandbox"], "yolo");
    assert!(
        parse(
            "questions",
            json!([{"id":"q","title":"One"},{"id":"q","title":"Two"}])
        )
        .is_err()
    );
    assert!(
        parse(
            "mcp",
            json!({"name":"Bad","transport":"http","url":"https://secret@example.com/mcp"})
        )
        .is_err()
    );
}
#[tokio::test]
async fn concurrent_messages_and_answers_are_idempotent_and_survive_restart() {
    let root = TempDir::new().unwrap();
    let config = config(&root);
    let service = Service::new(config.clone()).await.unwrap();
    let chat = service
        .chat_create(json!({"agentId":MAIN_AGENT_ID}))
        .await
        .unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let message = json!({"id":id(),"text":"Investigate this","mode":"queue"});
    let (a, b) = tokio::join!(
        service.chat_send(chat_id, message.clone()),
        service.chat_send(chat_id, message.clone())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let run_id = id();
    let mut chat = chat.clone();
    chat["runId"] = run_id.clone().into();
    service.store.put("chats", chat).await.unwrap();
    let run = json!({"id":run_id,"taskId":chat_id,"projectId":null,"status":"running","createdAt":now(),"trigger":"chat"});
    service
        .store
        .write(move |db| db.add_run(&run, None))
        .await
        .unwrap();
    let question = json!({"id":"a".repeat(64),"blocking":false,"fields":[{"id":"choice","title":"Which approach?","secret":true}]});
    service
        .store
        .set("push-device:fixture", json!({"createdAt":now()}), None)
        .await
        .unwrap();
    service
        .question_receive(&run_id, question.clone())
        .await
        .unwrap();
    service
        .question_receive(&run_id, question.clone())
        .await
        .unwrap();
    assert_eq!(service.store.keys("push-outbox:").await.unwrap().len(), 1);
    let question_id = question["id"].as_str().unwrap();
    let answer = json!({"id":id(),"answers":{"choice":["A private answer"]}});
    let (a, b) = tokio::join!(
        service.question_answer(chat_id, question_id, answer.clone()),
        service.question_answer(chat_id, question_id, answer.clone())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let reopened = Service::new(config).await.unwrap();
    let detail = reopened.chat_detail(chat_id).await.unwrap();
    assert_eq!(detail["messages"].as_array().unwrap().len(), 2);
    assert_eq!(detail["questions"][0]["status"], "answering");
    reopened
        .chat_acknowledge(&run_id, answer["id"].as_str().unwrap())
        .await
        .unwrap();
    reopened
        .chat_acknowledge(&run_id, answer["id"].as_str().unwrap())
        .await
        .unwrap();
    let events = reopened
        .store
        .read(move |db| db.events(&run_id, 0, 100))
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["text"], "Answered a private question.");
    assert_eq!(
        reopened.chat_detail(chat_id).await.unwrap()["questions"][0]["status"],
        "answered"
    );
}

#[tokio::test]
async fn microvm_migration_preserves_linked_worktree_commits_and_uncommitted_changes() {
    let root = TempDir::new().unwrap();
    let mut config = config(&root);
    config.runner_url = "http://runner:4311".into();
    tokio::fs::create_dir(&config.home).await.unwrap();
    let repo = root.path().join("repository");
    let old = root.path().join("old-worktree");
    let git = |directory: &std::path::Path, args: &[&str]| {
        let result = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8_lossy(&result.stdout).trim().to_owned()
    };
    tokio::fs::create_dir(&repo).await.unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.test"]);
    tokio::fs::write(repo.join("deleted"), "tracked")
        .await
        .unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "initial"]);
    git(
        &repo,
        &["worktree", "add", "-b", "feat/saved", old.to_str().unwrap()],
    );
    tokio::fs::write(old.join("committed"), "saved commit")
        .await
        .unwrap();
    git(&old, &["add", "."]);
    git(&old, &["commit", "-m", "saved work"]);
    let head = git(&old, &["rev-parse", "HEAD"]);
    tokio::fs::remove_file(old.join("deleted")).await.unwrap();
    tokio::fs::write(old.join("untracked"), "unsaved work")
        .await
        .unwrap();
    let s = Service::new(config).await.unwrap();
    let project = s
        .project(
            json!({"name":"Fixture","path":repo,"baseBranch":"main"}),
            None,
        )
        .await
        .unwrap();
    let task = s.task(json!({"name":"Migrate","agentId":MAIN_AGENT_ID,"projectId":project["id"],"prompt":"inspect"}),None).await.unwrap();
    let run = s
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let home = s
        .config
        .data_dir
        .join("runs")
        .join(run["id"].as_str().unwrap())
        .join("codex");
    tokio::fs::create_dir_all(home.join("sessions"))
        .await
        .unwrap();
    tokio::fs::write(home.join("leo-managed-auth"), "1")
        .await
        .unwrap();
    tokio::fs::write(home.join("sessions/saved.jsonl"), "saved session")
        .await
        .unwrap();
    let prepared = json!({"isolated":false,"workspaces":[{"projectId":project["id"],"path":old,"kind":"worktree"}]});
    let migrated = leo_agent_manager::execution::restore(&run, prepared, &s.config, None)
        .await
        .unwrap();
    let target = std::path::Path::new(migrated["workspaces"][0]["path"].as_str().unwrap());
    assert!(target.join(".git").is_dir());
    assert_eq!(git(target, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(target, &["branch", "--show-current"]), "feat/saved");
    assert!(!target.join("deleted").exists());
    assert_eq!(
        tokio::fs::read_to_string(target.join("untracked"))
            .await
            .unwrap(),
        "unsaved work"
    );
    assert!(
        home.parent()
            .unwrap()
            .join("home/.codex/sessions/saved.jsonl")
            .exists()
    );
    assert_eq!(git(&old, &["rev-parse", "HEAD"]), head);
}
