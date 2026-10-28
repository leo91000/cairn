//! A15 uses the A1/A2 clocks, WAL settings, audit and alternating repetitions.
use super::*;
use cairn_installation::{
    accounts::{Account, AccountState, Exhaustion},
    error::Result,
    provider::Provider,
};

impl StorageEntity for Account {
    const KIND: &'static str = "agentAccounts";
    const TABLE: &'static str = "coding_agent_accounts";
    const SAVE: &'static str = "account.saved";

    fn id(&self) -> String {
        self.id.clone()
    }

    fn read(db: &Db<'_>, id: &str) -> Self {
        db.coding_account(id).unwrap().unwrap()
    }

    fn list(db: &Db<'_>) -> usize {
        db.coding_accounts().unwrap().len()
    }

    fn reference_list(db: &Db<'_>) -> usize {
        db.list(Self::KIND)
            .unwrap()
            .into_iter()
            .map(Account::from_value)
            .collect::<Result<Vec<_>>>()
            .unwrap()
            .len()
    }

    fn write(&self, db: &Db<'_>, _: bool) {
        db.save_coding_account(self).unwrap();
        db.audit(Self::SAVE, &json!({ "id": self.id })).unwrap();
    }

    fn encoded_bytes(db: &Db<'_>) -> Option<i64> {
        Some(db.0.query_row("SELECT coalesce(sum(payload),0) FROM dbstat WHERE name IN ('records','records_kind','records_node_backups_run','records_chat_slug','sqlite_autoindex_records_1','coding_agent_accounts','coding_agent_accounts_identity','coding_agent_accounts_updated','sqlite_autoindex_coding_agent_accounts_1','account_usage','account_usage_windows','account_exhaustion','account_leases','account_leases_account','sqlite_autoindex_account_leases_1')",[],|row|row.get(0)).unwrap())
    }

    fn reference_write(&self, db: &Db<'_>, _: bool) {
        // Frozen records mapping at 663259d9, before A15. Only field spelling /
        // absent optional fields differ; both readers return identical Account values.
        let mut value = self.to_value();
        let object = value.as_object_mut().unwrap();
        object.remove("revision");
        for key in ["priority", "modelProvider", "modelProviderName"] {
            if object[key].is_null() {
                object.remove(key);
            }
        }
        for usage in ["usage", "exhausted"] {
            let value = if usage == "usage" {
                value.get_mut(usage)
            } else {
                value
                    .get_mut(usage)
                    .and_then(|exhausted| exhausted.get_mut("usage"))
            };
            if let Some(object) = value.and_then(Value::as_object_mut)
                && object.get("useBalance") == Some(&Value::Bool(false))
            {
                object.remove("useBalance");
            }
        }
        db.0.prepare_cached("INSERT INTO records VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at").unwrap().execute(params![self.id,Self::KIND,value.to_string(),now()]).unwrap();
    }

    fn reference_schema(db: &Connection) {
        db.execute_batch("DROP TABLE account_leases;DROP TABLE account_exhaustion;DROP TABLE account_usage_windows;DROP TABLE account_usage;DROP TABLE coding_agent_accounts;DROP TABLE records;
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';").unwrap();
    }

    fn admission_state(db: &Db<'_>, _: &str, reference: bool) -> Option<String> {
        // Mapping and ranking portion of selection, with no CLI/network effects.
        // The pool's behavioral suites cover Driver support, speed and slot fencing.
        let mut accounts: Vec<Account> = if reference {
            db.list(Self::KIND)
                .unwrap()
                .into_iter()
                .map(|v| Account::from_value(v).unwrap())
                .collect()
        } else {
            db.coding_accounts().unwrap()
        };
        accounts.retain(|a| {
            a.is_active() && a.exhausted.is_none() && !a.usage().blocked("gpt-6.1-sol")
        });
        accounts.sort_by(|a, b| {
            b.usage()
                .remaining("gpt-6.1-sol")
                .unwrap_or(-1.)
                .total_cmp(&a.usage().remaining("gpt-6.1-sol").unwrap_or(-1.))
                .then(a.last_used_at.cmp(&b.last_used_at))
                .then(a.id.cmp(&b.id))
        });
        Some(accounts.first().map(|a| a.id.clone()).unwrap_or_default())
    }
}

#[tokio::test]
#[ignore = "local measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn account_storage_layouts() {
    for count in [3, 15, 30] {
        let accounts: Vec<_> = (0..count)
            .map(|n| {
                let mut account = Account::new(
                    Provider::ALL[n % 3],
                    &format!("Account {n}"),
                    AccountState::Ready,
                );
                account.id = uuid::Uuid::from_u128((4_u128 << 76) | (2_u128 << 62) | (n as u128))
                    .to_string();
                account.revision = String::new();
                account.created_at = Some(1700000000000 + n as i64);
                account.checked_at = Some(1700000001000);
                account.last_used_at = Some(1700000000500 + n as i64);
                account.email = Some(format!("account{n}@example.test"));
                account.identity = Some(format!("synthetic-identity-{n}"));
                account.plan = Some("synthetic plan".into());
                if n % 3 == 2 {
                    account.model_provider = Some("opencode-go".into());
                    account.model_provider_name = Some("OpenCode Go".into());
                    account.priority = Some(n as u64);
                }
                account.enabled = n % 7 != 0;
                let windows = [
                    ("five", "5-hour window", 300, 7),
                    ("week", "Weekly", 10080, 11),
                    ("model", "Model weekly", 10080, 13),
                ]
                .map(|(id, label, duration, factor)| {
                    json!({
                        "id": id,
                        "label": label,
                        "usedPercent": (n * factor % 100) as f64,
                        "resetsAt": 1900000000,
                        "durationMins": duration,
                        "models": if id == "model" { vec!["gpt-6.1-sol"] } else { vec![] },
                        "reached": false,
                    })
                });
                account.usage = Some(
                    serde_json::from_value(json!({
                        "allowed": true,
                        "checkedAt": 1700000001000_i64,
                        "windows": windows,
                        "resets": {
                            "available": 1,
                            "credits": [{ "id": "synthetic", "expiresAt": 1900000000 }],
                        },
                    }))
                    .unwrap(),
                );
                if n % 4 == 0 {
                    account.exhausted = Some(Exhaustion {
                        at: 1700000000900,
                        model: "gpt-6.1-sol".into(),
                        usage: account.usage.clone(),
                    });
                }
                account
            })
            .collect();
        measure(&accounts);
    }
}
