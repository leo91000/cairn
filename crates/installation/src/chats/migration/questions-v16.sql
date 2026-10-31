CREATE TABLE chat_questions(
    key INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE CHECK(length(id)>0),
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
