//! Frozen A12 records mapping at 40d25b07 versus A13's typed plugin state.
use super::*;
use cairn_installation::plugins::{
    queues::{Delivery, Event, Receipt},
    state::{Activity, Request, RequestStatus, Timer},
};

const INSTANCE: &str = "00000000-0000-4000-8000-000000000071";

fn prepare(db: &Db<'_>) {
    let mut instance = PluginInstance::draft(INSTANCE, "discord", json!({}), Vec::new());
    instance.name = "Qualification".into();
    instance.revision = "qualification".into();
    db.save_plugin_instance(&instance).unwrap();
}

fn identifier(n: u128) -> String {
    uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n).to_string()
}

impl StorageEntity for Request {
    const KIND: &'static str = "plugin-requests";
    const TABLE: &'static str = "plugin_requests";
    const SAVE: &'static str = "plugin.access.requested";

    fn prepare(db: &Db<'_>) {
        prepare(db);
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.plugin_request(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.plugin_requests().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_plugin_request(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }
}

impl StorageEntity for Event {
    const KIND: &'static str = "plugin-events";
    const TABLE: &'static str = "plugin_events";
    const SAVE: &'static str = "plugin.event.queued";

    fn prepare(db: &Db<'_>) {
        prepare(db);
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.plugin_event(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.plugin_events(INSTANCE, i64::MAX).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_plugin_event(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }
}

impl StorageEntity for Delivery {
    const KIND: &'static str = "plugin-deliveries";
    const TABLE: &'static str = "plugin_deliveries";
    const SAVE: &'static str = "plugin.message.admitted";

    fn prepare(db: &Db<'_>) {
        prepare(db);
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.plugin_delivery(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.plugin_deliveries(INSTANCE, i64::MAX).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_plugin_delivery(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }
}

impl StorageEntity for Timer {
    const KIND: &'static str = "plugin-timers";
    const TABLE: &'static str = "plugin_timers";
    const SAVE: &'static str = "plugin.timer.scheduled";

    fn prepare(db: &Db<'_>) {
        prepare(db);
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.plugin_timer(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.plugin_timers(INSTANCE, i64::MAX).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, replacement: bool) {
        if replacement {
            db.remove_plugin_timer(&self.id).unwrap();
        }
        db.save_plugin_timer(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }
}

impl StorageEntity for Activity {
    const KIND: &'static str = "plugin-activity";
    const TABLE: &'static str = "plugin_activity";
    const SAVE: &'static str = "plugin.activity.saved";

    fn prepare(db: &Db<'_>) {
        prepare(db);
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.plugin_activity_item(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.plugin_activity().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_plugin_activity(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }
}

impl StorageEntity for Receipt {
    const KIND: &'static str = "plugin-http-receipts";
    const TABLE: &'static str = "plugin_http_receipts";
    const SAVE: &'static str = "plugin.receipt.saved";

    fn prepare(db: &Db<'_>) {
        prepare(db);
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.plugin_receipt(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.plugin_receipts(INSTANCE).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_plugin_receipt(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        let v = db.get(Self::KIND, id).unwrap().unwrap();
        Receipt {
            id: id.into(),
            instance_id: v["connectionId"].as_str().unwrap().into(),
            fingerprint: v["fingerprint"].as_str().unwrap().into(),
            response: v.get("response").cloned(),
            created_at: v["createdAt"].as_i64().unwrap(),
        }
    }

    fn reference_write(&self, db: &Db<'_>, _: bool) {
        let mut v = json!({"id":self.id,"connectionId":self.instance_id,"fingerprint":self.fingerprint,"state":if self.response.is_some(){"completed"}else{"pending"},"createdAt":self.created_at});
        if let Some(response) = &self.response {
            v["response"] = response.clone();
        }
        db.0.execute("INSERT INTO records VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at",params![self.id,Self::KIND,v.to_string(),now()]).unwrap();
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct PackageApproval {
    id: String,
    active: String,
    approvals: std::collections::BTreeMap<String, String>,
}

impl StorageEntity for PackageApproval {
    const KIND: &'static str = "plugin-packages";
    const TABLE: &'static str = "plugin_packages";
    const SAVE: &'static str = "plugin.package.approved";

    fn prepare(db: &Db<'_>) {
        for id in ["discord", "github", "gmail", "onepassword"] {
            db.remove_plugin_package(id).unwrap();
        }
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE plugin_approvals;DROP TABLE plugin_versions;DROP TABLE plugin_packages;DROP TABLE records;CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);CREATE INDEX records_kind ON records(kind,updated_at DESC);CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';").unwrap();
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        let state = db.plugin_package(id).unwrap();
        Self {
            id: id.into(),
            active: state.active.unwrap(),
            approvals: state.approvals,
        }
    }

    fn list(db: &Db<'_>) -> usize {
        db.plugin_packages().unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        for (version, digest) in &self.approvals {
            db.activate_plugin_version(&self.id, version, digest, true)
                .unwrap();
        }
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        let v = db
            .get(Self::KIND, &format!("package:{id}"))
            .unwrap()
            .unwrap();
        Self {
            id: id.into(),
            active: v["active"].as_str().unwrap().into(),
            approvals: serde_json::from_value(v["approvals"].clone()).unwrap(),
        }
    }

    fn reference_write(&self, db: &Db<'_>, _: bool) {
        let v = json!({"id":format!("package:{}",self.id),"pluginId":self.id,"active":self.active,"approvals":self.approvals,"updatedAt":now()});
        db.0.execute("INSERT INTO records VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at",params![format!("package:{}",self.id),Self::KIND,v.to_string(),now()]).unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.execute("DELETE FROM records WHERE id=?1", [format!("package:{id}")])
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn plugin_package_and_queue_storage_layouts() {
    measure(
        &(0..1000)
            .map(|n| PackageApproval {
                id: format!("qualification-{n:04}"),
                active: "1.0.0".into(),
                approvals: std::collections::BTreeMap::from([("1.0.0".into(), "a".repeat(64))]),
            })
            .collect::<Vec<_>>(),
    );
    measure(
        &(0..50)
            .map(|n| Request {
                id: identifier(n),
                instance_id: INSTANCE.into(),
                user_id: format!("person-{n}"),
                name: format!("Person {n}"),
                status: if n % 3 == 0 {
                    RequestStatus::Denied
                } else {
                    RequestStatus::Pending
                },
                mentions: 7,
                created_at: 1700000000000,
                updated_at: 1700000000001,
            })
            .collect::<Vec<_>>(),
    );
    measure(&(0..50).map(|n|Event{id:identifier(n),instance_id:INSTANCE.into(),revision:"qualification".into(),name:"message".into(),payload:json!({"id":n.to_string(),"channel":"channel-1","author":{"id":"person-1","name":"Person 1"},"content":"Deterministic event payload. ".repeat(15)}),author_id:Some("person-1".into()),attempts:0,created_at:1700000000000,next_attempt_at:1700000000000}).collect::<Vec<_>>());
    measure(
        &(0..20)
            .map(|n| Delivery {
                id: identifier(n),
                instance_id: INSTANCE.into(),
                revision: "qualification".into(),
                chat_id: identifier(n + 100),
                thread: format!("thread-{n}"),
                user_id: "person-1".into(),
                reply: json!({"channel":"channel-1","message":"message-1"}),
                state: json!({"sent":false}),
                asked: vec!["question-1".into()],
                question_retry_at: std::collections::BTreeMap::from([(
                    "question-2".into(),
                    1700000000010,
                )]),
                progress_at: 1700000000000,
                attempts: 1,
                created_at: 1700000000000,
                next_attempt_at: 1700000000000,
            })
            .collect::<Vec<_>>(),
    );
    measure(
        &(0..100)
            .map(|n| Timer {
                id: identifier(n),
                instance_id: INSTANCE.into(),
                revision: "qualification".into(),
                name: "refresh".into(),
                payload: json!({"key":n,"token":false}),
                at: 1700000000000 + n as i64,
            })
            .collect::<Vec<_>>(),
    );
    measure(
        &(0..30)
            .map(|n| Activity {
                id: identifier(n),
                delivery_id: identifier(n + 100),
                instance_id: INSTANCE.into(),
                chat_id: identifier(n + 200),
                user_id: "person-1".into(),
                name: "Person 1".into(),
                preview: "Deterministic message preview. ".repeat(4),
                status: cairn_contracts::PluginActivityStatus::Working,
                created_at: 1700000000000,
                updated_at: 1700000000000,
            })
            .collect::<Vec<_>>(),
    );
    measure(
        &(0..1000)
            .map(|n| Receipt {
                id: format!(
                    "{INSTANCE}:{}",
                    cairn_installation::auth::hex_digest(&n.to_string())
                ),
                instance_id: INSTANCE.into(),
                fingerprint: "a".repeat(64),
                response: (n % 3 != 0).then(
                    || json!({"status":200,"headers":{},"json":{"id":n.to_string()},"text":null}),
                ),
                created_at: 1700000000000,
            })
            .collect::<Vec<_>>(),
    );
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct StorageNote {
    key: String,
    value: Value,
}

impl StorageEntity for StorageNote {
    const KIND: &'static str = "plugin-storage";
    const TABLE: &'static str = "plugin_storage";
    const SAVE: &'static str = "plugin.storage.saved";

    fn prepare(db: &Db<'_>) {
        prepare(db);
    }

    fn id(&self) -> String {
        self.key.clone()
    }

    fn read(db: &Db<'_>, key: &str) -> Self {
        Self {
            key: key.into(),
            value: db.plugin_storage(INSTANCE, key).unwrap().unwrap(),
        }
    }

    fn list(db: &Db<'_>) -> usize {
        let mut stmt =
            db.0.prepare("SELECT key,value FROM plugin_storage WHERE instance_id=?1 ORDER BY key")
                .unwrap();
        stmt.query_map([INSTANCE], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Value>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_plugin_storage(INSTANCE, &self.key, &self.value)
            .unwrap();
        db.audit(Self::SAVE, &json!({"id":self.key})).unwrap();
    }

    fn delete_candidate(db: &Db<'_>, key: &str) {
        db.remove_plugin_storage(INSTANCE, key).unwrap();
    }

    fn reference_read(db: &Db<'_>, key: &str) -> Self {
        let id = format!(
            "storage:{INSTANCE}:{}",
            cairn_installation::auth::hex_digest(key)
        );
        let v = db.get(Self::KIND, &id).unwrap().unwrap();
        Self {
            key: key.into(),
            value: v["value"].clone(),
        }
    }

    fn reference_write(&self, db: &Db<'_>, _: bool) {
        let id = format!(
            "storage:{INSTANCE}:{}",
            cairn_installation::auth::hex_digest(&self.key)
        );
        let value = json!({"id":id,"connectionId":INSTANCE,"key":self.key,"value":self.value,"updatedAt":now()});
        db.0.execute("INSERT INTO records VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at",params![id,Self::KIND,value.to_string(),now()]).unwrap();
    }

    fn reference_delete(db: &Db<'_>, key: &str) {
        db.0.execute(
            "DELETE FROM records WHERE id=?1",
            [format!(
                "storage:{INSTANCE}:{}",
                cairn_installation::auth::hex_digest(key)
            )],
        )
        .unwrap();
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn plugin_private_storage_layouts() {
    measure(&(0..1000).map(|n|StorageNote{key:format!("note-{n:04}"),value:json!({"pending":false,"message":format!("message-{n}"),"text":"Deterministic plugin-owned opaque state. ".repeat(8)})}).collect::<Vec<_>>());
}
