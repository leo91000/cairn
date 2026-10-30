//! A7 mapping qualification, reusing the A1/A2 workload, clocks and distributions.
//! Frozen reference: records/chats and chat_messages.data at 663259d9.
use super::{StorageEntity, measure};
use cairn_installation::{
    chats::{Conversation, Message},
    store::Db,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn legacy_schema(db: &Connection) {
    db.execute_batch("DROP TABLE conversation_engine_switches;
DROP TABLE message_attachments;
DROP TABLE chat_messages;
DROP TABLE conversations;
DROP TABLE conversation_retention;
DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';
CREATE TABLE chat_messages(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,data TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id,created_at);").unwrap();
}

// The frozen path returns Values; the candidate returns typed state. Keep the
// conversion used only for equality outside the measured read itself.
#[derive(Clone, Debug)]
struct MeasuredConversation {
    typed: Option<Conversation>,
    wire: Value,
}

impl Serialize for MeasuredConversation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MeasuredConversation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = Value::deserialize(deserializer)?;
        let typed = serde_json::from_value(wire.clone()).map_err(serde::de::Error::custom)?;
        Ok(Self {
            typed: Some(typed),
            wire,
        })
    }
}

impl MeasuredConversation {
    fn fixture(wire: Value) -> Self {
        Self {
            typed: Some(serde_json::from_value(wire.clone()).unwrap()),
            wire,
        }
    }

    fn typed(value: Conversation) -> Self {
        Self {
            typed: Some(value),
            wire: Value::Null,
        }
    }

    fn serialized(&self) -> Value {
        self.typed.as_ref().map_or_else(
            || self.wire.clone(),
            |value| serde_json::to_value(value).unwrap(),
        )
    }
}

impl PartialEq for MeasuredConversation {
    fn eq(&self, other: &Self) -> bool {
        let normalize = |conversation: &Self| {
            let mut value = conversation.serialized();
            value.as_object_mut().unwrap().retain(|key, value| {
                !matches!(key.as_str(), "slug" | "revision") && !value.is_null()
            });
            value
        };
        normalize(self) == normalize(other)
    }
}

impl StorageEntity for MeasuredConversation {
    const KIND: &'static str = "chats";
    const TABLE: &'static str = "conversations";
    const SAVE: &'static str = "chat.saved";

    fn id(&self) -> String {
        self.typed.as_ref().map_or_else(
            || self.wire["id"].as_str().unwrap().to_owned(),
            |value| value.id.clone(),
        )
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        Self::typed(db.conversation(id).unwrap().unwrap())
    }

    fn list(db: &Db<'_>) -> usize {
        db.conversations().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        let mut value = self.typed.as_ref().unwrap().clone();
        if replacement {
            value.revision = db.conversation(&value.id).unwrap().unwrap().revision;
        }
        db.save_conversation(&value).unwrap();
        db.audit(Self::SAVE, &json!({ "id": value.id })).unwrap();
    }

    fn reference_schema(db: &Connection) {
        legacy_schema(db);
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        Self {
            typed: None,
            wire: db.get("chats", id).unwrap().unwrap(),
        }
    }

    fn reference_write(&self, db: &Db<'_>, replacement: bool) {
        let mut value = self.wire.clone();
        if replacement {
            // The old mutator read its current record, then its writer read it
            // again to preserve the immutable slug. Match those two reads.
            let previous = Self::reference_read(db, &self.id());
            value["revision"] = previous.wire["revision"].clone();
            value["slug"] = Self::reference_read(db, &self.id()).wire["slug"].clone();
        } else {
            let _ = db.get("chats", &self.id()).unwrap();
            loop {
                let slug = cairn_protocol::slugs::readable(uuid::Uuid::new_v4().as_bytes());
                let used: bool = db.0.query_row("SELECT EXISTS(SELECT 1 FROM records WHERE kind='chats' AND json_extract(data,'$.slug')=?)", [&slug], |r| r.get(0)).unwrap();
                if !used {
                    value["slug"] = slug.into();
                    break;
                }
            }
        }
        db.0.prepare_cached("INSERT INTO records VALUES(?,'chats',?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at")
            .unwrap().execute(params![self.id(), value.to_string(), cairn_installation::config::now()]).unwrap();
    }

    fn encoded_bytes(db: &Db<'_>) -> Option<i64> {
        Some(db.0.query_row("SELECT coalesce(sum(payload),0) FROM dbstat WHERE name='conversations' OR name LIKE 'conversations_%' OR name LIKE 'sqlite_autoindex_conversations_%' OR name='records' OR name='records_kind' OR name='records_chat_slug' OR name LIKE 'sqlite_autoindex_records_%'", [], |row| row.get(0)).unwrap())
    }
}

#[tokio::test]
#[ignore = "local A7 measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn conversation_storage_layouts() {
    let corpus: Vec<_> = (0..1000).map(|n| MeasuredConversation::fixture(json!({
        "id": format!("00000000-0000-4000-8000-{n:012}"),
        "slug": format!("quiet-cedar-{n:08x}"),
        "revision": "00000000-0000-4000-9000-000000000001",
        "title": format!("Conversation {n}: {}", "A deterministic retained conversation. ".repeat(3)),
        "agentId": "00000000-0000-4000-8000-999999999999",
        "projectId": null,
        "paused": n % 3 == 0,
        "createdAt": 1700000000000_i64,
        "updatedAt": 1700000000100_i64,
        "lastActivityAt": 1700000000100_i64,
        "lifecycle": if n % 5 == 0 { "trash" } else { "active" },
        "trashedAt": if n % 5 == 0 { Some(1700000000100_i64) } else { None },
        "purgeAt": if n % 5 == 0 { Some(1700000000100_i64) } else { None },
        "sessionRestartRequested": false,
        "cancelledByDeletion": false,
        "nativePrincipals": ["owner-account"],
        "nativeAuthorityNodes": [],
        "nativeProvenanceUnknown": false,
        "requestedBy": if n % 4 == 0 {
            Some(json!({
                "runId": "parent-run",
                "chatId": "parent-chat",
                "requestId": format!("request-{n}"),
            }))
        } else { None },
    }))).collect();
    measure(&corpus);
}

#[derive(Clone, Debug)]
struct MeasuredMessage {
    typed: Option<Message>,
    wire: Value,
}

impl Serialize for MeasuredMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MeasuredMessage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = Value::deserialize(deserializer)?;
        let typed = serde_json::from_value(wire.clone()).map_err(serde::de::Error::custom)?;
        Ok(Self {
            typed: Some(typed),
            wire,
        })
    }
}

impl MeasuredMessage {
    fn fixture(wire: Value) -> Self {
        Self {
            typed: Some(serde_json::from_value(wire.clone()).unwrap()),
            wire,
        }
    }

    fn typed(value: Message) -> Self {
        Self {
            typed: Some(value),
            wire: Value::Null,
        }
    }

    fn serialized(&self) -> Value {
        self.typed.as_ref().map_or_else(
            || self.wire.clone(),
            |value| serde_json::to_value(value).unwrap(),
        )
    }
}

impl PartialEq for MeasuredMessage {
    fn eq(&self, other: &Self) -> bool {
        let normalize = |message: &Self| {
            let mut value = message.serialized();
            value.as_object_mut().unwrap().remove("revision");
            value
        };
        normalize(self) == normalize(other)
    }
}

const CHAT: &str = "00000000-0000-4000-8000-000000000001";

impl StorageEntity for MeasuredMessage {
    const KIND: &'static str = "messages";
    const TABLE: &'static str = "chat_messages";
    const SAVE: &'static str = "chat.message.saved";

    fn id(&self) -> String {
        self.typed.as_ref().map_or_else(
            || self.wire["id"].as_str().unwrap().to_owned(),
            |value| value.id.clone(),
        )
    }

    fn prepare(db: &Db<'_>) {
        db.save_conversation(&serde_json::from_value(json!({"id": CHAT})).unwrap())
            .unwrap();
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        Self::typed(db.conversation_message(CHAT, id).unwrap().unwrap())
    }

    fn list(db: &Db<'_>) -> usize {
        db.conversation_messages(CHAT).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _replacement: bool) {
        db.save_message(self.typed.as_ref().unwrap()).unwrap();
        db.audit(Self::SAVE, &json!({"id": self.id()})).unwrap();
    }

    fn reference_schema(db: &Connection) {
        let parent = serde_json::to_value(Db(db).conversation(CHAT).unwrap().unwrap()).unwrap();
        legacy_schema(db);
        db.execute(
            "INSERT INTO records VALUES(?,'chats',?,?)",
            params![CHAT, parent.to_string(), 0],
        )
        .unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        let messages = db
            .json_rows(
                "SELECT data FROM chat_messages WHERE chat_id=? ORDER BY created_at,rowid",
                [CHAT],
            )
            .unwrap();
        Self {
            typed: None,
            wire: messages
                .into_iter()
                .find(|message| message["id"] == id)
                .unwrap(),
        }
    }

    fn reference_list(db: &Db<'_>) -> usize {
        db.json_rows(
            "SELECT data FROM chat_messages WHERE chat_id=? ORDER BY created_at,rowid",
            [CHAT],
        )
        .unwrap()
        .len()
    }

    fn reference_write(&self, db: &Db<'_>, _replacement: bool) {
        db.0.prepare_cached("INSERT INTO chat_messages VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data")
            .unwrap().execute(params![self.id(),CHAT,self.wire.to_string(),self.wire["createdAt"].as_i64().unwrap()]).unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        Self::delete_candidate(db, id);
    }

    fn encoded_bytes(db: &Db<'_>) -> Option<i64> {
        Some(db.0.query_row(
            "SELECT coalesce(sum(payload),0) FROM dbstat WHERE name='chat_messages' OR name LIKE 'chat_messages_%' OR name LIKE 'sqlite_autoindex_chat_messages_%' OR name='message_attachments' OR name LIKE 'sqlite_autoindex_message_attachments_%'",
            [], |row| row.get(0),
        ).unwrap())
    }
}

#[tokio::test]
#[ignore = "local A7 measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn message_storage_layouts() {
    let corpus: Vec<_> = (0..1000)
        .map(|n| {
            MeasuredMessage::fixture(json!({
                "id": format!("00000000-0000-4000-8000-{n:012}"),
                "chatId": CHAT,
                "text": "A reproducible conversation message with UTF-8 🦊. ".repeat(12),
                "createdAt": 1700000000000_i64 + n,
                "mode": if n % 3 == 0 { "steer" } else { "queue" },
                "status": if n % 4 == 0 { "queued" } else { "delivered" },
                "provider": "codex",
                "model": "gpt-6-sol",
                "reasoning": "high",
                "speed": "standard",
                "answers": if n % 2 == 0 {
                    json!({"field": ["Free text", "second choice", "Free text"]})
                } else {
                    json!({"field": {"selected": ["b", "a"], "comment": "Separate comment"}})
                },
                "attachments": if n % 4 == 0 { vec![json!({
                    "id": format!("00000000-0000-4000-9000-{n:012}"),
                "chatId": CHAT,
                    "name": format!("file-{n}.txt"),
                    "size": 1024,
                    "kind": "file",
                    "mediaType": "text/plain",
                    "digest": "a".repeat(64),
                    "createdAt": 1700000000000_i64,
                })] } else { Vec::<Value>::new() },
            }))
        })
        .collect();
    measure(&corpus);
}
