use super::*;
use crate::config::{Config, id};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;

async fn fixture() -> (tempfile::TempDir, Arc<Service>, String, String) {
    let root = tempfile::tempdir().unwrap();
    let config: Config = serde_json::from_value(json!({
        "dataDir":root.path().join("data"),"home":root.path().join("home"),
        "workspaceRoots":[root.path()],"publicUrl":"http://localhost:4310","host":"127.0.0.1",
        "port":0,"codexBin":"codex","concurrency":1,"logger":false,"workerEnabled":false,"runnerUrl":""
    })).unwrap();
    let s = Service::new(config).await.unwrap();
    let a = s.chat_create(json!({})).await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = s.chat_create(json!({})).await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (root, s, a, b)
}

async fn source(s: &Service, chat: &str, id: &str) -> PathBuf {
    s.uploaded_attachment(chat, id)
        .await
        .unwrap()
        .path(&s.config.data_dir)
}

fn gated(s: &Arc<Service>, id: &str) -> Arc<PreparationGate> {
    let gate = Arc::new(PreparationGate::default());
    s.attachments
        .preparation
        .lock()
        .unwrap()
        .insert(id.into(), gate.clone());
    gate
}

fn upload(s: &Arc<Service>, chat: &str, id: &str) -> JoinHandle<Result<ChatAttachment>> {
    let (s, chat, id) = (s.clone(), chat.to_owned(), id.to_owned());
    tokio::spawn(async move { s.store_attachment(&chat, &id, "file.txt", b"data").await })
}

async fn settle<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("operation must make progress")
}

#[tokio::test]
async fn slow_upload_allows_other_conversations_and_files_and_identical_retry_coalesces() {
    let (_root, s, a, b) = fixture().await;
    let file = id();
    let gate = gated(&s, &file);
    let first = upload(&s, &a, &file);
    settle(gate.reached.notified()).await;
    let other_chat = settle(s.store_attachment(&b, &id(), "other.txt", b"other"))
        .await
        .unwrap();
    let same_chat = settle(s.store_attachment(&a, &id(), "another.txt", b"more"))
        .await
        .unwrap();
    assert_eq!(other_chat.chat_id, b);
    assert_eq!(same_chat.chat_id, a);
    let retry = upload(&s, &a, &file);
    gate.resume.notify_one();
    let first = settle(first).await.unwrap().unwrap();
    assert_eq!(settle(retry).await.unwrap().unwrap(), first);
    assert_eq!(
        s.store_attachment(&a, &file, "file.txt", b"changed")
            .await
            .unwrap_err()
            .status,
        409
    );
    assert_eq!(
        s.store_attachment(&a, &file, "renamed.txt", b"data")
            .await
            .unwrap_err()
            .status,
        409
    );
}

#[tokio::test]
async fn cancellation_releases_reserved_bytes_and_allows_same_identifier_to_retry() {
    let (_root, s, a, _) = fixture().await;
    let file = id();
    let gate = Arc::new(PreparationGate::default());
    s.attachments
        .writing
        .lock()
        .unwrap()
        .insert(file.clone(), gate.clone());
    let (service, chat, attachment) = (s.clone(), a.clone(), file.clone());
    let first = tokio::spawn(async move {
        service
            .store_attachment(&chat, &attachment, "file.txt", &vec![b'x'; 128 * 1024])
            .await
    });
    settle(gate.reached.notified()).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    // The retry waits for owned cleanup, then must reuse the quota and identity.
    let saved = settle(upload(&s, &a, &file)).await.unwrap().unwrap();
    assert_eq!(saved.id, file);
    assert_eq!(
        tokio::fs::read(source(&s, &a, &file).await).await.unwrap(),
        b"data"
    );
    assert_eq!(
        s.store
            .read(|db| Ok(db.0.query_row(
                "SELECT count(*) FROM attachment_reservations",
                [],
                |r| r.get::<_, u32>(0)
            )?))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn revocation_during_preparation_cannot_publish_or_keep_its_reservation() {
    let (_root, s, a, _) = fixture().await;
    let file = id();
    let gate = gated(&s, &file);
    let allowed = Arc::new(AtomicBool::new(true));
    let check = allowed.clone();
    let (service, chat, attachment) = (s.clone(), a.clone(), file.clone());
    let upload = tokio::spawn(async move {
        service
            .store_attachment_authorized(&chat, &attachment, "file.txt", b"data", move |_| {
                if check.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(Error::forbidden("Revoked"))
                }
            })
            .await
    });
    settle(gate.reached.notified()).await;
    allowed.store(false, Ordering::SeqCst);
    gate.resume.notify_one();
    assert_eq!(settle(upload).await.unwrap().unwrap_err().status, 403);
    let chat = a.clone();
    assert!(
        s.store
            .read(move |db| db.attachments(&chat))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        tokio::fs::read_dir(s.config.data_dir.join("chat-attachments").join(&a))
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        s.store_attachment(&a, &file, "file.txt", b"data")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn purge_during_upload_fences_publication_and_removes_staging_bytes() {
    let (_root, s, a, _) = fixture().await;
    let file = id();
    let gate = gated(&s, &file);
    let first = upload(&s, &a, &file);
    settle(gate.reached.notified()).await;
    s.chat_trash(&a, true).await.unwrap();
    let (service, chat) = (s.clone(), s.conversation_boundary(&a).await.unwrap());
    let purge =
        tokio::spawn(async move { crate::conversation_deletion::purge(&service, chat).await });
    gate.resume.notify_one();
    assert_eq!(settle(first).await.unwrap().unwrap_err().status, 409);
    settle(purge).await.unwrap().unwrap();
    assert!(!s.config.data_dir.join("chat-attachments").join(&a).exists());
    let chat = a.clone();
    assert!(
        s.store
            .read(move |db| db.attachments(&chat))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn abandoned_reservation_expires_and_stale_writer_cannot_overwrite_retry() {
    let (_root, s, a, _) = fixture().await;
    let file = id();
    let gate = gated(&s, &file);
    let first = upload(&s, &a, &file);
    settle(gate.reached.notified()).await;
    let token: String = s
        .store
        .read(|db| {
            Ok(db
                .0
                .query_row("SELECT token FROM attachment_reservations", [], |r| {
                    r.get(0)
                })?)
        })
        .await
        .unwrap();
    let directory = s.config.data_dir.join("chat-attachments").join(&a);
    private_dir(&directory).await.unwrap();
    let orphan = directory.join(format!("{file}.{token}"));
    tokio::fs::write(&orphan, b"crash bytes").await.unwrap();
    s.store
        .transaction(|db| {
            db.0.execute("UPDATE attachment_reservations SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    s.reconcile_attachments().await.unwrap();
    assert!(!orphan.exists());
    // An independent service/domain emulates a retry after the failed process.
    let recovered = Attachments::default();
    let recovered = Arc::new(recovered);
    let metadata = ChatAttachment {
        id: file.clone(),
        chat_id: a.clone(),
        name: "file.txt".into(),
        size: 4,
        media_type: "application/octet-stream".into(),
        kind: "file".into(),
        digest: hex::encode(Sha256::digest(b"next")),
        created_at: now(),
        width: None,
        height: None,
    };
    recovered
        .upload(
            s.store.clone(),
            s.config.data_dir.join("chat-attachments"),
            metadata,
            b"next".to_vec(),
            |_| Ok(()),
        )
        .await
        .unwrap();
    gate.resume.notify_one();
    assert!(settle(first).await.unwrap().is_err());
    assert_eq!(
        tokio::fs::read(source(&s, &a, &file).await).await.unwrap(),
        b"next"
    );
    s.reconcile_attachments().await.unwrap();
    assert_eq!(std::fs::read_dir(directory).unwrap().count(), 1);
}

#[tokio::test]
async fn failed_file_write_releases_quota_before_retry() {
    let (_root, s, a, _) = fixture().await;
    let root = s.config.data_dir.join("chat-attachments");
    private_dir(&root).await.unwrap();
    tokio::fs::write(root.join(&a), b"cannot create a directory here")
        .await
        .unwrap();
    let file = id();
    assert!(
        s.store_attachment(&a, &file, "file.txt", b"data")
            .await
            .is_err()
    );
    tokio::fs::remove_file(root.join(&a)).await.unwrap();
    assert!(
        s.store_attachment(&a, &file, "file.txt", b"data")
            .await
            .is_ok()
    );
    let chat = a.clone();
    assert_eq!(
        s.store
            .read(move |db| Ok(db.0.query_row(
                "SELECT reserved_bytes FROM attachment_quotas WHERE chat_id=?",
                [chat],
                |r| r.get::<_, u32>(0)
            )?))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn startup_reconciles_crash_files_without_removing_committed_uploads() {
    let (_root, s, a, _) = fixture().await;
    let saved = id();
    s.store_attachment(&a, &saved, "retained.txt", b"keep")
        .await
        .unwrap();
    let config = s.config.clone();
    let abandoned = id();
    let token = id();
    let metadata = ChatAttachment {
        id: abandoned.clone(),
        chat_id: a.clone(),
        name: "crash.txt".into(),
        size: 4,
        media_type: "application/octet-stream".into(),
        kind: "file".into(),
        digest: hex::encode(Sha256::digest(b"lost")),
        created_at: now(),
        width: None,
        height: None,
    };
    let claim = token.clone();
    s.store
        .transaction(move |db| {
            db.reserve_attachment(&metadata, &claim, now() + 300000)?;
            db.0.execute("UPDATE attachment_reservations SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let directory = config.data_dir.join("chat-attachments").join(&a);
    tokio::fs::write(directory.join(format!("{abandoned}.{token}")), b"lost")
        .await
        .unwrap();
    tokio::fs::write(
        directory.join(format!("{}.{}", id(), id())),
        b"orphan after crash",
    )
    .await
    .unwrap();
    drop(s);
    let recovered = Service::new(config).await.unwrap();
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    assert_eq!(
        tokio::fs::read(source(&recovered, &a, &saved).await)
            .await
            .unwrap(),
        b"keep"
    );
    assert!(
        recovered
            .store_attachment(&a, &abandoned, "crash.txt", b"next")
            .await
            .is_ok()
    );
}

async fn seed(s: &Service, chat: &str, sizes: Vec<usize>) -> Vec<String> {
    let chat = chat.to_owned();
    s.store
        .transaction(move |db| {
            let mut ids = Vec::new();
            for size in sizes {
                let file = id();
                let a = ChatAttachment {
                    id: file.clone(),
                    chat_id: chat.clone(),
                    name: "retained.txt".into(),
                    size: size as u64,
                    media_type: "application/octet-stream".into(),
                    kind: "file".into(),
                    digest: "a".repeat(64),
                    created_at: now(),
                    width: None,
                    height: None,
                };
                db.reserve_attachment(&a, &file, now() + 300000)?;
                db.commit_attachment(&a, &file)?;
                ids.push(file);
            }
            Ok(ids)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn an_inflight_reservation_counts_toward_the_chat_quota_and_cancellation_reclaims_it() {
    let (_root, s, a, b) = fixture().await;
    let mut sizes = vec![MAX_FILE; 20];
    sizes[19] -= 3;
    seed(&s, &a, sizes).await;
    let first = id();
    let gate = gated(&s, &first);
    let (service, chat, file) = (s.clone(), a.clone(), first.clone());
    let upload = tokio::spawn(async move {
        service
            .store_attachment(&chat, &file, "one.txt", b"12")
            .await
    });
    settle(gate.reached.notified()).await;
    let second = id();
    assert_eq!(
        s.store_attachment(&a, &second, "two.txt", b"34")
            .await
            .unwrap_err()
            .status,
        413
    );
    assert!(
        s.store_attachment(&b, &id(), "free.txt", b"34")
            .await
            .is_ok()
    );
    upload.abort();
    assert!(upload.await.unwrap_err().is_cancelled());
    // An identical retry waits for cancellation cleanup and then uses the same two bytes.
    assert!(
        settle(s.store_attachment(&a, &first, "one.txt", b"12"))
            .await
            .is_ok()
    );
    assert_eq!(
        s.store_attachment(&a, &second, "two.txt", b"34")
            .await
            .unwrap_err()
            .status,
        413
    );
}

#[tokio::test]
async fn message_selection_keeps_eight_files_forty_mib_scope_and_duplicate_limits() {
    let (_root, s, a, b) = fixture().await;
    let mut sizes = vec![5 * 1024 * 1024; 9];
    sizes.push(MAX_FILE);
    let ids = seed(&s, &a, sizes).await;
    s.store
        .read(move |db| {
            let mut exact = json!({"text":"","attachmentIds":ids[..8]});
            crate::attachments::message(db, &a, &mut exact)?;
            assert_eq!(exact["attachments"].as_array().unwrap().len(), 8);
            for (selection, error) in [
                (ids[..9].to_vec(), "8 files"),
                ([ids[..7].to_vec(), vec![ids[9].clone()]].concat(), "40 MB"),
                (vec![ids[0].clone(), ids[0].clone()], "Duplicate attachment"),
            ] {
                let failure = crate::attachments::message(
                    db,
                    &a,
                    &mut json!({"text":"","attachmentIds":selection}),
                )
                .unwrap_err();
                assert_eq!(failure.status, 400);
                assert!(failure.message.contains(error), "{failure}");
            }
            assert_eq!(
                crate::attachments::message(
                    db,
                    &b,
                    &mut json!({"text":"","attachmentIds":[ids[0]]})
                )
                .unwrap_err()
                .status,
                404
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn cancelling_a_plugin_preparation_prevents_its_upload_publication() {
    use crate::plugins::operations::{Audience, Operations};
    let (_root, s, a, _) = fixture().await;
    let operations = Operations::default();
    let file = id();
    let gate = gated(&s, &file);
    let (service, chat, attachment) = (s.clone(), a.clone(), file.clone());
    let operation = operations
        .start(s.clone(), Audience::Owner, async move {
            service
                .store_attachment(&chat, &attachment, "plugin.txt", b"data")
                .await?;
            Err(Error::internal("Unexpected publication"))
        })
        .unwrap();
    settle(gate.reached.notified()).await;
    operations
        .cancel(operation["operation"].as_str().unwrap())
        .unwrap();
    gate.resume.notify_one();
    // A retry with different bytes proves the cancelled upload never committed.
    assert!(
        settle(s.store_attachment(&a, &file, "plugin.txt", b"replacement"))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn attachment_publication_rechecks_delegation_after_preparation() {
    use crate::agent_fixture::AgentFixture;
    use crate::plugin_grant_fixture::{Fixture as GrantFixture, contribution, support};
    use crate::validation::text;

    let f = GrantFixture::new().await;
    f.marker("live-parent").await;
    f.set(json!([contribution("tool", "grant_probe_post", support())]))
        .await;
    let child =
        f.s.agent_fixture(
            json!({ "name": "Child", "access": { "installation": true } }),
            None,
        )
        .await
        .unwrap();
    let child_id = text(&child, "id");
    f.set_for(
        child_id,
        json!([contribution(
            "tool",
            "grant_probe_post",
            json!([{ "resourceType": "thread", "id": "support-thread" }])
        )]),
    )
    .await;
    let (chat, bearer) = f.delegated(crate::config::MAIN_AGENT_ID, child_id).await;
    let authorize = crate::installation_mcp::management_authorization(&f.s, Some(&bearer))
        .await
        .unwrap();
    let file = id();
    let gate = gated(&f.s, &file);
    let service = f.s.clone();
    let (conversation, attachment) = (chat.clone(), file.clone());
    let upload = tokio::spawn(async move {
        service
            .store_attachment_live(&conversation, &attachment, "file.txt", b"data", authorize)
            .await
    });
    settle(gate.reached.notified()).await;
    f.moved_parent.store(true, Ordering::SeqCst);
    gate.resume.notify_one();
    let error = settle(upload).await.unwrap().unwrap_err();
    assert_eq!(error.status, 403);
    f.s.store
        .read(move |db| {
            assert!(db.uploaded_attachment(&chat, &file)?.is_none());
            assert_eq!(
                db.0.query_row("SELECT COUNT(*) FROM attachment_reservations", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    f.s.shutdown.cancel();
}
