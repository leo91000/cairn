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

// The v15 reader kept option objects and extension fields as Values. Keep that
// decoder here; conversions used only for equality run outside the timed read.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviousQuestionField {
    id: String,
    title: String,
    #[serde(default)]
    secret: bool,
    #[serde(default = "previous_single")]
    mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min_choices: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_choices: Option<usize>,
    #[serde(default)]
    options: Vec<Value>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

fn previous_single() -> String {
    "single".into()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviousQuestion {
    id: String,
    chat_id: String,
    run_id: String,
    blocking: bool,
    #[serde(default)]
    source: String,
    status: cairn_contracts::QuestionStatus,
    fields: Vec<PreviousQuestionField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone)]
enum QuestionSample {
    Prepared {
        current: ChatQuestion,
        previous: Box<PreviousQuestion>,
    },
    Current(ChatQuestion),
    Previous(Box<PreviousQuestion>),
}

impl QuestionSample {
    fn id(&self) -> &str {
        match self {
            Self::Prepared { current, .. } | Self::Current(current) => &current.id,
            Self::Previous(previous) => &previous.id,
        }
    }

    fn current(&self) -> &ChatQuestion {
        match self {
            Self::Prepared { current, .. } | Self::Current(current) => current,
            _ => panic!("prepared candidate writer"),
        }
    }

    fn previous(&self) -> &PreviousQuestion {
        match self {
            Self::Prepared { previous, .. } | Self::Previous(previous) => previous,
            _ => panic!("prepared reference writer"),
        }
    }

    fn wire(&self) -> Value {
        match self {
            Self::Prepared { current, .. } | Self::Current(current) => {
                serde_json::to_value(current).unwrap()
            }
            Self::Previous(previous) => serde_json::to_value(previous).unwrap(),
        }
    }
}

impl PartialEq for QuestionSample {
    fn eq(&self, other: &Self) -> bool {
        self.wire() == other.wire()
    }
}

impl serde::Serialize for QuestionSample {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.wire().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for QuestionSample {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<PreviousQuestion>::deserialize(deserializer).map(Self::Previous)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
struct MeasuredQuestion(QuestionSample);

impl StorageEntity for MeasuredQuestion {
    const KIND: &'static str = "a8-questions";
    const TABLE: &'static str = "chat_questions";
    const SAVE: &'static str = "chat.question";

    fn prepare(db: &Db<'_>) {
        db.save_conversation_boundary(&json!({"id":CHAT,"slug":"a8-question-chat"}))
            .unwrap();
    }

    fn id(&self) -> String {
        self.0.id().into()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        Self(QuestionSample::Current(
            db.question(CHAT, id).unwrap().unwrap(),
        ))
    }

    fn list(db: &Db<'_>) -> usize {
        db.questions(CHAT).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_question(self.0.current()).unwrap();
        db.audit(Self::SAVE, &json!({"id":self.id()})).unwrap();
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE question_options; DROP TABLE question_fields; DROP TABLE question_import_metadata; DROP TABLE chat_questions; DROP TABLE kv;
CREATE TABLE kv(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%' AND key NOT LIKE 'chat-coding-agent:%' AND key NOT LIKE 'chat-coding-agent-request:%' AND key!='conversation-expiration'),data TEXT NOT NULL,expires INTEGER);").unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        Self(QuestionSample::Previous(
            db.kv_as(&format!("chat-question:{CHAT}:{id}"))
                .unwrap()
                .unwrap(),
        ))
    }

    fn reference_list(db: &Db<'_>) -> usize {
        let mut questions = db
            .keys_as::<PreviousQuestion>(&format!("chat-question:{CHAT}:"))
            .unwrap();
        questions.sort_by_key(|(_, q)| q.created_at);
        questions.len()
    }

    fn reference_write(&self, db: &Db<'_>, _: bool) {
        db.set_as(
            &format!("chat-question:{CHAT}:{}", self.id()),
            self.0.previous(),
            None,
        )
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
            db.save_conversation_boundary(&json!({"id":owner(n),"slug":format!("a8-owner-{n}")}))
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
            requester: reader.lineage(id).unwrap().unwrap(),
        }
    }

    fn list(db: &Db<'_>) -> usize {
        let tx =
            rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Deferred)
                .unwrap();
        let reader = Db(&tx);
        let wakeups = reader.wakeups(true, None).unwrap();
        let lineage = reader.lineages().unwrap();
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
    let measured = values
        .into_iter()
        .map(|current| {
            let previous =
                Box::new(serde_json::from_value(serde_json::to_value(&current).unwrap()).unwrap());
            MeasuredQuestion(QuestionSample::Prepared { current, previous })
        })
        .collect::<Vec<_>>();
    measure(&measured);
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
                progress: WaitMemberProgress::default(),
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

// Frozen v15 typed decoder, including the historical string-list variant.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum PreviousMessageAnswer {
    Legacy(Vec<String>),
    Current {
        #[serde(default)]
        selected: Vec<String>,
        #[serde(default)]
        comment: String,
    },
}

type CurrentAnswers = std::collections::BTreeMap<String, cairn_installation::chats::MessageAnswer>;

type PreviousAnswers = std::collections::BTreeMap<String, PreviousMessageAnswer>;

#[derive(Debug, Clone)]
enum AnswerSample {
    Prepared {
        current: CurrentAnswers,
        previous: PreviousAnswers,
    },
    Current(CurrentAnswers),
    Previous(PreviousAnswers),
}

impl AnswerSample {
    fn current(&self) -> &CurrentAnswers {
        match self {
            Self::Prepared { current, .. } | Self::Current(current) => current,
            _ => panic!("prepared typed writer"),
        }
    }

    fn previous(&self) -> &PreviousAnswers {
        match self {
            Self::Prepared { previous, .. } | Self::Previous(previous) => previous,
            _ => panic!("prepared frozen writer"),
        }
    }
}

impl serde::Serialize for AnswerSample {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Prepared { current, .. } | Self::Current(current) => {
                current.serialize(serializer)
            }
            Self::Previous(previous) => previous.serialize(serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for AnswerSample {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        PreviousAnswers::deserialize(deserializer).map(Self::Previous)
    }
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

fn previous_answers(db: &Db<'_>, message: Option<&str>) -> Vec<PrivateAnswer> {
    if db.0.is_autocommit() {
        let tx =
            rusqlite::Transaction::new_unchecked(db.0, rusqlite::TransactionBehavior::Deferred)
                .unwrap();
        let values = previous_answers(&Db(&tx), message);
        tx.commit().unwrap();
        return values;
    }
    let filter = if message.is_some() {
        "chat_id=?1 AND id=?2"
    } else {
        "chat_id=?1 AND ?2 IS NULL"
    };
    let query = format!(
        "SELECT id,chat_id,revision,text,provider,model,reasoning,speed,mode,status,created_at,question_id,answers FROM chat_messages WHERE {filter} ORDER BY created_at,rowid"
    );
    let values =
        db.0.prepare_cached(&query)
            .unwrap()
            .query_map(params![CHAT, message], |r| {
                let content = cairn_contracts::ChatMessage {
                    id: r.get(0)?,
                    chat_id: r.get(1)?,
                    revision: r.get(2)?,
                    text: r.get(3)?,
                    provider: r.get(4)?,
                    model: r.get(5)?,
                    reasoning: r.get(6)?,
                    speed: r.get(7)?,
                    mode: cairn_contracts::MessageMode::parse(&r.get::<_, String>(8)?).unwrap(),
                    status: cairn_contracts::MessageStatus::parse(&r.get::<_, String>(9)?).unwrap(),
                    created_at: r.get(10)?,
                    question_id: r.get(11)?,
                    attachments: vec![],
                };
                let raw: String = r.get(12)?;
                Ok(PrivateAnswer {
                    id: content.id,
                    text: content.text,
                    answers: AnswerSample::Previous(serde_json::from_str(&raw).unwrap()),
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
    let attachment_filter = if message.is_some() {
        "m.chat_id=?1 AND m.id=?2"
    } else {
        "m.chat_id=?1 AND ?2 IS NULL"
    };
    let query = format!(
        "SELECT a.message_id,a.attachment_id,a.chat_id,a.name,a.size,a.media_type,a.kind,a.width,a.height,a.digest,a.created_at FROM message_attachments a JOIN chat_messages m ON m.id=a.message_id WHERE {attachment_filter} ORDER BY a.message_id,a.position"
    );
    assert!(
        !db.0
            .prepare_cached(&query)
            .unwrap()
            .exists(params![CHAT, message])
            .unwrap(),
        "This workload has no attachments."
    );
    values
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
        let answers = self.answers.current();
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
        db.execute_batch("DROP TABLE question_legacy_answer_items; DROP TABLE question_answer_choices; DROP TABLE question_answers; DROP TABLE question_answer_deliveries; ALTER TABLE chat_messages ADD COLUMN answers TEXT CHECK(answers IS NULL OR json_valid(answers));").unwrap();
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        previous_answers(db, Some(id)).pop().unwrap()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        previous_answers(db, None).len()
    }

    fn reference_write(&self, db: &Db<'_>, _: bool) {
        db.0.execute_batch("SAVEPOINT conversation_message")
            .unwrap();
        db.0.prepare_cached("INSERT INTO chat_messages(id,chat_id,revision,text,provider,model,reasoning,speed,mode,status,created_at,question_id,answers) VALUES(?,?,?,?,'codex','model','high','','queue','queued',1700000000000,'question',?) ON CONFLICT(id) DO UPDATE SET text=excluded.text,answers=excluded.answers,revision=excluded.revision").unwrap().execute(params![self.id,CHAT,cairn_installation::config::id(),self.text,serde_json::to_string(self.answers.previous()).unwrap()]).unwrap();
        db.0.execute(
            "DELETE FROM message_attachments WHERE message_id=?",
            [&self.id],
        )
        .unwrap();
        db.0.execute_batch("RELEASE conversation_message").unwrap();
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
                        if n % 4 == 0 && f < 2 {
                            cairn_installation::chats::MessageAnswer::Legacy(if f == 0 {
                                vec![
                                    "Synthetic historical free text. ".repeat(8),
                                    "Second choice".into(),
                                    "Synthetic historical free text. ".repeat(8),
                                ]
                            } else {
                                vec![]
                            })
                        } else {
                            cairn_installation::chats::MessageAnswer::Current(
                                cairn_contracts::QuestionAnswer {
                                    selected: vec!["second".into(), "first".into()],
                                    comment: "Synthetic private questionnaire answer. ".repeat(12),
                                },
                            )
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
                answers: AnswerSample::Prepared {
                    previous: serde_json::from_value(serde_json::to_value(&answers).unwrap())
                        .unwrap(),
                    current: answers,
                },
            }
        })
        .collect::<Vec<_>>();
    measure(&values);
}
