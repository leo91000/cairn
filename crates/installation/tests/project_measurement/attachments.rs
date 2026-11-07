//! D2 reuses A1/A2 clocks, distributions, connections and alternating layout runs.
//! Frozen upload mapping: b9cd3c2a (v16), no production legacy reader.
use super::*;
use cairn_contracts::ChatAttachment;
use cairn_installation::{attachments::Reservation, service::Service};
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[path = "../common/mod.rs"]
mod common;

const CHATS: [&str; 2] = [
    "00000000-0000-4000-8000-000000001001",
    "00000000-0000-4000-8000-000000001002",
];

fn chat(id: &str) -> &'static str {
    CHATS[(uuid::Uuid::parse_str(id).unwrap().as_u128() & 1) as usize]
}

fn key(chat: &str, id: &str) -> String {
    format!("chat-attachment:{chat}:{id}")
}

fn previous_schema(db: &Connection) {
    db.execute_batch(
        "DROP TABLE attachment_reservations; DROP TABLE attachments; DROP TABLE attachment_quotas;
DROP TABLE kv; CREATE TABLE kv(key TEXT PRIMARY KEY,data TEXT NOT NULL,expires INTEGER);",
    )
    .unwrap();
}

impl StorageEntity for ChatAttachment {
    const KIND: &'static str = "attachments";
    const TABLE: &'static str = "attachments";
    const SAVE: &'static str = "attachment.saved";

    fn id(&self) -> String {
        self.id.clone()
    }

    fn prepare(db: &Db<'_>) {
        for id in CHATS {
            db.save_conversation_boundary(&json!({"id":id})).unwrap();
        }
    }

    fn reference_schema(db: &Connection) {
        previous_schema(db);
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.uploaded_attachment(chat(id), id)
            .unwrap()
            .unwrap()
            .metadata
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        serde_json::from_value(db.kv(&key(chat(id), id)).unwrap().unwrap()).unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        CHATS
            .iter()
            .map(|chat| db.attachments(chat).unwrap().len())
            .sum()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        CHATS
            .iter()
            .map(|chat| {
                let mut values = db.keys(&format!("chat-attachment:{chat}:")).unwrap();
                values.sort_by(|a, b| {
                    a.1["createdAt"]
                        .as_i64()
                        .cmp(&b.1["createdAt"].as_i64())
                        .then_with(|| a.1["id"].as_str().cmp(&b.1["id"].as_str()))
                });
                values.len()
            })
            .sum()
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        let reservation = db
            .reserve_attachment(self, &self.id, now() + 300000)
            .unwrap();
        if replacement {
            assert!(matches!(reservation, Reservation::Existing(_)));
        } else {
            assert!(matches!(reservation, Reservation::Acquired));
            db.commit_attachment(self, &self.id).unwrap();
        }
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }

    fn reference_write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            assert_eq!(Self::reference_read(db, &self.id), *self);
        } else {
            db.set(
                &key(&self.chat_id, &self.id),
                &serde_json::to_value(self).unwrap(),
                None,
            )
            .unwrap();
        }
    }

    fn admission_state(db: &Db<'_>, id: &str, reference: bool) -> Option<String> {
        Some(if reference {
            db.keys(&format!("chat-attachment:{}:", chat(id)))
                .unwrap()
                .iter()
                .map(|(_, a)| a["size"].as_u64().unwrap())
                .sum::<u64>()
                .to_string()
        } else {
            db.0.query_row(
                "SELECT committed_bytes+reserved_bytes FROM attachment_quotas WHERE chat_id=?",
                [chat(id)],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            .to_string()
        })
    }

    fn encoded_bytes(db: &Db<'_>) -> Option<i64> {
        let sql = if db.0.table_exists(None, "attachments").unwrap() {
            "SELECT sum(payload) FROM dbstat WHERE name IN ('attachments','attachment_quotas','attachment_reservations','attachments_order','attachment_reservations_expiry') OR name GLOB 'sqlite_autoindex_attachment*'"
        } else {
            "SELECT sum(payload) FROM dbstat WHERE name IN ('kv','sqlite_autoindex_kv_1')"
        };
        Some(db.0.query_row(sql, [], |row| row.get(0)).unwrap())
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.delete(&key(chat(id), id)).unwrap();
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn attachment_storage_layouts() {
    let corpus: Vec<ChatAttachment> = (0..1000)
        .map(|n| {
            let id = uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n).to_string();
            ChatAttachment {
                chat_id: chat(&id).into(),
                id,
                name: format!("measure-{n:04}.txt"),
                size: 1024,
                media_type: "application/octet-stream".into(),
                kind: "file".into(),
                digest: hex::encode(Sha256::digest(n.to_le_bytes())),
                created_at: 1700000000000,
                width: None,
                height: None,
            }
        })
        .collect();
    measure(&corpus);
}

async fn previous_upload(
    s: &Service,
    lock: &tokio::sync::Mutex<()>,
    chat: &str,
    id: &str,
    bytes: &[u8],
) {
    cairn_installation::validation::uuid(chat).unwrap();
    cairn_installation::validation::uuid(id).unwrap();
    let _guard = lock.lock().await;
    assert!(bytes.len() <= cairn_installation::attachments::MAX_FILE);
    let key = key(chat, id);
    assert!(s.store.kv(&key).await.unwrap().is_none());
    let prefix = format!("chat-attachment:{chat}:");
    let used = s
        .store
        .read(move |db| {
            Ok(db
                .keys(&prefix)?
                .iter()
                .map(|(_, a)| a["size"].as_u64().unwrap_or(0))
                .sum::<u64>())
        })
        .await
        .unwrap();
    assert!(used + bytes.len() as u64 <= 200 * 1024 * 1024);
    // Frozen v16 filename cleaning for this fixed ASCII text workload.
    let name: String = "file.txt"
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " ._-()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let a = json!({
        "id": id,
        "chatId": chat,
        "name": name.trim(),
        "size": bytes.len(),
        "mediaType": "application/octet-stream",
        "kind": "file",
        "digest": hex::encode(Sha256::digest(bytes)),
        "createdAt": now(),
    });
    let path = s
        .config
        .data_dir
        .join("chat-attachments")
        .join(chat)
        .join(id);
    cairn_installation::skills::private_dir(path.parent().unwrap())
        .await
        .unwrap();
    cairn_installation::skills::atomic_write(&path, bytes)
        .await
        .unwrap();
    let chat = chat.to_owned();
    s.store
        .transaction(move |db| {
            cairn_installation::conversation_lifecycle::require_active(
                &db.conversation_boundary(&chat)?.unwrap(),
            )?;
            db.set(&key, &a, None)
        })
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn two_conversation_uploads() {
    for repetition in 0..5 {
        for reference in if repetition % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let root = tempfile::tempdir().unwrap();
            let s = Service::new(common::config(root.path())).await.unwrap();
            let a = s.chat_create(json!({})).await.unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let b = s.chat_create(json!({})).await.unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            if reference {
                s.store
                    .write(|db| {
                        previous_schema(db.0);
                        Ok(())
                    })
                    .await
                    .unwrap();
            }
            let bytes = vec![b'x'; 256 * 1024];
            let lock = Arc::new(tokio::sync::Mutex::new(()));
            let uploads = async |chat: &str| {
                let mut times = Vec::new();
                for n in 0..20 {
                    let id = uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n).to_string();
                    let start = Instant::now();
                    if reference {
                        previous_upload(&s, &lock, chat, &id, &bytes).await;
                    } else {
                        s.store_attachment(chat, &id, "file.txt", &bytes)
                            .await
                            .unwrap();
                    }
                    times.push(start.elapsed().as_nanos());
                }
                times
            };
            for n in 100..102 {
                let id = uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n).to_string();
                for chat in [&a, &b] {
                    if reference {
                        previous_upload(&s, &lock, chat, &id, &bytes).await;
                    } else {
                        s.store_attachment(chat, &id, "file.txt", &bytes)
                            .await
                            .unwrap();
                    }
                }
            }
            let cpu_start = cpu();
            let start = Instant::now();
            let (left, right) = tokio::join!(uploads(&a), uploads(&b));
            println!(
                "ATTACHMENT_CONCURRENCY {}",
                json!({
                    "repetition": repetition,
                    "reference": reference,
                    "chats": 2,
                    "filesPerChat": 20,
                    "fileBytes": bytes.len(),
                    "elapsedUs": start.elapsed().as_micros(),
                    "cpuUs": cpu()-cpu_start,
                    "latency": distribution(&[left,right].concat()),
                })
            );
        }
    }
}
