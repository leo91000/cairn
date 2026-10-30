//! Frozen v15 domain boundaries compared using A1/A2's shared qualification.
use super::*;
use cairn_contracts::{
    ChatQuestion, ChatWakeup, ConversationRequester, PluginWaitData, WaitCondition,
    WaitMemberProgress,
};

const CHAT: &str = "00000000-0000-4000-8000-000000000001";

fn owner(n: usize) -> String {
    uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n as u128).to_string()
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
        "id":format!("{n:064x}"),"chatId":CHAT,"runId":uuid::Uuid::from_u128((4_u128<<76)|(2_u128<<62)|n as u128).to_string(),"source":"cairn","blocking":false,"status":"pending","createdAt":1700000000000i64+n,
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum AnswerSample {
    Current(std::collections::BTreeMap<String, cairn_contracts::QuestionAnswer>),
    Previous(Value),
}

impl PartialEq for AnswerSample {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(self).unwrap() == serde_json::to_value(other).unwrap()
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct PrivateAnswer {
    id: String,
    text: String,
    answers: AnswerSample,
}

impl StorageEntity for PrivateAnswer {
    const KIND: &'static str = "a8-private-answers";
    const TABLE: &'static str = "chat_messages";
    const SAVE: &'static str = "chat.answer";

    fn prepare(db: &Db<'_>) {
        db.save_conversation_boundary(&json!({"id":CHAT})).unwrap();
    }

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        let message = db.conversation_message(CHAT, id).unwrap().unwrap();
        Self {
            id: message.id.clone(),
            text: message.text.clone(),
            answers: AnswerSample::Current(message.answers.unwrap()),
        }
    }

    fn list(db: &Db<'_>) -> usize {
        db.conversation_messages(CHAT).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        let AnswerSample::Current(answers) = &self.answers else {
            panic!("typed writer")
        };
        let message = cairn_installation::chats::Message {
            content: cairn_contracts::ChatMessage {
                id: self.id.clone(),
                chat_id: CHAT.into(),
                revision: "fixture".into(),
                text: self.text.clone(),
                provider: "codex".into(),
                model: "model".into(),
                reasoning: "high".into(),
                speed: String::new(),
                mode: cairn_contracts::MessageMode::Queue,
                status: cairn_contracts::MessageStatus::Queued,
                created_at: 1700000000000,
                attachments: vec![],
                question_id: Some("question".into()),
            },
            answers: Some(answers.clone()),
        };
        db.save_message(&message).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id})).unwrap();
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE question_answer_choices; DROP TABLE question_answers; DROP TABLE question_answer_deliveries; ALTER TABLE chat_messages ADD COLUMN answers TEXT CHECK(answers IS NULL OR json_valid(answers));").unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        let (text, answers): (String, String) =
            db.0.prepare_cached("SELECT text,answers FROM chat_messages WHERE chat_id=? AND id=?")
                .unwrap()
                .query_row(params![CHAT, id], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
        Self {
            id: id.into(),
            text,
            answers: AnswerSample::Previous(serde_json::from_str(&answers).unwrap()),
        }
    }

    fn reference_list(db: &Db<'_>) -> usize {
        db.0.prepare_cached(
            "SELECT id,text,answers FROM chat_messages WHERE chat_id=? ORDER BY created_at,rowid",
        )
        .unwrap()
        .query_map([CHAT], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|row| {
            let (id, text, answers) = row.unwrap();
            PrivateAnswer {
                id,
                text,
                answers: AnswerSample::Previous(serde_json::from_str(&answers).unwrap()),
            }
        })
        .count()
    }

    fn reference_write(&self, db: &Db<'_>, _: bool) {
        db.0.prepare_cached("INSERT INTO chat_messages(id,chat_id,revision,text,provider,model,reasoning,speed,mode,status,created_at,question_id,answers) VALUES(?,?,'fixture',?,'codex','model','high','','queue','queued',1700000000000,'question',?) ON CONFLICT(id) DO UPDATE SET text=excluded.text,answers=excluded.answers,revision=excluded.revision").unwrap().execute(params![self.id,CHAT,self.text,serde_json::to_string(&self.answers).unwrap()]).unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        Self::delete_candidate(db, id);
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn a8_private_answer_storage_layouts() {
    let values = (0..1000)
        .map(|n| {
            let answers = (0..3)
                .map(|f| {
                    (
                        format!("field-{f}"),
                        cairn_contracts::QuestionAnswer {
                            selected: vec!["second".into(), "first".into()],
                            comment: "Synthetic private questionnaire answer. ".repeat(12),
                        },
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            PrivateAnswer {
                id: format!("answer-{n:04}"),
                text: format!(
                    "My answers to your questions: {}",
                    serde_json::to_string(&answers).unwrap()
                ),
                answers: AnswerSample::Current(answers),
            }
        })
        .collect::<Vec<_>>();
    measure(&values);
}
