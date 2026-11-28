//! B4 reuses A1/A2 clocks, distributions, connections and alternating layout runs.
//! Compare v39 chat read marks and snoozes with the equivalent v38 rows.
use super::*;
use cairn_installation::reads::sql::StoredSnooze;

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000a1";
/// Another member's state shares the tables and KV range the account reads.
const OTHER: &str = "00000000-0000-4000-8000-0000000000a2";

fn previous_schema(db: &Connection) {
    db.execute_batch(
        "DROP TABLE account_reads; DROP TABLE account_snoozes;
         CREATE TABLE account_reads(account_id TEXT NOT NULL, kind TEXT NOT NULL,
             item_id TEXT NOT NULL, finished_at INTEGER NOT NULL,
             PRIMARY KEY(account_id,kind,item_id)) WITHOUT ROWID;
         CREATE TABLE account_dismissals(account_id TEXT NOT NULL, task_id TEXT NOT NULL,
             finished_at INTEGER NOT NULL, snoozed_until INTEGER,
             PRIMARY KEY(account_id,task_id)) WITHOUT ROWID;",
    )
    .unwrap();
}

fn candidate(db: &Connection) -> bool {
    db.table_exists(None, "account_snoozes").unwrap()
}

fn item(n: u128) -> String {
    uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n).to_string()
}

/// A conversation read mark.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
struct Mark {
    key: String,
    finished_at: i64,
}

impl StorageEntity for Mark {
    const KIND: &'static str = "reads";
    const TABLE: &'static str = "account_reads";
    const SAVE: &'static str = "read.saved";

    fn id(&self) -> String {
        self.key.clone()
    }

    fn setup(db: &Connection) {
        let db = Db(db);
        for n in 0..1000 {
            let key = item(n);
            if candidate(db.0) {
                db.mark_read(OTHER, &key, 1_761_800_000_000).unwrap();
            } else {
                db.0.execute(
                    "INSERT INTO account_reads VALUES(?1,'chat',?2,?3)",
                    params![OTHER, key, 1_761_800_000_000_i64],
                )
                .unwrap();
            }
        }
    }

    fn reference_schema(db: &Connection) {
        previous_schema(db);
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        let finished_at =
            db.0.prepare_cached(
                "SELECT finished_at FROM account_reads WHERE account_id=? AND chat_id=?",
            )
            .unwrap()
            .query_row(params![ACCOUNT, id], |r| r.get(0))
            .unwrap();
        Self {
            key: id.into(),
            finished_at,
        }
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        Self {
            key: id.into(),
            finished_at: db.0.prepare_cached(
                "SELECT finished_at FROM account_reads WHERE account_id=? AND kind='chat' AND item_id=?",
            ).unwrap().query_row(params![ACCOUNT, id], |r| r.get(0)).unwrap(),
        }
    }

    fn list(db: &Db<'_>) -> usize {
        db.read_marks(ACCOUNT).unwrap().len()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        let mut statement = db
            .0
            .prepare_cached("SELECT kind,item_id,finished_at FROM account_reads WHERE account_id=?")
            .unwrap();
        statement
            .query_map([ACCOUNT], |row| {
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
            .unwrap()
            .len()
    }

    fn write(&self, db: &Db<'_>, _replacement: bool) {
        db.mark_read(ACCOUNT, &self.key, self.finished_at).unwrap();
        db.audit(Self::SAVE, &json!({ "id": self.key })).unwrap();
    }

    fn reference_write(&self, db: &Db<'_>, _replacement: bool) {
        db.0.prepare_cached(
            "INSERT INTO account_reads VALUES(?1,'chat',?2,?3) ON CONFLICT DO UPDATE
             SET finished_at=max(finished_at,excluded.finished_at)",
        )
        .unwrap()
        .execute(params![ACCOUNT, self.key, self.finished_at])
        .unwrap();
    }

    fn delete_candidate(db: &Db<'_>, id: &str) {
        db.0.prepare_cached("DELETE FROM account_reads WHERE account_id=? AND chat_id=?")
            .unwrap()
            .execute(params![ACCOUNT, id])
            .unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.prepare_cached(
            "DELETE FROM account_reads WHERE account_id=? AND kind='chat' AND item_id=?",
        )
        .unwrap()
        .execute(params![ACCOUNT, id])
        .unwrap();
    }

    fn encoded_bytes(db: &Db<'_>) -> Option<i64> {
        Some(
            db.0.query_row(
                "SELECT sum(payload) FROM dbstat WHERE name='account_reads'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        )
    }
}

/// The same dated reminder under the legacy mission and conversation keys.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
struct Aside {
    chat_id: String,
    finished_at: i64,
    until: i64,
}

impl StorageEntity for Aside {
    const KIND: &'static str = "snoozes";
    const TABLE: &'static str = "account_snoozes";
    const SAVE: &'static str = "snooze.saved";

    fn id(&self) -> String {
        self.chat_id.clone()
    }

    fn setup(db: &Connection) {
        let db = Db(db);
        for n in 0..1000 {
            let snooze = StoredSnooze {
                finished_at: 1_761_800_000_000,
                until: 1_761_886_400_000,
            };
            if candidate(db.0) {
                db.set_snooze(OTHER, &item(n), Some(snooze)).unwrap();
            } else {
                db.0.execute(
                    "INSERT INTO account_dismissals VALUES(?,?,?,?)",
                    params![OTHER, item(n), snooze.finished_at, snooze.until],
                )
                .unwrap();
            }
        }
    }

    fn reference_schema(db: &Connection) {
        previous_schema(db);
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        let (finished_at, until) = db.0.prepare_cached(
            "SELECT finished_at,snoozed_until FROM account_snoozes WHERE account_id=? AND chat_id=?",
        ).unwrap().query_row(params![ACCOUNT, id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        Self {
            chat_id: id.into(),
            finished_at,
            until,
        }
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        let (finished_at, until) = db.0.prepare_cached(
            "SELECT finished_at,snoozed_until FROM account_dismissals WHERE account_id=? AND task_id=?",
        ).unwrap().query_row(params![ACCOUNT, id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        Self {
            chat_id: id.into(),
            finished_at,
            until,
        }
    }

    fn list(db: &Db<'_>) -> usize {
        db.snoozes(ACCOUNT).unwrap().len()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        let mut statement = db.0.prepare_cached(
            "SELECT task_id,finished_at,snoozed_until FROM account_dismissals WHERE account_id=?",
        ).unwrap();
        statement
            .query_map([ACCOUNT], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get::<_, i64>(1)?, row.get::<_, Option<i64>>(2)?),
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
            .unwrap()
            .len()
    }

    fn write(&self, db: &Db<'_>, _replacement: bool) {
        let snooze = StoredSnooze {
            finished_at: self.finished_at,
            until: self.until,
        };
        db.set_snooze(ACCOUNT, &self.chat_id, Some(snooze)).unwrap();
        db.audit(Self::SAVE, &json!({ "id": self.chat_id }))
            .unwrap();
    }

    fn reference_write(&self, db: &Db<'_>, _replacement: bool) {
        db.0.prepare_cached(
            "INSERT INTO account_dismissals VALUES(?,?,?,?) ON CONFLICT DO UPDATE
             SET finished_at=excluded.finished_at,snoozed_until=excluded.snoozed_until",
        )
        .unwrap()
        .execute(params![ACCOUNT, self.chat_id, self.finished_at, self.until])
        .unwrap();
    }

    fn delete_candidate(db: &Db<'_>, id: &str) {
        db.set_snooze(ACCOUNT, id, None).unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.0.prepare_cached("DELETE FROM account_dismissals WHERE account_id=? AND task_id=?")
            .unwrap()
            .execute(params![ACCOUNT, id])
            .unwrap();
    }

    fn encoded_bytes(db: &Db<'_>) -> Option<i64> {
        let table = if candidate(db.0) {
            Self::TABLE
        } else {
            "account_dismissals"
        };
        Some(
            db.0.query_row(
                "SELECT sum(payload) FROM dbstat WHERE name=?",
                [table],
                |row| row.get(0),
            )
            .unwrap(),
        )
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn read_mark_storage_layouts() {
    let marks: Vec<Mark> = (0..1000)
        .map(|n| Mark {
            key: item(10_000 + n),
            finished_at: 1_761_800_000_000 + n as i64,
        })
        .collect();
    measure(&marks);
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn snooze_storage_layouts() {
    let asides: Vec<Aside> = (0..1000)
        .map(|n| Aside {
            chat_id: item(10_000 + n),
            finished_at: 1_761_800_000_000 + n as i64,
            until: 1_761_886_400_000,
        })
        .collect();
    measure(&asides);
}
