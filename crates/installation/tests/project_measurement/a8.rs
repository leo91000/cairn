//! Frozen v15 domain boundaries compared using A1/A2's shared qualification.
use super::*;
use cairn_contracts::{
    ChatQuestion, ChatWakeup, ConversationRequester, PluginWaitData, WaitCondition,
    WaitMemberProgress,
};

const CHAT: &str = "measurement-question-chat";
fn owner(n: usize) -> String {
    format!("a8-owner-{n:04}")
}

impl StorageEntity for ChatQuestion {
    const KIND: &'static str = "a8-questions";
    const TABLE: &'static str = "chat_questions";
    const SAVE: &'static str = "chat.question";
    fn prepare(db: &Db<'_>) {
        db.save_conversation_boundary(&json!({"id":CHAT})).unwrap();
    }
    fn id(&self) -> String {
        self.id.clone()
    }
    fn read(db: &Db<'_>, id: &str) -> Self {
        db.question(CHAT, id).unwrap().unwrap()
    }
    fn list(db: &Db<'_>) -> usize {
        db.questions(CHAT).unwrap().len()
    }
    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_question(self).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }
    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE question_options; DROP TABLE question_fields; DROP TABLE question_import_metadata; DROP TABLE chat_questions; DROP TABLE kv;
CREATE TABLE kv(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%' AND key NOT LIKE 'chat-coding-agent:%' AND key NOT LIKE 'chat-coding-agent-request:%' AND key!='conversation-expiration'),data TEXT NOT NULL,expires INTEGER);").unwrap();
    }
    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        db.kv_as(&format!("chat-question:{CHAT}:{id}"))
            .unwrap()
            .unwrap()
    }
    fn reference_list(db: &Db<'_>) -> usize {
        let mut questions = db
            .keys_as::<ChatQuestion>(&format!("chat-question:{CHAT}:"))
            .unwrap();
        questions.sort_by_key(|(_, q)| q.created_at);
        questions.len()
    }
    fn reference_write(&self, db: &Db<'_>, _: bool) {
        db.set_as(&format!("chat-question:{CHAT}:{}", self.id), self, None)
            .unwrap();
    }
    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.execute(
            "DELETE FROM kv WHERE key=?",
            [format!("chat-question:{CHAT}:{id}")],
        )
        .unwrap();
    }
}

// Each read returns its actual domain representation. Equality normalizes only
// after the harness stops the timer, so neither reader pays the other's decoder.
#[derive(Debug, Clone)]
enum WakeupSample {
    Prepared {
        current: ChatWakeup,
        previous: Value,
    },
    Current(ChatWakeup),
    Previous(Value),
}
fn previous_condition(value: &mut Value) {
    if value["type"] == "plugin" {
        let mut raw = value["input"].as_object().unwrap().clone();
        raw.insert("type".into(), value["conditionType"].clone());
        raw.insert("pluginId".into(), value["pluginId"].clone());
        if let Some(project) = value.get("projectId") {
            raw.insert("projectId".into(), project.clone());
        }
        *value = raw.into();
    } else if value["type"] == "group" {
        for member in value["conditions"].as_array_mut().unwrap() {
            previous_condition(member);
        }
    }
}
fn previous_wakeup(current: &ChatWakeup) -> Value {
    let mut value = serde_json::to_value(current).unwrap();
    if value["condition"].is_object() {
        previous_condition(&mut value["condition"]);
    }
    value
}
impl WakeupSample {
    fn current(&self) -> &ChatWakeup {
        match self {
            Self::Prepared { current, .. } | Self::Current(current) => current,
            _ => panic!("write fixtures use prepared values"),
        }
    }
    fn previous(&self) -> Value {
        match self {
            Self::Prepared { previous, .. } | Self::Previous(previous) => previous.clone(),
            Self::Current(current) => previous_wakeup(current),
        }
    }
}
impl PartialEq for WakeupSample {
    fn eq(&self, other: &Self) -> bool {
        self.previous() == other.previous()
    }
}
impl serde::Serialize for WakeupSample {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.previous().serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for WakeupSample {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Value::deserialize(deserializer).map(Self::Previous)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Continuation {
    id: String,
    wakeup: WakeupSample,
    requester: ConversationRequester,
}
impl StorageEntity for Continuation {
    const KIND: &'static str = "a8-continuations-lineage";
    const SAVE: &'static str = "chat.wait";
    fn prepare(db: &Db<'_>) {
        for n in 0..1000 {
            db.save_conversation_boundary(&json!({"id":owner(n)}))
                .unwrap();
        }
    }
    fn id(&self) -> String {
        self.id.clone()
    }
    fn read(db: &Db<'_>, id: &str) -> Self {
        let tx =
            rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Deferred)
                .unwrap();
        let reader = Db(&tx);
        Self {
            id: id.into(),
            wakeup: WakeupSample::Current(reader.wakeup(true, id).unwrap().unwrap()),
            requester: reader.lineages(Some(id)).unwrap().remove(id).unwrap(),
        }
    }
    fn list(db: &Db<'_>) -> usize {
        let tx =
            rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Deferred)
                .unwrap();
        let reader = Db(&tx);
        let wakeups = reader.wakeups(true, None).unwrap();
        let lineage = reader.lineages(None).unwrap();
        assert_eq!(wakeups.len(), lineage.len());
        wakeups.len()
    }
    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_wakeup(true, &self.id, Some(self.wakeup.current()))
            .unwrap();
        db.save_lineage(&self.id, Some(&self.requester)).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }
    fn delete_candidate(db: &Db<'_>, id: &str) {
        db.save_wakeup(true, id, None).unwrap();
        db.save_lineage(id, None).unwrap();
    }
    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE wait_conditions; DROP TABLE durable_wakeups; DROP TABLE conversation_lineage; ALTER TABLE conversations ADD COLUMN wakeup TEXT CHECK(wakeup IS NULL OR json_valid(wakeup)); ALTER TABLE conversations ADD COLUMN requested_by TEXT CHECK(requested_by IS NULL OR json_valid(requested_by));").unwrap();
    }
    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        let (wake, request): (String, String) =
            db.0.prepare_cached("SELECT wakeup,requested_by FROM conversations WHERE id=?")
                .unwrap()
                .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
        Self {
            id: id.into(),
            wakeup: WakeupSample::Previous(serde_json::from_str(&wake).unwrap()),
            requester: serde_json::from_str(&request).unwrap(),
        }
    }
    fn reference_list(db: &Db<'_>) -> usize {
        db.0.prepare_cached(
            "SELECT wakeup,requested_by FROM conversations WHERE wakeup IS NOT NULL",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(|row| {
            let (w, r) = row.unwrap();
            let _: Value = serde_json::from_str(&w).unwrap();
            let _: ConversationRequester = serde_json::from_str(&r).unwrap();
        })
        .count()
    }
    fn reference_write(&self, db: &Db<'_>, _: bool) {
        db.0.prepare_cached("UPDATE conversations SET wakeup=?,requested_by=? WHERE id=?")
            .unwrap()
            .execute(params![
                serde_json::to_string(match &self.wakeup {
                    WakeupSample::Prepared { previous, .. } | WakeupSample::Previous(previous) =>
                        previous,
                    _ => panic!("prepared reference writer"),
                })
                .unwrap(),
                serde_json::to_string(&self.requester).unwrap(),
                self.id
            ])
            .unwrap();
    }
    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.execute(
            "UPDATE conversations SET wakeup=NULL,requested_by=NULL WHERE id=?",
            [id],
        )
        .unwrap();
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn a8_question_storage_layouts() {
    let values=(0..1000).map(|n|serde_json::from_value(json!({
        "id":format!("question-{n:04}"),"chatId":CHAT,"runId":format!("run-{n:04}"),"source":"cairn","blocking":false,"status":"pending","createdAt":1700000000000i64+n,
        "fields":(0..3).map(|f|json!({"id":format!("field-{f}"),"title":"Deterministic questionnaire storage qualification. ".repeat(4),"secret":f==2,"mode":"multiple","options":(0..4).map(|o|json!({"id":format!("option-{o}"),"label":format!("Choice {o}"),"description":"Reproducible synthetic option. ".repeat(3),"recommended":o==0})).collect::<Vec<_>>()})).collect::<Vec<_>>()
    })).unwrap()).collect::<Vec<ChatQuestion>>();
    measure(&values);
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn a8_continuation_storage_layouts() {
    let values = (0..1000)
        .map(|n| {
            let id = owner(n);
            let conditions = vec![
                WaitCondition::RunFinished {
                    run_id: format!("run-{n}"),
                },
                WaitCondition::Plugin {
                    plugin_id: "github".into(),
                    condition_type: "github_workflow".into(),
                    project_id: Some("project".into()),
                    input: PluginWaitData(std::collections::BTreeMap::from([
                        ("runId".into(), json!(n + 1)),
                        ("headSha".into(), json!("a".repeat(40))),
                    ])),
                },
            ];
            let current = ChatWakeup {
                id: id.clone(),
                message_id: Some(format!("message-{n}")),
                reason: "Deterministic continuation storage qualification. ".repeat(3),
                seconds: 3600,
                created_at: 1700000000000,
                wake_at: 1700003600000,
                condition: Some(WaitCondition::Group {
                    mode: if n % 2 == 0 { "all" } else { "any" }.into(),
                    conditions,
                }),
                time_condition: None,
                members: Some(vec![
                    WaitMemberProgress::default(),
                    WaitMemberProgress {
                        next_check_at: Some(1700000000000),
                        last_error: (n % 3 == 0).then(|| "Temporary synthetic error".into()),
                        ..Default::default()
                    },
                ]),
                progress: Default::default(),
            };
            let previous = previous_wakeup(&current);
            Continuation {
                id: id.clone(),
                wakeup: WakeupSample::Prepared { current, previous },
                requester: ConversationRequester {
                    chat_id: "requester".into(),
                    run_id: "requester-run".into(),
                    request_id: Some(format!("request-{n}")),
                },
            }
        })
        .collect::<Vec<_>>();
    measure(&values);
}
