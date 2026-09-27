use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use leo_agent_manager::{config::Config, http::router, service::Service};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

struct App {
    _root: TempDir,
    router: Router,
    service: Arc<Service>,
    cookie: String,
    csrf: String,
}
impl App {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        let service = Service::new(Config {
            data_dir: root.path().join("data"),
            home: root.path().join("home"),
            workspace_roots: vec![root.path().to_owned()],
            public_url: "http://localhost:4310".into(),
            host: "127.0.0.1".into(),
            port: 0,
            setup_token: "test-setup".into(),
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            gh_bin: "gh".into(),
            concurrency: 1,
            logger: false,
            worker_enabled: false,
            runner_url: String::new(),
        })
        .await
        .unwrap();
        let router = router(service.clone()).await.unwrap();
        let session = service.auth.session().await.unwrap();
        Self {
            _root: root,
            router,
            service,
            cookie: format!("leo_session={}", session["value"].as_str().unwrap()),
            csrf: session["csrf"].as_str().unwrap().into(),
        }
    }
    async fn request(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "localhost:4310")
                    .header("content-type", "application/json")
                    .header("cookie", &self.cookie)
                    .header("x-csrf-token", &self.csrf)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}

#[tokio::test]
async fn deleting_a_conversation_moves_it_to_trash_and_recovery_preserves_it() {
    let app = App::new().await;
    let (status, chat) = app.request("POST", "/api/chats", json!({})).await;
    assert_eq!(status, 200);
    let path = format!("/api/chats/{}", chat["id"].as_str().unwrap());
    let (status, deleted) = app.request("DELETE", &path, json!({})).await;
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(deleted["lifecycle"], "trash");
    assert_eq!(
        deleted["purgeAt"].as_i64().unwrap() - deleted["trashedAt"].as_i64().unwrap(),
        30 * 86_400_000
    );
    let (_, active) = app.request("GET", "/api/chats", Value::Null).await;
    assert!(active.as_array().unwrap().is_empty());
    let (_, trash) = app
        .request("GET", "/api/chats?view=trash", Value::Null)
        .await;
    assert_eq!(trash[0]["id"], chat["id"]);
    let (status, restored) = app
        .request("POST", &format!("{path}/restore"), json!({}))
        .await;
    assert_eq!(status, 200, "{restored}");
    assert_eq!(restored["lifecycle"], "active");
    assert!(restored["archiveNotBefore"].as_i64().unwrap() > leo_agent_manager::config::now());
    let (_, active) = app.request("GET", "/api/chats", Value::Null).await;
    assert_eq!(active[0]["id"], chat["id"]);
}

#[tokio::test]
async fn trash_requires_confirmation_for_pending_work_and_never_replays_cancelled_messages() {
    let app = App::new().await;
    let (_, chat) = app.request("POST", "/api/chats", json!({})).await;
    let path = format!("/api/chats/{}", chat["id"].as_str().unwrap());
    let message = json!({"id":leo_agent_manager::config::id(),"text":"Keep these instructions, do not execute them after recovery"});
    assert_eq!(
        app.request("POST", &format!("{path}/messages"), message)
            .await
            .0,
        200
    );
    assert_eq!(app.request("DELETE", &path, json!({})).await.0, 409);
    assert_eq!(
        app.request("DELETE", &path, json!({"confirm":true}))
            .await
            .0,
        200
    );
    assert_eq!(
        app.request(
            "POST",
            &format!("{path}/messages"),
            json!({"id":leo_agent_manager::config::id(),"text":"Must not dispatch"})
        )
        .await
        .0,
        409
    );
    assert_eq!(
        app.request("POST", &format!("{path}/pause"), json!({"paused":false}))
            .await
            .0,
        409
    );
    let (_, hidden) = app.request("GET", &path, Value::Null).await;
    assert!(hidden["messages"].as_array().unwrap().is_empty());
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        200
    );
    let (_, restored) = app.request("GET", &path, Value::Null).await;
    assert_eq!(restored["messages"][0]["status"], "cancelled");
    assert_eq!(
        restored["messages"][0]["text"],
        "Keep these instructions, do not execute them after recovery"
    );
    app.service.chat_tick(&Default::default()).await.unwrap();
    let (_, after) = app.request("GET", &path, Value::Null).await;
    assert!(after["run"].is_null());
    assert_eq!(after["messages"][0]["status"], "cancelled");
}

#[tokio::test]
async fn retention_policy_previews_old_conversations_but_protects_pending_work() {
    let app = App::new().await;
    for pending in [false, true] {
        let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
        if pending {
            let path = format!("/api/chats/{}/messages", chat["id"].as_str().unwrap());
            app.request(
                "POST",
                &path,
                json!({"id":leo_agent_manager::config::id(),"text":"Still waiting"}),
            )
            .await;
        }
        chat["updatedAt"] = (leo_agent_manager::config::now() - 40 * 86_400_000_i64).into();
        let pause = format!("/api/chats/{}/pause", chat["id"].as_str().unwrap());
        app.service.store.put("chats", chat).await.unwrap();
        assert_eq!(
            app.request("POST", &pause, json!({"paused":true})).await.0,
            200
        );
    }
    let (status, preview) = app
        .request("GET", "/api/conversation-retention", Value::Null)
        .await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["eligible"], 1);
    assert_eq!(preview["enabled"], false);
    assert_eq!(preview["inactivityDays"], 30);
    assert_eq!(preview["coldAfterDays"], 90);
    assert_eq!(
        app.request(
            "PUT",
            "/api/conversation-retention",
            json!({"enabled":false,"inactivityDays":0,"coldAfterDays":90})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        app.request(
            "PUT",
            "/api/conversation-retention",
            json!({"enabled":false,"inactivityDays":60,"coldAfterDays":180})
        )
        .await
        .0,
        200
    );
    let (_, preview) = app
        .request("GET", "/api/conversation-retention", Value::Null)
        .await;
    assert_eq!(preview["eligible"], 0);
}

#[tokio::test]
async fn trash_denies_direct_run_history_and_artifact_access() {
    let app = App::new().await;
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    let run = leo_agent_manager::config::id();
    let owned = run.clone();
    app.service.store.transaction(move |db| {
        db.0.execute("INSERT INTO runs(id,task_id,project_id,status,created_at,data) VALUES(?1,?1,'','succeeded',0,?2)", rusqlite::params![owned,json!({"id":owned,"status":"succeeded"}).to_string()])?;
        db.event(&owned, "chat.user", "Private transcript", None)?;
        Ok(())
    }).await.unwrap();
    chat["runId"] = run.clone().into();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    let path = format!("/api/chats/{}", chat["id"].as_str().unwrap());
    assert_eq!(app.request("DELETE", &path, json!({})).await.0, 200);
    for suffix in ["", "/events", "/history", "/artifacts"] {
        let (status, _) = app
            .request("GET", &format!("/api/runs/{run}{suffix}"), Value::Null)
            .await;
        assert_eq!(status, 409, "run{suffix} must be inaccessible in trash");
    }
}

#[tokio::test]
async fn complete_archive_round_trip_preserves_history_and_files_and_requires_explicit_restore() {
    let app = App::new().await;
    configure_archive(&app);
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    chat["updatedAt"] = (leo_agent_manager::config::now() - 40 * 86_400_000_i64).into();
    let path = format!("/api/chats/{}", chat["id"].as_str().unwrap());
    let run = leo_agent_manager::config::id();
    let owned = run.clone();
    app.service.store.transaction(move |db| {
        db.0.execute("INSERT INTO runs(id,task_id,project_id,status,created_at,data) VALUES(?1,?1,'','succeeded',0,?2)",rusqlite::params![owned,json!({"id":owned,"status":"succeeded","summary":"Saved result","sessionId":"native-session"}).to_string()])?;
        db.event(&owned,"chat.user","Original conversation",None)?;
        Ok(())
    }).await.unwrap();
    chat["runId"] = run.clone().into();
    let workspace = app
        .service
        .config
        .data_dir
        .join("runs")
        .join(&run)
        .join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("unpublished.txt"), "Unsaved agent work").unwrap();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    let (status, response) = app
        .request(
            "PUT",
            "/api/conversation-retention",
            json!({"enabled":true,"inactivityDays":30,"coldAfterDays":90,"confirmExisting":true}),
        )
        .await;
    assert_eq!(status, 200, "{response}");
    app.service.retention_tick().await.unwrap();
    let (_, archived) = app.request("GET", &path, Value::Null).await;
    assert_eq!(archived["lifecycle"], "archived", "{archived}");
    assert!(!workspace.exists());
    let (_, archives) = app
        .request("GET", "/api/chats?view=archives", Value::Null)
        .await;
    assert_eq!(archives[0]["id"], chat["id"]);
    assert_eq!(
        app.request(
            "POST",
            &format!("{path}/messages"),
            json!({"id":leo_agent_manager::config::id(),"text":"No implicit restore"})
        )
        .await
        .0,
        409
    );
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        200
    );
    app.service.retention_tick().await.unwrap();
    let (_, restored) = app.request("GET", &path, Value::Null).await;
    assert_eq!(restored["lifecycle"], "active", "{restored}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("unpublished.txt")).unwrap(),
        "Unsaved agent work"
    );
    let (_, events) = app
        .request("GET", &format!("/api/runs/{run}/events"), Value::Null)
        .await;
    assert!(events.to_string().contains("Original conversation"));
    assert_eq!(restored["run"]["sessionId"], "native-session");
    assert!(restored["archiveNotBefore"].as_i64().unwrap() > leo_agent_manager::config::now());
    // Remote cleanup can still be pending immediately after a successful restore.
    // Trash recovery must nevertheless return to the already-restored active state.
    assert_eq!(app.request("DELETE", &path, json!({})).await.0, 200);
    let (_, recovered) = app
        .request("POST", &format!("{path}/restore"), json!({}))
        .await;
    assert_eq!(recovered["lifecycle"], "active");
}

#[tokio::test]
async fn expired_trash_is_purged_but_unexpired_trash_can_be_recovered() {
    let app = App::new().await;
    let (_, chat) = app.request("POST", "/api/chats", json!({})).await;
    let path = format!("/api/chats/{}", chat["id"].as_str().unwrap());
    let (_, mut trash) = app.request("DELETE", &path, json!({})).await;
    app.service.retention_tick().await.unwrap();
    assert_eq!(app.request("GET", &path, Value::Null).await.0, 200);
    trash["purgeAt"] = 1.into();
    app.service.store.put("chats", trash).await.unwrap();
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        410
    );
    app.service.retention_tick().await.unwrap();
    assert_eq!(app.request("GET", &path, Value::Null).await.0, 404);
}

#[tokio::test]
async fn incompatible_restored_session_requires_consent_and_never_restarts_on_its_own() {
    let app = App::new().await;
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    let run = leo_agent_manager::config::id();
    chat["runId"] = run.clone().into();
    chat["restoredAt"] = 10.into();
    chat["paused"] = true.into();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    let owned = run.clone();
    app.service.store.transaction(move |db| {
        db.0.execute("INSERT INTO runs(id,task_id,project_id,status,created_at,data) VALUES(?1,?1,'','failed',0,?2)",rusqlite::params![owned,json!({"id":owned,"status":"failed","sessionId":"old-native"}).to_string()])?;
        db.set(&format!("run-checkpoint:{owned}"),&json!({"prepared":{"workspace":"preserved"},"launched":true}),None)?;
        db.event(&owned,"chat.user","Context to preserve",None)?;
        Ok(())
    }).await.unwrap();
    let path = format!("/api/chats/{}/new-session", chat["id"].as_str().unwrap());
    assert_eq!(app.request("POST", &path, json!({})).await.0, 409);
    assert_eq!(
        app.request("POST", &path, json!({"confirm":true})).await.0,
        200
    );
    let pause = format!("/api/chats/{}/pause", chat["id"].as_str().unwrap());
    assert_eq!(
        app.request("POST", &pause, json!({"paused":false})).await.0,
        200
    );
    app.service.chat_tick(&Default::default()).await.unwrap();
    let (_, run) = app
        .request("GET", &format!("/api/runs/{run}"), Value::Null)
        .await;
    assert_ne!(run["status"], "queued");
    assert_ne!(run["status"], "running");
    assert!(run["sessionId"].is_null());
}

fn configure_archive(app: &App) -> std::path::PathBuf {
    configure_archive_with(app, json!({}))
}

fn configure_archive_with(app: &App, settings: Value) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let binary = app._root.path().join("aws-fixture");
    let objects = app._root.path().join("objects");
    std::fs::create_dir_all(&objects).unwrap();
    let script = r#"#!/usr/bin/env python3
import sys,json,pathlib,shutil
args=sys.argv[1:]
root=pathlib.Path(ARCHIVE_ROOT)
compatible=(root/'compatible').exists()
if compatible:
 with open(root/'calls','a') as log: log.write(json.dumps(args)+'\n')
 if '--endpoint-url' in args: i=args.index('--endpoint-url'); args=args[:i]+args[i+2:]
def obj(uri): return root / uri.split('/',3)[3]
def unsupported(op): sys.stderr.write(f'An error occurred (NotImplemented) when calling the {op} operation: Not implemented.'); sys.exit(254)
if args[:2]==['s3api','get-public-access-block']:
 if compatible: unsupported('GetPublicAccessBlock')
 print(json.dumps({'PublicAccessBlockConfiguration':dict.fromkeys(['BlockPublicAcls','IgnorePublicAcls','BlockPublicPolicy','RestrictPublicBuckets'],True)}))
elif args[:2]==['s3api','get-bucket-acl']:
 grants=[{'Grantee':{'Type':'CanonicalUser'},'Permission':'FULL_CONTROL'}]
 if (root/'public-acl').exists(): grants.append({'Grantee':{'Type':'Group','URI':'http://acs.amazonaws.com/groups/global/AllUsers'},'Permission':'READ'})
 print(json.dumps({'Grants':grants}))
elif args[:2]==['s3api','get-bucket-policy']: unsupported('GetBucketPolicy')
elif args[:2]==['s3','cp']:
 src,dst=args[2:4]; src=obj(src) if src.startswith('s3://') else pathlib.Path(src); dst=obj(dst) if dst.startswith('s3://') else pathlib.Path(dst)
 dst.parent.mkdir(parents=True,exist_ok=True)
 if src != dst: shutil.copyfile(src,dst)
 if args[3].startswith('s3://') and (root/'corrupt-upload').exists(): dst.write_bytes(b'corrupt')
 if '--storage-class' in args: (root/'cold').write_text(args[args.index('--storage-class')+1])
elif args[:2]==['s3api','head-object']:
 value={}
 if (root/'cold').exists():
  value['StorageClass']=(root/'cold').read_text() or 'GLACIER'
  if (root/'ready').exists(): value['Restore']='ongoing-request="false"'
  elif (root/'requested').exists(): value['Restore']='ongoing-request="true"'
 print(json.dumps(value))
elif args[:2]==['s3api','restore-object']: (root/'requested').touch()
elif args[:2] in [['s3api','list-object-versions'],['s3api','list-multipart-uploads']]: print('{}')
elif args[:2]==['s3','rm']: shutil.rmtree(obj(args[2]),ignore_errors=True)
"#.replace("ARCHIVE_ROOT", &serde_json::to_string(objects.to_str().unwrap()).unwrap());
    std::fs::write(&binary, script).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(app.service.config.data_dir.join("archive-s3.json"), {
        let mut config = json!({"bucket":"fixture-bucket","awsBinary":binary});
        config
            .as_object_mut()
            .unwrap()
            .extend(settings.as_object().unwrap().clone());
        config.to_string()
    })
    .unwrap();
    objects
}

#[tokio::test]
async fn corrupt_transfer_preserves_local_data_and_retries_after_restart() {
    let app = App::new().await;
    let objects = configure_archive(&app);
    std::fs::write(objects.join("corrupt-upload"), "").unwrap();
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    let cid = chat["id"].as_str().unwrap().to_owned();
    chat["updatedAt"] = 1.into();
    app.service.store.put("chats", chat).await.unwrap();
    let attachment = app
        .service
        .config
        .data_dir
        .join("chat-attachments")
        .join(&cid);
    std::fs::create_dir_all(&attachment).unwrap();
    std::fs::write(
        attachment.join("saved.txt"),
        "Must survive failed verification",
    )
    .unwrap();
    assert_eq!(
        app.request(
            "PUT",
            "/api/conversation-retention",
            json!({"enabled":true,"inactivityDays":30,"coldAfterDays":90,"confirmExisting":true})
        )
        .await
        .0,
        200
    );
    app.service.retention_tick().await.unwrap();
    let (_, failed) = app
        .request("GET", &format!("/api/chats/{cid}"), Value::Null)
        .await;
    assert_eq!(failed["lifecycle"], "archiving");
    assert!(
        failed["lifecycleError"]
            .as_str()
            .unwrap()
            .contains("verification")
    );
    assert_eq!(
        std::fs::read_to_string(attachment.join("saved.txt")).unwrap(),
        "Must survive failed verification"
    );
    std::fs::remove_file(objects.join("corrupt-upload")).unwrap();
    let mut persisted = app.service.get("chats", &cid).await.unwrap();
    persisted["retryAfter"] = 0.into();
    app.service.store.put("chats", persisted).await.unwrap();
    let restarted = Service::new(app.service.config.clone()).await.unwrap();
    restarted.retention_tick().await.unwrap();
    let (_, archived) = app
        .request("GET", &format!("/api/chats/{cid}"), Value::Null)
        .await;
    assert_eq!(archived["lifecycle"], "archived", "{archived}");
    assert!(!attachment.exists());
}

#[tokio::test]
async fn cold_restore_waits_and_trash_recovery_does_not_request_glacier_retrieval() {
    let app = App::new().await;
    let objects = configure_archive(&app);
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    let cid = chat["id"].as_str().unwrap().to_owned();
    let path = format!("/api/chats/{cid}");
    chat["updatedAt"] = 1.into();
    app.service.store.put("chats", chat).await.unwrap();
    assert_eq!(
        app.request(
            "PUT",
            "/api/conversation-retention",
            json!({"enabled":true,"inactivityDays":30,"coldAfterDays":90,"confirmExisting":true})
        )
        .await
        .0,
        200
    );
    app.service.retention_tick().await.unwrap();
    let mut archived = app.service.get("chats", &cid).await.unwrap();
    assert_eq!(archived["lifecycle"], "archived", "{archived}");
    archived["archivedAt"] = 1.into();
    app.service.store.put("chats", archived).await.unwrap();
    app.service.retention_tick().await.unwrap();
    assert!(objects.join("cold").exists());
    assert_eq!(app.request("DELETE", &path, json!({})).await.0, 200);
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        200
    );
    let (_, recovered) = app.request("GET", &path, Value::Null).await;
    assert_eq!(recovered["lifecycle"], "archived");
    assert!(!objects.join("requested").exists());
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        200
    );
    app.service.retention_tick().await.unwrap();
    assert!(objects.join("requested").exists());
    let (_, waiting) = app.request("GET", &path, Value::Null).await;
    assert_eq!(waiting["lifecycle"], "restoring");
    // A cold retrieval must not monopolize the scheduler while AWS prepares it.
    let (_, mut other) = app.request("POST", "/api/chats", json!({})).await;
    other["updatedAt"] = 1.into();
    app.service.store.put("chats", other.clone()).await.unwrap();
    app.service.retention_tick().await.unwrap();
    assert_eq!(
        app.request(
            "GET",
            &format!("/api/chats/{}", other["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1["lifecycle"],
        "archived"
    );
    // Deletion takes precedence over a pending retrieval, even after recovery.
    assert_eq!(app.request("DELETE", &path, json!({})).await.0, 200);
    std::fs::write(objects.join("ready"), "").unwrap();
    app.service.retention_tick().await.unwrap();
    assert_eq!(
        app.request("GET", &path, Value::Null).await.1["lifecycle"],
        "trash"
    );
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .1["lifecycle"],
        "archived"
    );
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        200
    );
    app.service.retention_tick().await.unwrap();
    assert_eq!(
        app.request("GET", &path, Value::Null).await.1["lifecycle"],
        "active"
    );
}

#[tokio::test]
async fn public_deliverable_stays_warm_when_archived_and_trash_revocation_is_permanent() {
    let app = App::new().await;
    configure_archive(&app);
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    let run = leo_agent_manager::config::id();
    let artifact = leo_agent_manager::config::id();
    chat["runId"] = run.clone().into();
    chat["updatedAt"] = 1.into();
    app.service.store.put("chats", chat.clone()).await.unwrap();
    let owned = run.clone();
    let aid = artifact.clone();
    app.service.store.transaction(move |db| {
        db.0.execute("INSERT INTO runs(id,task_id,project_id,status,created_at,data) VALUES(?1,?1,'','succeeded',0,?2)",rusqlite::params![owned,json!({"id":owned,"status":"succeeded"}).to_string()])?;
        db.set(&format!("artifact:{owned}:{aid}"),&json!({"id":aid,"runId":owned,"name":"result.txt","mediaType":"text/plain","size":13,"visibility":"private"}),None)?;
        Ok(())
    }).await.unwrap();
    std::fs::create_dir_all(app.service.config.data_dir.join("artifacts")).unwrap();
    std::fs::write(
        app.service
            .config
            .data_dir
            .join("artifacts")
            .join(&artifact),
        "Public result",
    )
    .unwrap();
    let visibility = format!("/api/runs/{run}/artifacts/{artifact}/visibility");
    let (status, shared) = app
        .request("PUT", &visibility, json!({"visibility":"public"}))
        .await;
    assert_eq!(status, 200);
    let public_path = format!(
        "/api/public/artifacts/{}",
        shared["publicToken"].as_str().unwrap()
    );
    async fn public_status(app: &App, path: &str) -> u16 {
        app.router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "localhost:4310")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
            .as_u16()
    }
    assert_eq!(public_status(&app, &public_path).await, 200);
    assert_eq!(
        app.request(
            "PUT",
            "/api/conversation-retention",
            json!({"enabled":true,"inactivityDays":30,"coldAfterDays":90,"confirmExisting":true})
        )
        .await
        .0,
        200
    );
    app.service.retention_tick().await.unwrap();
    assert_eq!(public_status(&app, &public_path).await, 200);
    let path = format!("/api/chats/{}", chat["id"].as_str().unwrap());
    assert_eq!(app.request("DELETE", &path, json!({})).await.0, 200);
    assert_eq!(public_status(&app, &public_path).await, 404);
    assert_eq!(
        app.request("PUT", &visibility, json!({"visibility":"public"}))
            .await
            .0,
        409
    );
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        200
    );
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        200
    );
    app.service.retention_tick().await.unwrap();
    assert_eq!(public_status(&app, &public_path).await, 404);
}

#[tokio::test]
async fn s3_compatible_provider_archives_to_its_cold_class_through_its_endpoint() {
    let app = App::new().await;
    let settings = json!({"endpoint":"https://s3.eu-west-par.io.cloud.ovh.net","coldStorageClass":"DEEP_ARCHIVE"});
    let objects = configure_archive_with(&app, settings);
    // OVHcloud implements neither public access blocks nor bucket policies.
    std::fs::write(objects.join("compatible"), "").unwrap();
    let retention =
        json!({"enabled":true,"inactivityDays":30,"coldAfterDays":90,"confirmExisting":true});
    std::fs::write(objects.join("public-acl"), "").unwrap();
    assert_eq!(
        app.request("PUT", "/api/conversation-retention", retention.clone())
            .await
            .0,
        400
    );
    std::fs::remove_file(objects.join("public-acl")).unwrap();
    assert_eq!(
        app.request("PUT", "/api/conversation-retention", retention)
            .await
            .0,
        200
    );
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    let cid = chat["id"].as_str().unwrap().to_owned();
    let path = format!("/api/chats/{cid}");
    chat["updatedAt"] = 1.into();
    app.service.store.put("chats", chat).await.unwrap();
    app.service.retention_tick().await.unwrap();
    let mut archived = app.service.get("chats", &cid).await.unwrap();
    assert_eq!(archived["lifecycle"], "archived", "{archived}");
    archived["archivedAt"] = 1.into();
    app.service.store.put("chats", archived).await.unwrap();
    app.service.retention_tick().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(objects.join("cold")).unwrap(),
        "DEEP_ARCHIVE"
    );
    assert_eq!(
        app.request("POST", &format!("{path}/restore"), json!({}))
            .await
            .0,
        200
    );
    app.service.retention_tick().await.unwrap();
    assert!(objects.join("requested").exists());
    std::fs::write(objects.join("ready"), "").unwrap();
    // Retrieval is polled once a minute.
    let mut waiting = app.service.get("chats", &cid).await.unwrap();
    waiting["retryAfter"] = 0.into();
    app.service.store.put("chats", waiting).await.unwrap();
    app.service.retention_tick().await.unwrap();
    assert_eq!(
        app.request("GET", &path, Value::Null).await.1["lifecycle"],
        "active"
    );
    let calls = std::fs::read_to_string(objects.join("calls")).unwrap();
    assert!(calls.lines().count() > 5);
    for call in calls.lines() {
        assert!(
            call.contains(r#""--endpoint-url", "https://s3.eu-west-par.io.cloud.ovh.net""#),
            "{call}"
        );
    }
}

#[tokio::test]
async fn transparent_storage_keeps_idle_conversations_active_unless_auto_archive_is_explicit() {
    let app = App::new().await;
    let (_, mut chat) = app.request("POST", "/api/chats", json!({})).await;
    chat["updatedAt"] = (leo_agent_manager::config::now() - 40 * 86_400_000_i64).into();
    app.service.store.put("chats", chat).await.unwrap();
    let (_, before) = app
        .request("GET", "/api/conversation-retention", Value::Null)
        .await;
    assert_eq!(before["eligible"], 1);
    let node = json!({"id":leo_agent_manager::nodes::LOCAL_NODE_ID,"storage":{"enabled":true,"automaticArchiving":false}});
    app.service.store.put("nodes", node.clone()).await.unwrap();
    let (_, cached) = app
        .request("GET", "/api/conversation-retention", Value::Null)
        .await;
    assert_eq!(cached["eligible"], 0);
    let mut node = node;
    node["storage"]["automaticArchiving"] = true.into();
    app.service.store.put("nodes", node).await.unwrap();
    let (_, explicit) = app
        .request("GET", "/api/conversation-retention", Value::Null)
        .await;
    assert_eq!(explicit["eligible"], 1);
}
