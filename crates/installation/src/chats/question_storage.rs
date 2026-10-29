//! Questionnaire SQL; answer material is owned separately from public message views.
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::{ChatQuestion, QuestionField, QuestionOption};
use rusqlite::params;
use std::collections::HashMap;

pub(crate) const SCHEMA: &str = "
CREATE TABLE chat_questions(
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)>0),
    chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    blocking INTEGER NOT NULL CHECK(blocking IN (0,1)),
    source TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','answering','answered','cancelled')),
    created_at INTEGER CHECK(created_at BETWEEN 0 AND 9007199254740991),
    message_id TEXT
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
    question_id TEXT NOT NULL REFERENCES chat_questions(id) ON DELETE CASCADE,
    id TEXT NOT NULL CHECK(length(id) BETWEEN 1 AND 200),
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 19),
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 10000),
    secret INTEGER NOT NULL CHECK(secret IN (0,1)),
    mode TEXT NOT NULL CHECK(mode IN ('single','multiple')),
    min_choices INTEGER CHECK(min_choices BETWEEN 1 AND 30),
    max_choices INTEGER CHECK(max_choices BETWEEN 1 AND 30),
    CHECK(min_choices IS NULL OR max_choices IS NULL OR min_choices<=max_choices),
    CHECK(mode!='single' OR max_choices IS NULL OR max_choices=1),
    PRIMARY KEY(question_id,id), UNIQUE(question_id,position)
) WITHOUT ROWID;
CREATE TABLE question_options(
    question_id TEXT NOT NULL,
    field_id TEXT NOT NULL,
    id TEXT NOT NULL CHECK(length(id)<=200),
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 29),
    label TEXT NOT NULL CHECK(length(label) BETWEEN 1 AND 2000),
    description TEXT NOT NULL CHECK(length(description)<=4000),
    recommended INTEGER NOT NULL CHECK(recommended IN (0,1)),
    PRIMARY KEY(question_id,field_id,position),
    FOREIGN KEY(question_id,field_id) REFERENCES question_fields(question_id,id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE question_answers(
    message_id TEXT NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    field_id TEXT NOT NULL,
    comment TEXT NOT NULL CHECK(length(comment)<=10000),
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
CREATE TABLE question_answer_deliveries(
    message_id TEXT PRIMARY KEY NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    text TEXT NOT NULL
) WITHOUT ROWID;
";

impl Db<'_> {
    pub fn questions(&self, chat: &str) -> Result<Vec<ChatQuestion>> {
        self.load_questions(chat, None)
    }

    pub fn question(&self, chat: &str, id: &str) -> Result<Option<ChatQuestion>> {
        Ok(self.load_questions(chat, Some(id))?.pop())
    }

    fn load_questions(&self, chat: &str, id: Option<&str>) -> Result<Vec<ChatQuestion>> {
        if self.0.is_autocommit() {
            let tx = rusqlite::Transaction::new_unchecked(
                self.0,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let result = Db(&tx).load_questions(chat, id)?;
            tx.commit()?;
            return Ok(result);
        }
        let identity = if id.is_some() {
            "q.chat_id=?1 AND q.id=?2"
        } else {
            "q.chat_id=?1 AND ?2 IS NULL"
        };
        let filter = format!(
            "{identity} AND NOT EXISTS(SELECT 1 FROM question_import_metadata m WHERE m.question_id=q.id AND m.expires<=?3)"
        );
        let mut questions = self.0.prepare_cached(&format!("SELECT q.id,q.chat_id,q.run_id,q.blocking,q.source,q.status,q.created_at,q.message_id FROM chat_questions q WHERE {filter} ORDER BY q.created_at,q.id"))?
            .query_map(params![chat,id,crate::config::now()], |r| {
                let status: String = r.get(5)?;
                Ok(ChatQuestion {
                    id:r.get(0)?,chat_id:r.get(1)?,run_id:r.get(2)?,blocking:r.get(3)?,source:r.get(4)?,
                    status:serde_json::from_value(status.into()).map_err(|e| rusqlite::Error::FromSqlConversionFailure(5,rusqlite::types::Type::Text,Box::new(e)))?,
                    created_at:r.get(6)?,message_id:r.get(7)?,fields:Vec::new(),
                })
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut options = HashMap::<(String, String), Vec<QuestionOption>>::new();
        let mut statement = self.0.prepare_cached(&format!("SELECT o.question_id,o.field_id,o.id,o.label,o.description,o.recommended FROM question_options o JOIN chat_questions q ON q.id=o.question_id WHERE {filter} ORDER BY o.question_id,o.field_id,o.position"))?;
        for value in statement.query_map(params![chat, id, crate::config::now()], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                QuestionOption {
                    id: r.get(2)?,
                    label: r.get(3)?,
                    description: r.get(4)?,
                    recommended: r.get(5)?,
                },
            ))
        })? {
            let (question, field, option) = value?;
            options.entry((question, field)).or_default().push(option);
        }
        let mut fields = HashMap::<String, Vec<QuestionField>>::new();
        let mut statement = self.0.prepare_cached(&format!("SELECT f.question_id,f.id,f.title,f.secret,f.mode,f.min_choices,f.max_choices FROM question_fields f JOIN chat_questions q ON q.id=f.question_id WHERE {filter} ORDER BY f.question_id,f.position"))?;
        for value in statement.query_map(params![chat, id, crate::config::now()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                QuestionField {
                    id: r.get(1)?,
                    title: r.get(2)?,
                    secret: r.get(3)?,
                    mode: r.get(4)?,
                    min_choices: r.get::<_, Option<u32>>(5)?.map(|n| n as usize),
                    max_choices: r.get::<_, Option<u32>>(6)?.map(|n| n as usize),
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
        cairn_contracts::validate_question_fields(&question.fields).map_err(Error::bad)?;
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
        let result = self.write_question(question);
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
        if question.source == "cairn" {
            cairn_contracts::validate_question_fields(&question.fields).map_err(Error::bad)?;
        }
        self.write_question(question)
    }

    fn write_question(&self, q: &ChatQuestion) -> Result<()> {
        self.0.execute("DELETE FROM chat_questions WHERE id=?1 AND EXISTS(SELECT 1 FROM question_import_metadata m WHERE m.question_id=?1 AND m.expires<=?2)", params![q.id,crate::config::now()])?;
        let existing = self.question(&q.chat_id, &q.id)?;
        self.0.prepare_cached("INSERT INTO chat_questions VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET blocking=excluded.blocking,status=excluded.status,message_id=excluded.message_id WHERE chat_questions.chat_id=excluded.chat_id AND chat_questions.run_id=excluded.run_id")?
            .execute(params![q.id,q.chat_id,q.run_id,q.blocking,q.source,q.status.as_str(),q.created_at,q.message_id])?;
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
                    q.id,
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
                        q.id,
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
    pub(super) fn message_answers(
        &self,
        chat: &str,
        message: Option<&str>,
    ) -> Result<(
        HashMap<String, std::collections::BTreeMap<String, cairn_contracts::QuestionAnswer>>,
        HashMap<String, String>,
    )> {
        let filter = if message.is_some() {
            "m.chat_id=?1 AND m.id=?2"
        } else {
            "m.chat_id=?1 AND ?2 IS NULL"
        };
        let deliveries=self.0.prepare_cached(&format!("SELECT d.message_id,d.text FROM question_answer_deliveries d JOIN chat_messages m ON m.id=d.message_id WHERE {filter}"))?
            .query_map(params![chat,message],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<HashMap<_,_>>>()?;
        let mut answers = deliveries
            .keys()
            .map(|id| {
                (
                    id.clone(),
                    std::collections::BTreeMap::<String, cairn_contracts::QuestionAnswer>::new(),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut statement=self.0.prepare_cached(&format!("SELECT a.message_id,a.field_id,a.comment FROM question_answers a JOIN chat_messages m ON m.id=a.message_id WHERE {filter}"))?;
        for value in statement.query_map(params![chat, message], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (message, field, comment) = value?;
            answers.entry(message).or_default().insert(
                field,
                cairn_contracts::QuestionAnswer {
                    selected: Vec::new(),
                    comment,
                },
            );
        }
        let mut statement=self.0.prepare_cached(&format!("SELECT c.message_id,c.field_id,c.option_id FROM question_answer_choices c JOIN chat_messages m ON m.id=c.message_id WHERE {filter} ORDER BY c.message_id,c.field_id,c.position"))?;
        for value in statement.query_map(params![chat, message], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (message, field, option) = value?;
            if let Some(answer) = answers.get_mut(&message).and_then(|a| a.get_mut(&field)) {
                answer.selected.push(option);
            }
        }
        Ok((answers, deliveries))
    }

    pub(super) fn save_message_answers(
        &self,
        message: &str,
        answers: Option<&std::collections::BTreeMap<String, cairn_contracts::QuestionAnswer>>,
        text: &str,
    ) -> Result<()> {
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
            "INSERT INTO question_answer_deliveries VALUES(?,?)",
            params![message, text],
        )?;
        for (field, answer) in answers {
            self.0.execute(
                "INSERT INTO question_answers VALUES(?,?,?)",
                params![message, field, answer.comment],
            )?;
            for (position, option) in answer.selected.iter().enumerate() {
                self.0.execute(
                    "INSERT INTO question_answer_choices VALUES(?,?,?,?)",
                    params![message, field, position as i64, option],
                )?;
            }
        }
        Ok(())
    }
}
