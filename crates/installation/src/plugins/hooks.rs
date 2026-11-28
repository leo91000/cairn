//! Durable hook intake. Deliberately separate from the script event pump: the rule
//! engine consumes these events, never plugin handlers or inbound messages.
use super::{capabilities::Context, connections, registry};
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) const SCHEMA: &str = "
CREATE TABLE plugin_hook_events(
 id TEXT PRIMARY KEY NOT NULL,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 revision TEXT NOT NULL,
 package_digest TEXT NOT NULL CHECK(length(package_digest)=64),
 external_id TEXT NOT NULL CHECK(length(external_id) BETWEEN 1 AND 200),
 name TEXT NOT NULL,
 payload TEXT NOT NULL CHECK(json_valid(payload) AND json_type(payload)='object' AND length(CAST(payload AS BLOB))<=262144),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE INDEX plugin_hook_events_instance ON plugin_hook_events(instance_id,created_at,id);
CREATE TABLE plugin_hook_receipts(
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 external_id TEXT NOT NULL CHECK(length(external_id) BETWEEN 1 AND 200),
 expires_at INTEGER NOT NULL,
 PRIMARY KEY(instance_id,external_id)
) WITHOUT ROWID;
CREATE INDEX plugin_hook_receipts_expiry ON plugin_hook_receipts(expires_at);
";

pub const MAX_PENDING: usize = 50;
pub const MAX_RECEIPTS: usize = 10_000;
pub const RETENTION_MS: i64 = 30 * 86_400_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookEvent {
    pub id: String,
    pub instance_id: String,
    pub revision: String,
    pub package_digest: String,
    pub external_id: String,
    pub name: String,
    pub payload: Value,
    pub created_at: i64,
}

impl Db<'_> {
    /// Only current, unexpired events can be consumed. The queue is at most 50.
    pub fn plugin_hooks(
        &self,
        instance: &str,
        revision: &str,
        digest: &str,
        at: i64,
    ) -> Result<Vec<HookEvent>> {
        let mut stmt = self.0.prepare_cached("SELECT id,instance_id,revision,package_digest,external_id,name,payload,created_at FROM plugin_hook_events WHERE instance_id=?1 AND revision=?2 AND package_digest=?3 AND created_at>?4 ORDER BY created_at,id")?;
        Ok(stmt
            .query_map(
                params![instance, revision, digest, at - RETENTION_MS],
                |row| {
                    let payload: String = row.get(6)?;
                    Ok(HookEvent {
                        id: row.get(0)?,
                        instance_id: row.get(1)?,
                        revision: row.get(2)?,
                        package_digest: row.get(3)?,
                        external_id: row.get(4)?,
                        name: row.get(5)?,
                        payload: serde_json::from_str(&payload).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                        created_at: row.get(7)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Acknowledgement never removes the deduplication receipt.
    pub fn remove_plugin_hook(&self, instance: &str, id: &str) -> Result<()> {
        self.0.execute(
            "DELETE FROM plugin_hook_events WHERE instance_id=?1 AND id=?2",
            params![instance, id],
        )?;
        Ok(())
    }

    pub(crate) fn prune_plugin_hooks(&self, at: i64) -> Result<()> {
        self.0.execute(
            "DELETE FROM plugin_hook_receipts WHERE expires_at<=?1",
            [at],
        )?;
        self.0.execute("DELETE FROM plugin_hook_events WHERE created_at<=?1 OR NOT EXISTS(SELECT 1 FROM plugin_instances i WHERE i.id=plugin_hook_events.instance_id AND i.revision=plugin_hook_events.revision)", [at - RETENTION_MS])?;
        Ok(())
    }

    pub(crate) fn invalidate_plugin_hooks(&self, plugin: &str, digest: &str) -> Result<()> {
        self.0.execute(
            "DELETE FROM plugin_hook_events WHERE instance_id IN (SELECT id FROM plugin_instances WHERE plugin_id=?1) AND package_digest<>?2",
            params![plugin, digest],
        )?;
        Ok(())
    }

    fn enqueue_hook(
        &self,
        context: &Context,
        digest: &str,
        external: &str,
        name: &str,
        payload: &Value,
        at: i64,
    ) -> Result<bool> {
        let current = connections::authorize(self, &context.connection)?;
        self.prune_plugin_hooks(at)?;
        // Withdraw queued events from an older approved package, even when an
        // instance's settings revision stayed unchanged across the update.
        self.0.execute(
            "DELETE FROM plugin_hook_events WHERE instance_id=?1 AND package_digest<>?2",
            params![current.id, digest],
        )?;
        // A busy service emits far more than rules use. Nothing is recorded for a hook
        // no enabled rule listens to, so it never fills the queue or the receipt ledger.
        let listened: bool = self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM hook_rule_uses u JOIN hook_rules r ON r.id=u.rule_id \
             WHERE u.kind='hook' AND u.instance_id=?1 AND u.name=?2 AND r.disabled_at IS NULL)",
            params![current.id, name],
            |row| row.get(0),
        )?;
        if !listened {
            return Ok(false);
        }
        let seen: bool = self.0.query_row("SELECT EXISTS(SELECT 1 FROM plugin_hook_receipts WHERE instance_id=?1 AND external_id=?2)", params![current.id, external], |row| row.get(0))?;
        if seen {
            return Ok(false);
        }
        let pending: i64 = self.0.query_row(
            "SELECT count(*) FROM plugin_hook_events WHERE instance_id=?1",
            [&current.id],
            |row| row.get(0),
        )?;
        let receipts: i64 = self.0.query_row(
            "SELECT count(*) FROM plugin_hook_receipts WHERE instance_id=?1",
            [&current.id],
            |row| row.get(0),
        )?;
        if pending >= MAX_PENDING as i64 || receipts >= MAX_RECEIPTS as i64 {
            return Ok(false);
        }
        self.0.execute(
            "INSERT INTO plugin_hook_events VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                id(),
                current.id,
                current.revision,
                digest,
                external,
                name,
                serde_json::to_string(payload)?,
                at
            ],
        )?;
        self.0.execute(
            "INSERT INTO plugin_hook_receipts VALUES(?1,?2,?3)",
            params![current.id, external, at + RETENTION_MS],
        )?;
        Ok(true)
    }
}

pub(crate) async fn emit(s: &Service, context: &Context, args: &Value) -> Result<Value> {
    // Only the transport and timer handlers produce hooks; a tool or resource
    // lookup cannot indirectly manufacture a rule trigger.
    if context.link.is_none() && context.event["name"] != "timer" {
        return Err(Error::forbidden(
            "Emit hooks from the transport worker or a timer handler.",
        ));
    }
    let name = args["name"].as_str().unwrap_or_default();
    let external = args["id"].as_str().unwrap_or_default();
    let payload = &args["payload"];
    let hook = context
        .manifest
        .hook(name)
        .ok_or_else(|| Error::bad("This hook is not declared."))?;
    if external.is_empty()
        || external.len() > 200
        || external.chars().any(char::is_control)
        || !payload.is_object()
        || serde_json::to_vec(payload)?.len() > 262_144
        || !jsonschema::is_valid(&hook.payload_schema, payload)
    {
        return Err(Error::bad("Invalid hook ID or payload."));
    }
    let _publication = s.plugins.package_publication.lock().await;
    let package = registry::active(s, &context.connection.plugin_id).await?;
    if context.package_digest.as_deref() != Some(package.digest.as_str()) {
        return Err(Error::forbidden(
            "The plugin package changed. Restart its worker.",
        ));
    }
    let digest = package.digest;
    let context = Context {
        management: None,
        connection: context.connection.clone(),
        manifest: context.manifest.clone(),
        package_digest: None,
        event: Value::Null,
        link: None,
        draft: None,
    };
    let external = external.to_owned();
    let name = name.to_owned();
    let payload = payload.clone();
    let queued = s
        .store
        .transaction(move |db| {
            db.enqueue_hook(&context, &digest, &external, &name, &payload, now())
        })
        .await?;
    if queued {
        s.rules.wake.notify_one();
    }
    Ok(json!(queued))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn hook_context() -> Context {
        let mut connection = connections::Connection::draft(&id(), "github", json!({}), vec![]);
        connection.name = "Hooks".into();
        connection.revision = id();
        connection.enabled = true;
        Context {
            connection,
            manifest: serde_json::from_value(json!({
                "id": "github", "name": "Hooks", "version": "1", "apiVersion": 2,
                "capabilities": [],
            }))
            .unwrap(),
            package_digest: None,
            event: Value::Null,
            management: None,
            link: None,
            draft: None,
        }
    }

    /// A rule listening to `name` on the instance; the intake records nothing otherwise.
    fn listen(db: &Db<'_>, instance: &str, name: &str) -> Result<String> {
        let rule = id();
        db.0.execute(
            "INSERT INTO hook_rules(id,creator_account_id,creator_owner,revision,version,created_at,updated_at) VALUES(?1,'owner',1,'r',1,0,0)",
            [&rule],
        )?;
        db.0.execute(
            "INSERT INTO hook_rule_uses VALUES(?1,'hook','agent',?2,?3)",
            params![rule, instance, name],
        )?;
        Ok(rule)
    }

    #[tokio::test]
    async fn hooks_no_enabled_rule_listens_to_are_not_recorded() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let context = hook_context();
                let instance = context.connection.id.clone();
                let digest = "a".repeat(64);
                db.save_plugin_instance(&context.connection)?;
                assert!(!db.enqueue_hook(
                    &context,
                    &digest,
                    "quiet",
                    "message",
                    &json!({}),
                    now()
                )?);
                let rule = listen(db, &instance, "reaction")?;
                assert!(!db.enqueue_hook(
                    &context,
                    &digest,
                    "other",
                    "message",
                    &json!({}),
                    now()
                )?);
                db.0.execute(
                    "UPDATE hook_rule_uses SET name='message' WHERE rule_id=?1",
                    [&rule],
                )?;
                db.0.execute("UPDATE hook_rules SET disabled_at=1 WHERE id=?1", [&rule])?;
                assert!(!db.enqueue_hook(
                    &context,
                    &digest,
                    "disabled",
                    "message",
                    &json!({}),
                    now()
                )?);
                let receipts: i64 =
                    db.0.query_row("SELECT count(*) FROM plugin_hook_receipts", [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(receipts, 0);
                db.0.execute(
                    "UPDATE hook_rules SET disabled_at=NULL WHERE id=?1",
                    [&rule],
                )?;
                // Unrecorded IDs stay available once a rule listens.
                assert!(db.enqueue_hook(
                    &context,
                    &digest,
                    "quiet",
                    "message",
                    &json!({}),
                    now()
                )?);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn queue_survives_reopen_and_deduplicates_after_acknowledgement() {
        let root = tempfile::tempdir().unwrap();
        let context = hook_context();
        let instance = context.connection.id.clone();
        let revision = context.connection.revision.clone();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(move |db| {
                db.save_plugin_instance(&context.connection)?;
                listen(db, &context.connection.id, "message")?;
                assert!(db.enqueue_hook(
                    &context,
                    &"a".repeat(64),
                    "external",
                    "message",
                    &json!({"content":"kept"}),
                    now()
                )?);
                assert!(!db.enqueue_hook(
                    &context,
                    &"a".repeat(64),
                    "external",
                    "message",
                    &json!({"content":"changed"}),
                    now()
                )?);
                Ok(())
            })
            .await
            .unwrap();
        drop(store);
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(move |db| {
                let events = db.plugin_hooks(&instance, &revision, &"a".repeat(64), now())?;
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].payload["content"], "kept");
                db.remove_plugin_hook(&instance, &events[0].id)?;
                let mut context = hook_context();
                context.connection = db.plugin_instance(&instance)?.unwrap();
                assert!(!db.enqueue_hook(
                    &context,
                    &"a".repeat(64),
                    "external",
                    "message",
                    &json!({}),
                    now()
                )?);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn backlog_ledger_retention_and_instance_boundaries_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.transaction(|db| {
            let first = hook_context();
            let second = hook_context();
            let digest = "a".repeat(64);
            let at = now();
            db.save_plugin_instance(&first.connection)?;
            db.save_plugin_instance(&second.connection)?;
            listen(db, &first.connection.id, "message")?;
            listen(db, &second.connection.id, "message")?;
            for index in 0..MAX_PENDING {
                assert!(db.enqueue_hook(&first, &digest, &index.to_string(), "message", &json!({}), at)?);
            }
            assert!(!db.enqueue_hook(&first, &digest, "retry", "message", &json!({}), at)?);
            assert!(db.enqueue_hook(&second, &digest, "0", "message", &json!({}), at)?);
            let event = &db.plugin_hooks(&first.connection.id, &first.connection.revision, &digest, at)?[0];
            db.remove_plugin_hook(&second.connection.id, &event.id)?;
            assert_eq!(db.plugin_hooks(&first.connection.id, &first.connection.revision, &digest, at)?.len(), MAX_PENDING);
            db.remove_plugin_hook(&first.connection.id, &event.id)?;
            assert!(db.enqueue_hook(&first, &digest, "retry", "message", &json!({}), at)?);
            db.prune_plugin_hooks(at + RETENTION_MS)?;
            assert!(db.plugin_hooks(&first.connection.id, &first.connection.revision, &digest, at + RETENTION_MS)?.is_empty());
            assert!(db.enqueue_hook(&first, &digest, "0", "message", &json!({}), at + RETENTION_MS)?);
            db.0.execute("WITH RECURSIVE n(x) AS(SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<?2) INSERT INTO plugin_hook_receipts SELECT ?1,'ledger-'||x,?3 FROM n", params![first.connection.id, MAX_RECEIPTS as i64 - 1, at + 2 * RETENTION_MS])?;
            assert!(!db.enqueue_hook(&first, &digest, "overflow", "message", &json!({}), at + RETENTION_MS)?);
            db.remove_plugin_instance(&first.connection.id)?;
            let left: i64 = db.0.query_row("SELECT count(*) FROM plugin_hook_receipts WHERE instance_id=?1", [&first.connection.id], |row| row.get(0))?;
            assert_eq!(left, 0);
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn revisions_and_package_versions_fence_old_events_and_emitters() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                let mut context = hook_context();
                let at = now();
                let a = "a".repeat(64);
                let b = "b".repeat(64);
                db.save_plugin_instance(&context.connection)?;
                listen(db, &context.connection.id, "message")?;
                assert!(db.enqueue_hook(&context, &a, "old", "message", &json!({}), at)?);
                assert!(
                    db.plugin_hooks(&context.connection.id, &context.connection.revision, &b, at)?
                        .is_empty()
                );
                assert!(db.enqueue_hook(&context, &b, "new", "message", &json!({}), at)?);
                assert!(
                    db.plugin_hooks(&context.connection.id, &context.connection.revision, &a, at)?
                        .is_empty()
                );
                let old = context.connection.clone();
                context.connection.revision = id();
                db.save_plugin_instance(&context.connection)?;
                db.prune_plugin_hooks(at)?;
                let count: i64 =
                    db.0.query_row("SELECT count(*) FROM plugin_hook_events", [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(count, 0);
                context.connection = old;
                assert!(
                    db.enqueue_hook(&context, &b, "stale", "message", &json!({}), at)
                        .is_err()
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}
