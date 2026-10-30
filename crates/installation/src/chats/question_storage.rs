//! Questionnaire SQL; answer material is owned separately from public message views.
use super::storage::MessageAnswer;
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::{ChatQuestion, QuestionField, QuestionOption, QuestionStatus};
use rusqlite::params;
use std::collections::{BTreeMap, HashMap, HashSet};

type AnswersByMessage = HashMap<String, BTreeMap<String, MessageAnswer>>;

pub(crate) const SCHEMA: &str = "
CREATE TABLE chat_questions(
    key INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE CHECK(length(id)>0),
    chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    blocking INTEGER NOT NULL CHECK(blocking IN (0,1)),
    source TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','answering','answered','cancelled')),
    created_at INTEGER CHECK(created_at BETWEEN 0 AND 9007199254740991),
    message_id TEXT,
    plan INTEGER NOT NULL DEFAULT 0 CHECK(plan IN (0,1))
);
CREATE INDEX chat_questions_chat ON chat_questions(chat_id,created_at,id);
CREATE INDEX chat_questions_run ON chat_questions(run_id,status);
-- Opaque metadata from retired engine questions is retained privately on import.
-- It never supplies current question definitions or public response material.
CREATE TABLE question_import_metadata(
    question_id TEXT PRIMARY KEY NOT NULL REFERENCES chat_questions(id) ON DELETE CASCADE,
    expires INTEGER,
    metadata TEXT NOT NULL CHECK(json_valid(metadata) AND json_type(metadata)='object')
) WITHOUT ROWID;
CREATE TABLE question_fields(
    question_key INTEGER NOT NULL REFERENCES chat_questions(key) ON DELETE CASCADE,
    id TEXT NOT NULL CHECK(length(id) BETWEEN 1 AND 200),
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 19),
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 10000),
    secret INTEGER NOT NULL CHECK(secret IN (0,1)),
    mode TEXT NOT NULL CHECK(mode IN ('single','multiple')),
    min_choices INTEGER CHECK(min_choices BETWEEN 1 AND 30),
    max_choices INTEGER CHECK(max_choices BETWEEN 1 AND 30),
    CHECK(min_choices IS NULL OR max_choices IS NULL OR min_choices<=max_choices),
    CHECK(mode!='single' OR max_choices IS NULL OR max_choices=1),
    PRIMARY KEY(question_key,id), UNIQUE(question_key,position)
) WITHOUT ROWID;
CREATE TABLE question_options(
    question_key INTEGER NOT NULL,
    field_id TEXT NOT NULL,
    id TEXT NOT NULL CHECK(length(id)<=200),
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 29),
    label TEXT NOT NULL CHECK(length(label) BETWEEN 1 AND 2000),
    description TEXT NOT NULL CHECK(length(description)<=4000),
    recommended INTEGER NOT NULL CHECK(recommended IN (0,1)),
    PRIMARY KEY(question_key,field_id,position),
    FOREIGN KEY(question_key,field_id) REFERENCES question_fields(question_key,id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE question_answers(
    message_id TEXT NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    field_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('current','legacy')),
    comment TEXT,
    CHECK((kind='current' AND comment IS NOT NULL AND length(comment)<=10000)
       OR (kind='legacy' AND comment IS NULL)),
    PRIMARY KEY(message_id,field_id)
) WITHOUT ROWID;
CREATE TABLE question_answer_choices(
    message_id TEXT NOT NULL,
    field_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 29),
    option_id TEXT NOT NULL,
    PRIMARY KEY(message_id,field_id,position),
    FOREIGN KEY(message_id,field_id) REFERENCES question_answers(message_id,field_id) ON DELETE CASCADE
) WITHOUT ROWID;
-- Retired responses mixed selections and free text in one string list. Keep
-- that list separately instead of inventing current choice/comment identities.
CREATE TABLE question_legacy_answer_items(
    message_id TEXT NOT NULL,
    field_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK(position>=0),
    text TEXT NOT NULL,
    PRIMARY KEY(message_id,field_id,position),
    FOREIGN KEY(message_id,field_id) REFERENCES question_answers(message_id,field_id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE question_answer_deliveries(
    message_id TEXT PRIMARY KEY NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    text TEXT NOT NULL,
    private INTEGER NOT NULL CHECK(private IN (0,1))
);
";

fn question_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatQuestion> {
    let status: String = row.get(5)?;
    let status = match status.as_str() {
        "pending" => QuestionStatus::Pending,
        "answering" => QuestionStatus::Answering,
        "answered" => QuestionStatus::Answered,
        "cancelled" => QuestionStatus::Cancelled,
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Invalid question status",
                )),
            ));
        }
    };
    Ok(ChatQuestion {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        run_id: row.get(2)?,
        blocking: row.get(3)?,
        source: row.get(4)?,
        status,
        created_at: row.get(6)?,
        message_id: row.get(7)?,
        fields: Vec::new(),
    })
}

impl Db<'_> {
    pub(super) fn private_answer_message(&self, message: &str) -> Result<bool> {
        Ok(self.0.prepare_cached("SELECT EXISTS(SELECT 1 FROM question_answer_deliveries WHERE message_id=? AND private=1)")?
            .query_row([message], |row| row.get(0))?)
    }

    /// Marks a question asked by a plan turn, whose answer stays a plan request.
    pub(super) fn mark_plan_question(&self, chat: &str, id: &str) -> Result<()> {
        self.0
            .prepare_cached("UPDATE chat_questions SET plan=1 WHERE chat_id=? AND id=?")?
            .execute(params![chat, id])?;
        Ok(())
    }

    pub(super) fn plan_question(&self, chat: &str, id: &str) -> Result<bool> {
        Ok(self
            .0
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM chat_questions WHERE chat_id=? AND id=? AND plan=1)",
            )?
            .query_row(params![chat, id], |row| row.get(0))?)
    }

    pub fn questions(&self, chat: &str) -> Result<Vec<ChatQuestion>> {
        self.load_questions(chat)
    }

    pub fn question(&self, chat: &str, id: &str) -> Result<Option<ChatQuestion>> {
        // A point read gets one atomic definition, including ordered children.
        // Internal integer keys keep the external digest out of every child key.
        let query="SELECT q.id,q.chat_id,q.run_id,q.blocking,q.source,q.status,q.created_at,q.message_id,
            f.id,f.position,f.title,f.secret,f.mode,f.min_choices,f.max_choices,
            o.position,o.id,o.label,o.description,o.recommended
            FROM chat_questions q
            LEFT JOIN question_fields f ON f.question_key=q.key
            LEFT JOIN question_options o ON o.question_key=f.question_key AND o.field_id=f.id
            WHERE q.chat_id=?1 AND q.id=?2
              AND NOT EXISTS(SELECT 1 FROM question_import_metadata m WHERE m.question_id=q.id AND m.expires<=?3)
            ORDER BY f.position,o.position";
        let mut statement = self.0.prepare_cached(query)?;
        let mut rows = statement.query(params![chat, id, crate::config::now()])?;
        let mut loaded: Option<ChatQuestion> = None;
        let mut field_position = None;
        while let Some(row) = rows.next()? {
            if loaded.is_none() {
                loaded = Some(question_row(row)?);
            }
            let question = loaded.as_mut().unwrap();
            let position: Option<i64> = row.get(9)?;
            if position.is_none() {
                continue;
            }
            if position != field_position {
                question.fields.push(QuestionField {
                    id: row.get(8)?,
                    title: row.get(10)?,
                    secret: row.get(11)?,
                    mode: row.get(12)?,
                    min_choices: row.get::<_, Option<u32>>(13)?.map(|n| n as usize),
                    max_choices: row.get::<_, Option<u32>>(14)?.map(|n| n as usize),
                    options: Vec::new(),
                });
                field_position = position;
            }
            if row.get::<_, Option<i64>>(15)?.is_some() {
                question
                    .fields
                    .last_mut()
                    .unwrap()
                    .options
                    .push(QuestionOption {
                        id: row.get(16)?,
                        label: row.get(17)?,
                        description: row.get(18)?,
                        recommended: row.get(19)?,
                    });
            }
        }
        Ok(loaded)
    }

    fn load_questions(&self, chat: &str) -> Result<Vec<ChatQuestion>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let result = Db(&tx).load_questions(chat)?;
            tx.commit()?;
            return Ok(result);
        }
        let time = crate::config::now();
        let filter = "q.chat_id=?1 AND NOT EXISTS(SELECT 1 FROM question_import_metadata m WHERE m.question_id=q.id AND m.expires<=?2)";
        let mut questions=self.0.prepare_cached(&format!("SELECT q.id,q.chat_id,q.run_id,q.blocking,q.source,q.status,q.created_at,q.message_id FROM chat_questions q WHERE {filter} ORDER BY q.created_at,q.id"))?.query_map(params![chat,time],question_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        if questions.is_empty() {
            return Ok(questions);
        }
        let mut options = HashMap::<(String, String), Vec<QuestionOption>>::new();
        let mut statement=self.0.prepare_cached(&format!("SELECT q.id,o.field_id,o.id,o.label,o.description,o.recommended FROM question_options o JOIN chat_questions q ON q.key=o.question_key WHERE {filter} ORDER BY q.id,o.field_id,o.position"))?;
        for value in statement.query_map(params![chat, time], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                QuestionOption {
                    id: row.get(2)?,
                    label: row.get(3)?,
                    description: row.get(4)?,
                    recommended: row.get(5)?,
                },
            ))
        })? {
            let (question, field, option) = value?;
            options.entry((question, field)).or_default().push(option);
        }
        let mut fields = HashMap::<String, Vec<QuestionField>>::new();
        let mut statement=self.0.prepare_cached(&format!("SELECT q.id,f.id,f.title,f.secret,f.mode,f.min_choices,f.max_choices FROM question_fields f JOIN chat_questions q ON q.key=f.question_key WHERE {filter} ORDER BY q.id,f.position"))?;
        for value in statement.query_map(params![chat, time], |row| {
            Ok((
                row.get::<_, String>(0)?,
                QuestionField {
                    id: row.get(1)?,
                    title: row.get(2)?,
                    secret: row.get(3)?,
                    mode: row.get(4)?,
                    min_choices: row.get::<_, Option<u32>>(5)?.map(|n| n as usize),
                    max_choices: row.get::<_, Option<u32>>(6)?.map(|n| n as usize),
                    options: Vec::new(),
                },
            ))
        })? {
            let (question, mut field) = value?;
            field.options = options
                .remove(&(question.clone(), field.id.clone()))
                .unwrap_or_default();
            fields.entry(question).or_default().push(field);
        }
        for question in &mut questions {
            question.fields = fields.remove(&question.id).unwrap_or_default();
        }
        Ok(questions)
    }

    pub fn save_question(&self, question: &ChatQuestion) -> Result<()> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            Db(&tx).save_question(question)?;
            tx.commit()?;
            return Ok(());
        }
        self.0.execute_batch("SAVEPOINT question_write")?;
        let before = self.0.total_changes();
        let discarded_before = self.discarded_change_count()?;
        let result = self.write_question(question, true);
        match result {
            Ok(()) => self.0.execute_batch("RELEASE question_write")?,
            Err(error) => {
                self.0
                    .execute_batch("ROLLBACK TO question_write; RELEASE question_write")?;
                self.discard_savepoint_changes(before, discarded_before)?;
                return Err(error);
            }
        }
        Ok(())
    }

    /// The only caller is the atomic v15 importer. Retired engine options had
    /// no selection identity; retain their labels/order without inventing one.
    pub(super) fn import_question(&self, question: &ChatQuestion) -> Result<()> {
        self.write_question(question, question.source == "cairn")
    }

    fn write_question(&self, q: &ChatQuestion, validate_new: bool) -> Result<()> {
        self.0.execute("DELETE FROM chat_questions WHERE id=?1 AND EXISTS(SELECT 1 FROM question_import_metadata m WHERE m.question_id=?1 AND m.expires<=?2)", params![q.id,crate::config::now()])?;
        let existing = self.question(&q.chat_id, &q.id)?;
        // Imported retired-engine definitions can have label-only options. Their
        // delivery status must still advance without inventing selection IDs.
        if existing.is_none() && validate_new {
            cairn_contracts::validate_question_fields(&q.fields).map_err(Error::bad)?;
        }
        let question_key:i64=self.0.prepare_cached("INSERT INTO chat_questions(id,chat_id,run_id,blocking,source,status,created_at,message_id) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET blocking=excluded.blocking,status=excluded.status,message_id=excluded.message_id WHERE chat_questions.chat_id=excluded.chat_id AND chat_questions.run_id=excluded.run_id RETURNING key")?
            .query_row(params![q.id,q.chat_id,q.run_id,q.blocking,q.source,q.status.as_str(),q.created_at,q.message_id],|row|row.get(0))?;
        if let Some(existing) = existing {
            if existing.fields != q.fields
                || existing.run_id != q.run_id
                || existing.source != q.source
                || existing.created_at != q.created_at
            {
                return Err(Error::conflict("Question definitions are immutable."));
            }
            return Ok(());
        }
        for (position, field) in q.fields.iter().enumerate() {
            self.0
                .prepare_cached("INSERT INTO question_fields VALUES(?,?,?,?,?,?,?,?)")?
                .execute(params![
                    question_key,
                    field.id,
                    position as i64,
                    field.title,
                    field.secret,
                    field.mode,
                    field.min_choices.map(|n| n as i64),
                    field.max_choices.map(|n| n as i64)
                ])?;
            for (position, option) in field.options.iter().enumerate() {
                self.0
                    .prepare_cached("INSERT INTO question_options VALUES(?,?,?,?,?,?,?)")?
                    .execute(params![
                        question_key,
                        field.id,
                        option.id,
                        position as i64,
                        option.label,
                        option.description,
                        option.recommended
                    ])?;
            }
        }
        Ok(())
    }
}

impl Db<'_> {
    /// Visibility belongs to the saved reply, even after a definition expires,
    /// is purged, or its request identifier is reused for another question.
    pub(super) fn private_answer_messages(&self, chat: &str) -> Result<HashSet<String>> {
        Ok(self.0.prepare_cached("SELECT d.message_id FROM question_answer_deliveries d JOIN chat_messages m ON m.id=d.message_id WHERE m.chat_id=? AND d.private=1")?
            .query_map([chat], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?)
    }

    pub(super) fn message_answers(
        &self,
        chat: &str,
        message: Option<&str>,
    ) -> Result<(AnswersByMessage, HashMap<String, String>)> {
        let filter = if message.is_some() {
            "m.chat_id=?1 AND m.id=?2"
        } else {
            "m.chat_id=?1 AND ?2 IS NULL"
        };
        let deliveries = self.0.prepare_cached(&format!("SELECT d.message_id,d.text FROM question_answer_deliveries d JOIN chat_messages m ON m.id=d.message_id WHERE {filter}"))?
            .query_map(params![chat,message],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<HashMap<_,_>>>()?;
        if deliveries.is_empty() {
            return Ok((HashMap::new(), deliveries));
        }

        let mut answers: AnswersByMessage = deliveries
            .keys()
            .map(|id| (id.clone(), BTreeMap::new()))
            .collect();
        let mut has_current = false;
        let mut has_legacy = false;
        let mut statement = self.0.prepare_cached(&format!("SELECT a.message_id,a.field_id,a.kind,a.comment FROM question_answers a JOIN chat_messages m ON m.id=a.message_id WHERE {filter}"))?;
        for value in statement.query_map(params![chat, message], |row| {
            let kind: String = row.get(2)?;
            let answer = match kind.as_str() {
                "current" => MessageAnswer::Current(cairn_contracts::QuestionAnswer {
                    selected: Vec::new(),
                    comment: row.get(3)?,
                }),
                "legacy" => MessageAnswer::Legacy(Vec::new()),
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, answer))
        })? {
            let (message, field, answer) = value?;
            has_current |= matches!(&answer, MessageAnswer::Current(_));
            has_legacy |= matches!(&answer, MessageAnswer::Legacy(_));
            answers.entry(message).or_default().insert(field, answer);
        }

        if has_current {
            let mut statement = self.0.prepare_cached(&format!("SELECT c.message_id,c.field_id,c.option_id FROM question_answer_choices c JOIN chat_messages m ON m.id=c.message_id WHERE {filter} ORDER BY c.message_id,c.field_id,c.position"))?;
            for value in statement.query_map(params![chat, message], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })? {
                let (message, field, option) = value?;
                let Some(MessageAnswer::Current(answer)) = answers
                    .get_mut(&message)
                    .and_then(|fields| fields.get_mut(&field))
                else {
                    return Err(rusqlite::Error::InvalidQuery.into());
                };
                answer.selected.push(option);
            }
        }
        if has_legacy {
            let mut statement = self.0.prepare_cached(&format!("SELECT a.message_id,a.field_id,a.text FROM question_legacy_answer_items a JOIN chat_messages m ON m.id=a.message_id WHERE {filter} ORDER BY a.message_id,a.field_id,a.position"))?;
            for value in statement.query_map(params![chat, message], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })? {
                let (message, field, text) = value?;
                let Some(MessageAnswer::Legacy(items)) = answers
                    .get_mut(&message)
                    .and_then(|fields| fields.get_mut(&field))
                else {
                    return Err(rusqlite::Error::InvalidQuery.into());
                };
                items.push(text);
            }
        }
        Ok((answers, deliveries))
    }

    pub(super) fn save_message_answers(
        &self,
        message: &str,
        answers: Option<&BTreeMap<String, MessageAnswer>>,
        text: &str,
    ) -> Result<()> {
        // Preserve a reply's original classification across rewrites. Missing
        // historical definitions cannot authorize revealing delivery material.
        let private = if answers.is_some() {
            self.0.prepare_cached("SELECT COALESCE((SELECT private FROM question_answer_deliveries WHERE message_id=?1), (SELECT CASE WHEN q.id IS NULL OR NOT EXISTS(SELECT 1 FROM question_fields f WHERE f.question_key=q.key) THEN 1 ELSE EXISTS(SELECT 1 FROM question_fields f WHERE f.question_key=q.key AND f.secret=1) END FROM chat_messages m LEFT JOIN chat_questions q ON q.id=m.question_id AND q.chat_id=m.chat_id WHERE m.id=?1))")?
                .query_row([message], |row| row.get::<_, bool>(0))?
        } else {
            false
        };
        self.0
            .execute("DELETE FROM question_answers WHERE message_id=?", [message])?;
        self.0.execute(
            "DELETE FROM question_answer_deliveries WHERE message_id=?",
            [message],
        )?;
        let Some(answers) = answers else {
            return Ok(());
        };
        self.0.execute(
            "INSERT INTO question_answer_deliveries VALUES(?,?,?)",
            params![message, text, private],
        )?;
        for (field, answer) in answers {
            let (kind, comment) = match answer {
                MessageAnswer::Current(answer) => ("current", Some(answer.comment.as_str())),
                MessageAnswer::Legacy(_) => ("legacy", None),
            };
            self.0.execute(
                "INSERT INTO question_answers VALUES(?,?,?,?)",
                params![message, field, kind, comment],
            )?;
            match answer {
                MessageAnswer::Current(answer) => {
                    for (position, option) in answer.selected.iter().enumerate() {
                        self.0.execute(
                            "INSERT INTO question_answer_choices VALUES(?,?,?,?)",
                            params![message, field, position as i64, option],
                        )?;
                    }
                }
                MessageAnswer::Legacy(items) => {
                    for (position, text) in items.iter().enumerate() {
                        self.0.execute(
                            "INSERT INTO question_legacy_answer_items VALUES(?,?,?,?)",
                            params![message, field, position as i64, text],
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}
