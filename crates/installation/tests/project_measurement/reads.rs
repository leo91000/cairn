//! B4 reuses A1/A2 clocks, distributions, connections and alternating layout runs.
//! Frozen KV mapping of read marks and set-aside outcomes: f489e1a5 (v17).
use super::*;
use cairn_installation::reads::sql::{ReadKind, StoredDismissal};

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000a1";
/// Another member's state shares the tables and KV range the account reads.
const OTHER: &str = "00000000-0000-4000-8000-0000000000a2";

fn previous_schema(db: &Connection) {
    db.execute_batch(
        "DROP TABLE account_read_starts; DROP TABLE account_reads; DROP TABLE account_dismissals;
DROP TABLE kv; CREATE TABLE kv(key TEXT PRIMARY KEY,data TEXT NOT NULL,expires INTEGER);",
    )
    .unwrap();
}

fn candidate(db: &Connection) -> bool {
    db.table_exists(None, "account_reads").unwrap()
}

fn item(n: u128) -> String {
    uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | n).to_string()
}

/// One account's read mark, by Fil key (`chat:<id>` or `task:<id>`).
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
struct Mark {
    key: String,
    finished_at: i64,
}

fn split(key: &str) -> (ReadKind, &str) {
    match key.split_once(':').unwrap() {
        ("chat", id) => (ReadKind::Chat, id),
        (_, id) => (ReadKind::Mission, id),
    }
}

fn kind(kind: ReadKind) -> &'static str {
    match kind {
        ReadKind::Chat => "chat",
        ReadKind::Mission => "mission",
    }
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
            let key = format!("{}:{}", ["chat", "task"][n as usize % 2], item(n));
            if candidate(db.0) {
                let (kind, id) = split(&key);
                db.mark_read(OTHER, kind, id, 1_761_800_000_000).unwrap();
            } else {
                db.set(
                    &format!("read:{OTHER}:{key}"),
                    &1_761_800_000_000_i64.into(),
                    None,
                )
                .unwrap();
            }
        }
    }

    fn reference_schema(db: &Connection) {
        previous_schema(db);
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        let (read_kind, item) = split(id);
        let finished_at =
            db.0.prepare_cached(
                "SELECT finished_at FROM account_reads WHERE account_id=? AND kind=? AND item_id=?",
            )
            .unwrap()
            .query_row(params![ACCOUNT, kind(read_kind), item], |r| r.get(0))
            .unwrap();
        Self {
            key: id.into(),
            finished_at,
        }
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        Self {
            key: id.into(),
            finished_at: db
                .kv_as::<i64>(&format!("read:{ACCOUNT}:{id}"))
                .unwrap()
                .unwrap(),
        }
    }

    fn list(db: &Db<'_>) -> usize {
        db.read_marks(ACCOUNT).unwrap().len()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        db.keys_as::<i64>(&format!("read:{ACCOUNT}:"))
            .unwrap()
            .len()
    }

    fn write(&self, db: &Db<'_>, _replacement: bool) {
        let (kind, id) = split(&self.key);
        db.mark_read(ACCOUNT, kind, id, self.finished_at).unwrap();
        db.audit(Self::SAVE, &json!({ "id": self.key })).unwrap();
    }

    /// The frozen monotonic overwrite: read the previous mark, keep the newest.
    fn reference_write(&self, db: &Db<'_>, _replacement: bool) {
        let key = format!("read:{ACCOUNT}:{}", self.key);
        let previous = db.kv_as::<i64>(&key).unwrap().unwrap_or(0);
        db.set(&key, &previous.max(self.finished_at).into(), None)
            .unwrap();
    }

    fn delete_candidate(db: &Db<'_>, id: &str) {
        let (read_kind, item) = split(id);
        db.0.prepare_cached(
            "DELETE FROM account_reads WHERE account_id=? AND kind=? AND item_id=?",
        )
        .unwrap()
        .execute(params![ACCOUNT, kind(read_kind), item])
        .unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.delete(&format!("read:{ACCOUNT}:{id}")).unwrap();
    }

    fn encoded_bytes(db: &Db<'_>) -> Option<i64> {
        let sql = if candidate(db.0) {
            "SELECT sum(payload) FROM dbstat WHERE name='account_reads'"
        } else {
            "SELECT sum(payload) FROM dbstat WHERE name IN ('kv','sqlite_autoindex_kv_1')"
        };
        Some(db.0.query_row(sql, [], |row| row.get(0)).unwrap())
    }
}

/// A mission outcome the account set aside, half of them snoozed.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
struct Aside {
    task_id: String,
    finished_at: i64,
    until: Option<i64>,
}

impl StorageEntity for Aside {
    const KIND: &'static str = "dismissals";
    const TABLE: &'static str = "account_dismissals";
    const SAVE: &'static str = "dismissal.saved";

    fn id(&self) -> String {
        self.task_id.clone()
    }

    fn setup(db: &Connection) {
        let db = Db(db);
        for n in 0..1000 {
            let dismissal = StoredDismissal {
                finished_at: 1_761_800_000_000,
                until: None,
            };
            if candidate(db.0) {
                db.set_dismissal(OTHER, &item(n), Some(dismissal)).unwrap();
            } else {
                db.set(
                    &format!("dismissed:{OTHER}:task:{}", item(n)),
                    &json!({ "finishedAt": dismissal.finished_at, "until": null }),
                    None,
                )
                .unwrap();
            }
        }
    }

    fn reference_schema(db: &Connection) {
        previous_schema(db);
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        let (finished_at, until) = db
            .0
            .prepare_cached(
                "SELECT finished_at,snoozed_until FROM account_dismissals WHERE account_id=? AND task_id=?",
            )
            .unwrap()
            .query_row(params![ACCOUNT, id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        Self {
            task_id: id.into(),
            finished_at,
            until,
        }
    }

    fn reference_read(db: &Db<'_>, id: &str) -> Self {
        let value = db
            .kv(&format!("dismissed:{ACCOUNT}:task:{id}"))
            .unwrap()
            .unwrap();
        Self {
            task_id: id.into(),
            finished_at: value["finishedAt"].as_i64().unwrap(),
            until: value["until"].as_i64(),
        }
    }

    fn list(db: &Db<'_>) -> usize {
        db.dismissals(ACCOUNT).unwrap().len()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        db.keys(&format!("dismissed:{ACCOUNT}:")).unwrap().len()
    }

    fn write(&self, db: &Db<'_>, _replacement: bool) {
        let dismissal = StoredDismissal {
            finished_at: self.finished_at,
            until: self.until,
        };
        db.set_dismissal(ACCOUNT, &self.task_id, Some(dismissal))
            .unwrap();
        db.audit(Self::SAVE, &json!({ "id": self.task_id }))
            .unwrap();
    }

    fn reference_write(&self, db: &Db<'_>, _replacement: bool) {
        db.set(
            &format!("dismissed:{ACCOUNT}:task:{}", self.task_id),
            &json!({ "finishedAt": self.finished_at, "until": self.until }),
            None,
        )
        .unwrap();
    }

    fn delete_candidate(db: &Db<'_>, id: &str) {
        db.set_dismissal(ACCOUNT, id, None).unwrap();
    }

    fn reference_delete(db: &Db<'_>, id: &str) {
        db.delete(&format!("dismissed:{ACCOUNT}:task:{id}"))
            .unwrap();
    }

    fn encoded_bytes(db: &Db<'_>) -> Option<i64> {
        let sql = if candidate(db.0) {
            "SELECT sum(payload) FROM dbstat WHERE name='account_dismissals'"
        } else {
            "SELECT sum(payload) FROM dbstat WHERE name IN ('kv','sqlite_autoindex_kv_1')"
        };
        Some(db.0.query_row(sql, [], |row| row.get(0)).unwrap())
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn read_mark_storage_layouts() {
    let marks: Vec<Mark> = (0..1000)
        .map(|n| Mark {
            key: format!("{}:{}", ["chat", "task"][n as usize % 2], item(10_000 + n)),
            finished_at: 1_761_800_000_000 + n as i64,
        })
        .collect();
    measure(&marks);
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn dismissal_storage_layouts() {
    let asides: Vec<Aside> = (0..1000)
        .map(|n| Aside {
            task_id: item(10_000 + n),
            finished_at: 1_761_800_000_000 + n as i64,
            until: (n % 2 == 0).then_some(1_761_886_400_000),
        })
        .collect();
    measure(&asides);
}
