//! Retire environments that could still mount an S3 base before restarting work.
//! The marker and every detachment commit together; no remote I/O is involved.
use crate::{
    config::now,
    environment_expiration::{Expiration, request_in},
    error::Result,
    service::Service,
    store::Db,
    validation::text,
};
use cairn_contracts::EnvironmentExpirationCause;
use serde_json::{Value, json};
use std::collections::HashSet;

const MARKER: &str = "migration:local-conversation-disks:v1";

pub(crate) async fn apply(s: &Service) -> Result<()> {
    s.store.transaction(migrate).await
}

fn migrate(db: &mut Db<'_>) -> Result<()> {
    if db.kv(MARKER)?.is_some() {
        return Ok(());
    }
    // Capture obsolete destinations before the existing expiration migration
    // abandons their reservations. They must not become fresh placement hints.
    let moving = db.json_rows(
        "SELECT json_quote(id) FROM runs WHERE placement_move_request IS NOT NULL
         OR placement_move_reservation IS NOT NULL
         OR json_type(boundary,'$.backendMigration')='object'",
        [],
    )?;
    crate::nodes::moves::abandon_legacy_moves(db)?;
    crate::backend_migration::abandon_legacy_conversions(db)?;
    for id in moving.iter().filter_map(Value::as_str) {
        db.update_run_placement(
            id,
            crate::nodes::run_placement::PlacementChanges {
                target_node_id: Some(None),
                ..Default::default()
            },
        )?;
    }
    // A completed move can leave its paid recovery marker after placement state
    // has cleared. Remove only that authorization, keeping ordinary recovery
    // evidence and current placement hints even for checkpoint-only leftovers.
    db.0.execute(
        "UPDATE run_checkpoints SET state=json_remove(state,'$.moveRecoveryReservation')
         WHERE json_type(state,'$.moveRecoveryReservation') IS NOT NULL",
        [],
    )?;

    // A completed point can already be installed even when its acknowledgement
    // was lost. Shared references and grants also cover interrupted publications
    // or missing point records. Pending demand alone does not imply a remote base.
    let mut remote = db
        .list("node-backups")?
        .into_iter()
        .filter(|point| point["destination"] == "s3")
        .map(|point| text(&point, "runId").to_owned())
        .collect::<HashSet<_>>();
    remote.extend(
        db.json_rows(
            "SELECT DISTINCT json_quote(p.run_id) FROM shared_publications p
         JOIN shared_references r ON r.publication=p.id
         WHERE NOT EXISTS(SELECT 1 FROM direct_journal_acknowledgements a
                          WHERE a.publication=p.id AND a.detached=1)",
            [],
        )?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned),
    );
    for grant in db.list("node-disk-grants")? {
        if grant["published"].is_string()
            || grant["backups"]
                .as_array()
                .is_some_and(|points| !points.is_empty())
        {
            remote.insert(text(&grant, "runId").to_owned());
        }
    }
    let mut revoked_environments = HashSet::new();
    for node in db
        .nodes_boundary()?
        .into_iter()
        .filter(|node| node["revoked"] == true)
    {
        revoked_environments.extend(crate::environment_expiration::runs_on_node(
            db,
            text(&node, "id"),
        )?);
    }
    let runs = db.json_rows(
        "SELECT json_set(boundary,'$.id',id,'$.nodeId',placement_node_id) FROM runs",
        [],
    )?;
    for run in runs {
        let id = text(&run, "id");
        let purging = db
            .conversation_for_run(id)?
            .is_some_and(|chat| chat.lifecycle == cairn_contracts::ConversationLifecycle::Purging);
        if run["environmentState"] == "expired"
            || crate::environment_expiration::pending(db, id)?
            || purging
        {
            continue;
        }
        let on_revoked = revoked_environments.contains(id);
        let has_remote_base = remote.contains(id)
            || run["backup"]["id"].is_string()
            || run["storage"]["published"]["backupId"].is_string();
        if on_revoked || has_remote_base {
            // No worker has resumed yet. The existing retirement queue keeps
            // its old checkpoint/attempt and fences it before fresh admission.
            request_in(
                db,
                id,
                &Expiration::new(EnvironmentExpirationCause::StorageMigration),
                false,
            )?;
        } else if run["backup"].is_object() || run["storage"].is_object() {
            // Never-published local disks survive without stale sync errors.
            let mut storage = run["storage"].clone();
            if let Some(storage) = storage.as_object_mut() {
                for field in [
                    "backupDegraded",
                    "backupUrgent",
                    "backupSeconds",
                    "maxDirtySeconds",
                ] {
                    storage.remove(field);
                }
                if storage
                    .get("waitingFor")
                    .is_some_and(|reason| reason == "backup-lag")
                {
                    storage.remove("waitingFor");
                }
            }
            db.update_run_boundary(id, &json!({ "backup": null, "storage": storage }))?;
        }
    }
    db.set(MARKER, &json!({ "completedAt": now() }), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        attempt_fixture::DbAttemptFixture, node_fixture::DbNodeFixture, run_fixture::DbRunFixture,
        store::Store, worker::checkpoint::RunCheckpoint,
    };

    const ACTIVE: &str = "00000000-0000-4000-8000-000000000001";
    const REVOKED: &str = "00000000-0000-4000-8000-000000000002";

    fn run(db: &Db<'_>, id: &str, node: Option<&str>) -> Result<()> {
        db.add_run_fixture(
            &json!({
                "id": id, "taskId": id, "status": "succeeded", "createdAt": 0,
                "trigger": "chat", "nodeId": node, "sessionId": "retained-session",
                "storage": { "mode": "on-demand", "dirtyBytes": 4096 },
            }),
            None,
        )?;
        Ok(())
    }

    #[tokio::test]
    async fn published_and_revoked_environments_expire_once_without_remote_deletion() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.transaction(|db| {
            db.put_node_fixture(&json!({ "id": ACTIVE, "revoked": false }))?;
            db.put_node_fixture(&json!({ "id": REVOKED, "revoked": true }))?;
            for id in ["point", "pointer", "receipt", "shared", "grant", "revoked", "checkpoint", "local"] {
                run(db, id, Some(if id == "revoked" { REVOKED } else { ACTIVE }))?;
            }
            db.put("node-backups", &json!({ "id": "old-point", "runId": "point", "destination": "s3" }))?;
            db.update_run_boundary("pointer", &json!({ "backup": { "id": "missing-point" } }))?;
            db.update_run_boundary("receipt", &json!({ "storage": { "mode": "on-demand", "published": { "backupId": "old" } } }))?;
            db.0.execute("INSERT INTO shared_publications VALUES('shared-publication','shared')", [])?;
            db.0.execute("INSERT INTO shared_objects VALUES('remote-object','s3','hash',4096,'{}','pending',NULL,0)", [])?;
            db.0.execute("INSERT INTO shared_references VALUES('shared-publication','remote-object')", [])?;
            db.put("node-disk-grants", &json!({ "id": "old-grant", "runId": "grant", "backups": ["unknown-point"] }))?;
            RunCheckpoint::from_value(json!({ "nodeId": REVOKED }))?.store_in(db, "checkpoint")?;
            db.update_run_boundary("local", &json!({ "backup": { "status": "error", "requestedRevision": 2 } }))?;
            db.0.execute("INSERT INTO remote_deletions(id,location,key,prefix) VALUES('old-deletion','{}','old-prefix/',1)", [])?;
            Ok(())
        }).await.unwrap();
        store.transaction(migrate).await.unwrap();
        let before = store.list("environment-retirements").await.unwrap();
        assert_eq!(before.len(), 7);
        store.transaction(migrate).await.unwrap();
        assert_eq!(store.list("environment-retirements").await.unwrap(), before);
        for id in [
            "point",
            "pointer",
            "receipt",
            "shared",
            "grant",
            "revoked",
            "checkpoint",
        ] {
            let run = store.run(id).await.unwrap();
            assert_eq!(run["environmentState"], "expired", "{id}");
            assert_eq!(
                run["environmentExpirationCause"], "storage-migration",
                "{id}"
            );
            assert!(run["nodeId"].is_null(), "{id}");
            assert!(run["sessionId"].is_null(), "{id}");
            assert!(run["backup"].is_null(), "{id}");
        }
        let local = store.run("local").await.unwrap();
        assert!(local["environmentState"].is_null());
        assert_eq!(local["nodeId"], ACTIVE);
        assert_eq!(local["sessionId"], "retained-session");
        assert_eq!(local["storage"]["dirtyBytes"], 4096);
        assert!(local["backup"].is_null());
        store
            .read(|db| {
                assert_eq!(
                    db.0.query_row("SELECT count(*) FROM remote_deletions", [], |row| row
                        .get::<_, i64>(0))?,
                    1
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn revoked_materialized_admission_expires_without_a_published_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.put_node_fixture(&json!({ "id": ACTIVE, "revoked": false }))?;
                db.put_node_fixture(&json!({ "id": REVOKED, "revoked": true }))?;
                for (id, node) in [("orphaned", REVOKED), ("local", ACTIVE)] {
                    run(db, id, None)?;
                    db.put_attempt_fixture(&json!({
                        "id": format!("admission-{id}"), "runId": id, "nodeId": node,
                        "role": "execution", "diskMaterialized": true, "released": false,
                    }))?;
                }
                Ok(())
            })
            .await
            .unwrap();
        store.transaction(migrate).await.unwrap();
        let orphaned = store.run("orphaned").await.unwrap();
        assert_eq!(orphaned["environmentState"], "expired");
        assert_eq!(orphaned["environmentExpirationCause"], "storage-migration");
        let retired = store.list("environment-retirements").await.unwrap();
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0]["attempts"][0]["id"], "admission-orphaned");
        assert_eq!(retired[0]["attempts"][0]["nodeId"], REVOKED);
        let local = store.run("local").await.unwrap();
        assert!(local["environmentState"].is_null());
        assert_eq!(local["sessionId"], "retained-session");
        store.transaction(migrate).await.unwrap();
        assert_eq!(
            store.list("environment-retirements").await.unwrap(),
            retired
        );
    }

    #[tokio::test]
    async fn detached_publications_do_not_expire_a_local_replacement() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.put_node_fixture(&json!({ "id": ACTIVE, "revoked": false }))?;
                for (id, detached) in [("local", true), ("remote", false)] {
                    run(db, id, Some(ACTIVE))?;
                    db.0.execute(
                        "INSERT INTO shared_publications VALUES(?1,?1)",
                        [id],
                    )?;
                    db.0.execute(
                        "INSERT INTO shared_objects(id,destination,hash,size,location,state) VALUES(?1,'s3',?1,4096,'{}','ready')",
                        [id],
                    )?;
                    db.0.execute("INSERT INTO shared_references VALUES(?1,?1)", [id])?;
                    db.0.execute(
                        "INSERT INTO direct_journal_acknowledgements(publication,run_id,node_id,detached) VALUES(?1,?1,'old-node',?2)",
                        rusqlite::params![id, detached],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        store.transaction(migrate).await.unwrap();
        let local = store.run("local").await.unwrap();
        assert!(local["environmentState"].is_null());
        assert_eq!(local["sessionId"], "retained-session");
        let remote = store.run("remote").await.unwrap();
        assert_eq!(remote["environmentState"], "expired");
        store
            .read(|db| {
                let pins: i64 = db.0.query_row(
                    "SELECT count(*) FROM shared_references WHERE publication='local'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(pins, 1);
                Ok(())
            })
            .await
            .unwrap();
        let retired = store.list("environment-retirements").await.unwrap();
        assert_eq!(retired.len(), 1);
        store.transaction(migrate).await.unwrap();
        assert_eq!(
            store.list("environment-retirements").await.unwrap(),
            retired
        );
    }

    #[tokio::test]
    async fn migration_expires_remote_and_revoked_trash_without_restarting_permanent_purge() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                db.put_node_fixture(&json!({ "id": ACTIVE, "revoked": false }))?;
                db.put_node_fixture(&json!({ "id": REVOKED, "revoked": true }))?;
                for (id, node, published) in [
                    ("trash-remote", ACTIVE, true),
                    ("trash-revoked", REVOKED, false),
                    ("trash-local", ACTIVE, false),
                    ("purging", ACTIVE, true),
                ] {
                    run(db, id, Some(node))?;
                    db.save_conversation_boundary(&json!({
                        "id": format!("chat-{id}"), "runId": id,
                        "lifecycle": if id == "purging" { "purging" } else { "trash" },
                        "previousLifecycle": "active", "paused": true,
                        "trashedAt": 1, "purgeAt": now() + 86_400_000,
                    }))?;
                    if published {
                        db.put(
                            "node-backups",
                            &json!({
                                "id": format!("point-{id}"), "runId": id, "destination": "s3",
                            }),
                        )?;
                    }
                }
                Ok(())
            })
            .await
            .unwrap();
        store.transaction(migrate).await.unwrap();
        for id in ["trash-remote", "trash-revoked"] {
            let run = store.run(id).await.unwrap();
            assert_eq!(run["environmentState"], "expired", "{id}");
            assert_eq!(run["environmentExpirationCause"], "storage-migration");
            let chat = store
                .conversation_boundary(&format!("chat-{id}"))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(chat["lifecycle"], "trash");
        }
        for id in ["trash-local", "purging"] {
            let run = store.run(id).await.unwrap();
            assert!(run["environmentState"].is_null(), "{id}");
            assert_eq!(run["sessionId"], "retained-session");
        }
        let retired = store.list("environment-retirements").await.unwrap();
        assert_eq!(retired.len(), 2);
        store.transaction(migrate).await.unwrap();
        assert_eq!(
            store.list("environment-retirements").await.unwrap(),
            retired
        );
    }

    #[tokio::test]
    async fn migration_preserves_never_published_work_while_abandoning_all_move_state() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store.transaction(|db| {
            db.put_node_fixture(&json!({ "id": ACTIVE, "revoked": false }))?;
            for (id, automatic) in [("explicit", false), ("capacity", true), ("recovery", true), ("backend", false)] {
                run(db, id, Some("offline-node"))?;
                let reservation = crate::config::id();
                db.put_attempt_fixture(&json!({ "id": reservation, "runId": id, "nodeId": ACTIVE, "role": "destination" }))?;
                let backend_reservation = crate::config::id();
                if id == "backend" {
                    db.put_attempt_fixture(&json!({ "id": backend_reservation, "runId": id, "nodeId": ACTIVE, "role": "destination" }))?;
                }
                db.update_run_boundary_fixture(id, &json!({
                    "moveRequest": { "nodeId": "destination", "reservation": reservation, "automatic": automatic },
                    "moveReservation": reservation,
                    "targetNodeId": "destination",
                    "backendMigration": if id == "backend" { json!({ "reservationId": backend_reservation, "nodeId": "destination" }) } else { Value::Null },
                }))?;
                RunCheckpoint::from_value(json!({ "moveRecoveryReservation": "old-attempt" }))?.store_in(db, id)?;
            }
            Ok(())
        }).await.unwrap();
        store.transaction(migrate).await.unwrap();
        store
            .read(|db| {
                assert_eq!(db.attempts()?.len(), 5);
                assert!(db.attempts()?.iter().all(|attempt| attempt.released));
                Ok(())
            })
            .await
            .unwrap();
        for id in ["explicit", "capacity", "recovery", "backend"] {
            let current = store.run(id).await.unwrap();
            assert_eq!(current["nodeId"], "offline-node");
            assert_eq!(current["sessionId"], "retained-session");
            assert!(current["environmentState"].is_null());
            for removed in [
                "moveRequest",
                "moveReservation",
                "targetNodeId",
                "backendMigration",
            ] {
                assert!(current[removed].is_null(), "{id}/{removed}");
            }
            assert!(
                RunCheckpoint::load(&store, id)
                    .await
                    .unwrap()
                    .unwrap()
                    .move_recovery_reservation
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn checkpoint_only_move_authorizations_are_cancelled_without_changing_normal_recovery() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let checkpoint = json!({
            "prepared": { "backend": "firecracker", "isolated": true },
            "runnerId": "attempt", "completed": false, "terminalCode": 75,
            "remainingMs": 1234, "controllerRecoveries": 4,
            "controllerRecoveryReservation": "attempt",
            "providerRetryReservation": "provider-attempt",
        });
        let original = checkpoint.clone();
        store
            .transaction(move |db| {
                db.put_node_fixture(&json!({ "id": ACTIVE, "revoked": false }))?;
                for id in ["move", "ordinary"] {
                    run(db, id, Some(ACTIVE))?;
                    db.update_run_placement(
                        id,
                        crate::nodes::run_placement::PlacementChanges {
                            target_node_id: Some(Some(ACTIVE.to_owned())),
                            ..Default::default()
                        },
                    )?;
                    let mut saved = RunCheckpoint::from_value(original.clone())?;
                    if id == "move" {
                        saved.move_recovery_reservation = Some("attempt".to_owned());
                    }
                    saved.store_in(db, id)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        for _ in 0..2 {
            store.transaction(migrate).await.unwrap();
            for id in ["move", "ordinary"] {
                let saved = RunCheckpoint::load(&store, id).await.unwrap().unwrap();
                assert_eq!(saved.to_value().unwrap(), checkpoint, "{id}");
                let current = store.run(id).await.unwrap();
                assert_eq!(current["nodeId"], ACTIVE, "{id}");
                assert_eq!(current["targetNodeId"], ACTIVE, "{id}");
                assert_eq!(current["sessionId"], "retained-session", "{id}");
                assert!(current["environmentState"].is_null(), "{id}");
            }
            assert!(
                store
                    .list("environment-retirements")
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn a_failed_migration_rolls_back_detachments_and_its_marker() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                for id in ["first", "second"] {
                    run(db, id, Some("source"))?;
                    db.update_run_boundary(id, &json!({ "backup": { "id": "remote" } }))?;
                }
                db.0.execute_batch(
                    "CREATE TRIGGER refuse_migration BEFORE UPDATE ON runs
                WHEN NEW.id='second' BEGIN SELECT RAISE(ABORT,'injected migration failure'); END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(store.transaction(migrate).await.is_err());
        store
            .read(|db| {
                assert!(db.kv(MARKER)?.is_none());
                assert!(db.list("environment-retirements")?.is_empty());
                for id in ["first", "second"] {
                    let current = db.run(id)?.unwrap();
                    assert!(current["environmentState"].is_null());
                    assert_eq!(current["sessionId"], "retained-session");
                }
                Ok(())
            })
            .await
            .unwrap();
        store
            .transaction(|db| {
                db.0.execute_batch("DROP TRIGGER refuse_migration")?;
                migrate(db)
            })
            .await
            .unwrap();
        assert_eq!(
            store.list("environment-retirements").await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn interrupted_chat_and_standalone_mission_keep_their_old_writer_in_retirement() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(|db| {
                for (id, trigger) in [("chat-run", "chat"), ("mission-run", "mission")] {
                    db.add_run_fixture(&json!({
                    "id": id, "taskId": id, "status": "running", "createdAt": 0,
                    "startedAt": 1, "trigger": trigger, "isolated": true, "nodeId": "old-node",
                    "backup": { "id": "old-point" }, "sessionId": "old-session",
                }), None)?;
                    RunCheckpoint::from_value(json!({
                        "runnerId": format!("attempt-{id}"), "nodeId": "old-node", "launched": true,
                        "remainingMs": 123456,
                    }))?
                    .store_in(db, id)?;
                }
                db.save_conversation_boundary(
                    &json!({ "id": "chat", "runId": "chat-run", "createdAt": 0 }),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        store.transaction(migrate).await.unwrap();
        let chat = store.run("chat-run").await.unwrap();
        assert_eq!(chat["status"], "cancelled");
        let mission = store.run("mission-run").await.unwrap();
        assert_eq!(mission["status"], "queued");
        assert_eq!(mission["environmentExpirationCause"], "storage-migration");
        assert!(mission["sessionId"].is_null());
        assert_eq!(
            RunCheckpoint::load(&store, "mission-run")
                .await
                .unwrap()
                .unwrap()
                .remaining_ms,
            Some(Some(123456))
        );
        store
            .read(|db| {
                for id in ["chat-run", "mission-run"] {
                    assert!(crate::environment_expiration::admission_blocked(db, id)?);
                }
                for retirement in db.list("environment-retirements")? {
                    assert_eq!(
                        retirement["attempt"],
                        format!("attempt-{}", text(&retirement, "runId"))
                    );
                    assert_eq!(retirement["attemptNode"], "old-node");
                    assert_eq!(retirement["fenced"], false);
                }
                Ok(())
            })
            .await
            .unwrap();
    }
}
