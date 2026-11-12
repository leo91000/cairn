//! Hook rules, their immutable versions and the rights the current version uses.
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::{HookRuleDefinition, HookRuleSource};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS hook_rules(
 id TEXT PRIMARY KEY NOT NULL,
 creator_account_id TEXT NOT NULL CHECK(length(creator_account_id) BETWEEN 1 AND 100),
 creator_access_id TEXT CHECK(creator_access_id IS NULL OR length(creator_access_id) BETWEEN 1 AND 300),
 creator_owner INTEGER NOT NULL CHECK(creator_owner IN (0,1)),
 revision TEXT NOT NULL CHECK(length(revision) BETWEEN 1 AND 100),
 version INTEGER NOT NULL CHECK(version>=1),
 disabled_at INTEGER CHECK(disabled_at BETWEEN 0 AND 9007199254740991),
 emergency_paused_at INTEGER CHECK(emergency_paused_at BETWEEN 0 AND 9007199254740991),
 budget_exhausted_at INTEGER CHECK(budget_exhausted_at BETWEEN 0 AND 9007199254740991),
 budget_reason TEXT CHECK(budget_reason IS NULL OR length(budget_reason)<=500),
 membership_ended_at INTEGER CHECK(membership_ended_at BETWEEN 0 AND 9007199254740991),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS hook_rules_creator ON hook_rules(creator_account_id);
CREATE TABLE IF NOT EXISTS hook_rule_versions(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 version INTEGER NOT NULL CHECK(version>=1),
 definition TEXT NOT NULL CHECK(json_valid(definition) AND json_type(definition)='object' AND length(CAST(definition AS BLOB))<=524288),
 saved_by TEXT NOT NULL CHECK(length(saved_by) BETWEEN 1 AND 100),
 source TEXT NOT NULL CHECK(source IN ('app','publicMcp','installationAgent','duplicate','template')),
 conversation_id TEXT CHECK(conversation_id IS NULL OR length(conversation_id) BETWEEN 1 AND 100),
 native_grant TEXT NOT NULL CHECK(json_valid(native_grant) AND json_type(native_grant)='object'),
 saved_at INTEGER NOT NULL CHECK(saved_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(rule_id,version)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_uses(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('executionAgent','startedAgent','hook','tool','cairnHook')),
 agent_id TEXT NOT NULL,
 instance_id TEXT NOT NULL,
 name TEXT NOT NULL CHECK(length(name)<=200),
 PRIMARY KEY(rule_id,kind,agent_id,instance_id,name)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS hook_rule_uses_agent ON hook_rule_uses(agent_id,kind);
CREATE INDEX IF NOT EXISTS hook_rule_uses_instance ON hook_rule_uses(instance_id,kind);
";

/// Version 30 to 31. No rule exists before, so nothing is converted. Earlier upgrade
/// rehearsals may already hold the empty tables.
pub(crate) fn upgrade(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Recheck under the write lock: another startup may have completed the upgrade.
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if (31..=crate::store::DATABASE_VERSION).contains(&version) {
        return Ok(());
    }
    if version != 30 {
        return Err(Error::bad(
            "Unexpected installation database version during upgrade.",
        ));
    }
    tx.execute_batch(SCHEMA)?;
    tx.pragma_update(None, "user_version", 31)?;
    tx.commit()?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RuleRow {
    pub id: String,
    pub creator_account_id: String,
    /// The membership the rule belongs to; the owner's included when the Beacon knows it.
    pub creator_access_id: Option<String>,
    pub creator_owner: bool,
    pub revision: String,
    pub version: u32,
    pub disabled_at: Option<i64>,
    pub emergency_paused_at: Option<i64>,
    pub budget_exhausted_at: Option<i64>,
    pub budget_reason: Option<String>,
    pub membership_ended_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VersionRow {
    pub rule_id: String,
    pub version: u32,
    pub definition: HookRuleDefinition,
    pub saved_by: String,
    pub source: HookRuleSource,
    pub conversation_id: Option<String>,
    /// The native provenance of the saving request (ADR-0050), never widened later.
    pub native_grant: Value,
    pub saved_at: i64,
}

impl VersionRow {
    pub(crate) fn native_authority(&self) -> bool {
        self.native_grant["nativeProvenanceUnknown"] == false
            && self.native_grant["nativeAuthorityNodes"]
                .as_array()
                .is_some_and(|nodes| !nodes.is_empty())
    }
}

/// A right the current version uses, kept to list rules and protect grants.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Use {
    pub kind: &'static str,
    pub agent_id: String,
    pub instance_id: String,
    pub name: String,
}

const COLUMNS: &str = "id,creator_account_id,creator_access_id,creator_owner,revision,version,disabled_at,emergency_paused_at,budget_exhausted_at,budget_reason,membership_ended_at,created_at,updated_at";

fn rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuleRow> {
    Ok(RuleRow {
        id: row.get(0)?,
        creator_account_id: row.get(1)?,
        creator_access_id: row.get(2)?,
        creator_owner: row.get(3)?,
        revision: row.get(4)?,
        version: row.get(5)?,
        disabled_at: row.get(6)?,
        emergency_paused_at: row.get(7)?,
        budget_exhausted_at: row.get(8)?,
        budget_reason: row.get(9)?,
        membership_ended_at: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn source_key(source: HookRuleSource) -> &'static str {
    match source {
        HookRuleSource::App => "app",
        HookRuleSource::PublicMcp => "publicMcp",
        HookRuleSource::InstallationAgent => "installationAgent",
        HookRuleSource::Duplicate => "duplicate",
        HookRuleSource::Template => "template",
    }
}

fn version(row: &rusqlite::Row<'_>) -> rusqlite::Result<VersionRow> {
    let decode = |index: usize, error: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    let definition: String = row.get(2)?;
    let source: String = row.get(4)?;
    let native: String = row.get(6)?;
    Ok(VersionRow {
        rule_id: row.get(0)?,
        version: row.get(1)?,
        definition: serde_json::from_str(&definition).map_err(|e| decode(2, e))?,
        saved_by: row.get(3)?,
        source: serde_json::from_value(Value::String(source)).map_err(|e| decode(4, e))?,
        conversation_id: row.get(5)?,
        native_grant: serde_json::from_str(&native).map_err(|e| decode(6, e))?,
        saved_at: row.get(7)?,
    })
}

impl Db<'_> {
    pub(crate) fn hook_rule(&self, id: &str) -> Result<Option<RuleRow>> {
        Ok(self
            .0
            .prepare_cached(&format!("SELECT {COLUMNS} FROM hook_rules WHERE id=?"))?
            .query_row([id], rule)
            .optional()?)
    }

    pub(crate) fn hook_rules(&self) -> Result<Vec<RuleRow>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM hook_rules ORDER BY created_at,id"
            ))?
            .query_map([], rule)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn save_hook_rule(&self, row: &RuleRow) -> Result<()> {
        self.0
            .prepare_cached(&format!(
                "INSERT INTO hook_rules({COLUMNS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                 ON CONFLICT(id) DO UPDATE SET creator_account_id=excluded.creator_account_id,creator_access_id=excluded.creator_access_id,creator_owner=excluded.creator_owner,revision=excluded.revision,version=excluded.version,disabled_at=excluded.disabled_at,emergency_paused_at=excluded.emergency_paused_at,budget_exhausted_at=excluded.budget_exhausted_at,budget_reason=excluded.budget_reason,membership_ended_at=excluded.membership_ended_at,created_at=excluded.created_at,updated_at=excluded.updated_at"
            ))?
            .execute(params![
                row.id,
                row.creator_account_id,
                row.creator_access_id,
                row.creator_owner,
                row.revision,
                row.version,
                row.disabled_at,
                row.emergency_paused_at,
                row.budget_exhausted_at,
                row.budget_reason,
                row.membership_ended_at,
                row.created_at,
                row.updated_at,
            ])?;
        Ok(())
    }

    pub(crate) fn delete_hook_rule(&self, id: &str) -> Result<bool> {
        Ok(self
            .0
            .prepare_cached("DELETE FROM hook_rules WHERE id=?")?
            .execute([id])?
            > 0)
    }

    pub(crate) fn hook_rule_version(&self, id: &str, number: u32) -> Result<Option<VersionRow>> {
        Ok(self
            .0
            .prepare_cached("SELECT rule_id,version,definition,saved_by,source,conversation_id,native_grant,saved_at FROM hook_rule_versions WHERE rule_id=? AND version=?")?
            .query_row(params![id, number], version)
            .optional()?)
    }

    pub(crate) fn hook_rule_versions(&self, id: &str) -> Result<Vec<VersionRow>> {
        Ok(self
            .0
            .prepare_cached("SELECT rule_id,version,definition,saved_by,source,conversation_id,native_grant,saved_at FROM hook_rule_versions WHERE rule_id=? ORDER BY version DESC")?
            .query_map([id], version)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Versions are immutable: a new one never replaces an earlier number.
    pub(crate) fn insert_hook_rule_version(&self, row: &VersionRow, uses: &[Use]) -> Result<()> {
        self.0
            .prepare_cached("INSERT INTO hook_rule_versions VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?
            .execute(params![
                row.rule_id,
                row.version,
                serde_json::to_string(&row.definition)?,
                row.saved_by,
                source_key(row.source),
                row.conversation_id,
                serde_json::to_string(&row.native_grant)?,
                row.saved_at,
            ])?;
        self.0
            .prepare_cached("DELETE FROM hook_rule_uses WHERE rule_id=?")?
            .execute([&row.rule_id])?;
        for usage in uses {
            self.0
                .prepare_cached("INSERT OR IGNORE INTO hook_rule_uses VALUES(?1,?2,?3,?4,?5)")?
                .execute(params![
                    row.rule_id,
                    usage.kind,
                    usage.agent_id,
                    usage.instance_id,
                    usage.name,
                ])?;
        }
        Ok(())
    }

    /// Rules whose current version uses this agent or plugin instance.
    pub(crate) fn hook_rules_using(
        &self,
        agent: Option<&str>,
        instance: Option<&str>,
    ) -> Result<Vec<String>> {
        Ok(self
            .0
            .prepare_cached("SELECT id FROM hook_rules WHERE (?1 IS NULL OR id IN (SELECT rule_id FROM hook_rule_uses WHERE agent_id=?1 AND kind IN ('executionAgent','startedAgent'))) AND (?2 IS NULL OR id IN (SELECT rule_id FROM hook_rule_uses WHERE instance_id=?2 AND kind IN ('hook','tool'))) ORDER BY id")?
            .query_map(params![agent, instance], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Hooks and tools used on this execution agent by rules of current memberships.
    pub(crate) fn hook_rule_grant_uses(&self, agent: &str) -> Result<Vec<(String, Use)>> {
        Ok(self
            .0
            .prepare_cached("SELECT u.rule_id,u.kind,u.agent_id,u.instance_id,u.name,r.creator_account_id FROM hook_rule_uses u JOIN hook_rules r ON r.id=u.rule_id WHERE u.agent_id=? AND u.kind IN ('hook','tool') AND r.membership_ended_at IS NULL ORDER BY u.instance_id,u.kind,u.name")?
            .query_map([agent], |row| {
                let kind: String = row.get(1)?;
                Ok((
                    row.get::<_, String>(5)?,
                    Use {
                        kind: if kind == "hook" { "hook" } else { "tool" },
                        agent_id: row.get(2)?,
                        instance_id: row.get(3)?,
                        name: row.get(4)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(connection: &Connection) -> Vec<String> {
        connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'hook_rule%' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[tokio::test]
    async fn version_thirty_gains_empty_rule_tables_once() {
        let root = tempfile::tempdir().unwrap();
        drop(crate::store::Store::open(root.path()).unwrap());
        let database = root.path().join("manager.db");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch("DROP TABLE hook_rule_uses; DROP TABLE hook_rule_versions; DROP TABLE hook_rules; PRAGMA user_version=30;")
            .unwrap();
        assert!(tables(&connection).is_empty());
        drop(connection);

        drop(crate::store::Store::open(root.path()).unwrap());
        let mut connection = Connection::open(&database).unwrap();
        assert_eq!(
            tables(&connection),
            ["hook_rule_uses", "hook_rule_versions", "hook_rules"]
        );
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 31);
        upgrade(&mut connection).unwrap();
        connection.pragma_update(None, "user_version", 29).unwrap();
        assert!(upgrade(&mut connection).is_err());
    }
}
