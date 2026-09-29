//! Durable ownership of immutable shared objects. Reads use authenticated manifest
//! locators, never this inventory. All mutation helpers run in a caller transaction.
use crate::{
    config::{id, now},
    error::{Error, Result},
    service::Service,
    store::Db,
    validation::text,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc, time::Duration};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS shared_publications(id TEXT PRIMARY KEY,run_id TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS shared_publications_run ON shared_publications(run_id);
CREATE TABLE IF NOT EXISTS shared_objects(id TEXT PRIMARY KEY,destination TEXT NOT NULL,hash TEXT NOT NULL,size INTEGER NOT NULL,location TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','ready','invalid','deleting')),unused_at INTEGER,retry_at INTEGER NOT NULL DEFAULT 0);
CREATE UNIQUE INDEX IF NOT EXISTS shared_objects_reuse ON shared_objects(destination,hash) WHERE state='ready';
CREATE INDEX IF NOT EXISTS shared_objects_garbage ON shared_objects(retry_at,unused_at) WHERE unused_at IS NOT NULL;
CREATE TABLE IF NOT EXISTS shared_references(publication TEXT NOT NULL REFERENCES shared_publications(id) ON DELETE CASCADE,object TEXT NOT NULL REFERENCES shared_objects(id),PRIMARY KEY(publication,object));
CREATE INDEX IF NOT EXISTS shared_references_object ON shared_references(object);
CREATE TABLE IF NOT EXISTS remote_deletions(id TEXT PRIMARY KEY,location TEXT NOT NULL,key TEXT NOT NULL,prefix INTEGER NOT NULL DEFAULT 0,retry_at INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS remote_deletions_due ON remote_deletions(retry_at);
";

const GRACE_MS: i64 = 300_000;
const BATCH: i64 = 32;

#[derive(Clone, Debug)]
pub(crate) struct Object {
    pub id: String,
    pub hash: String,
    pub size: u64,
    pub ready: bool,
}

pub fn key(hash: &str, object: &str) -> String {
    format!("shared-blocks/v1/{hash}/{object}")
}

fn location(point: &Value) -> Value {
    json!({"destination":"s3", "bucket":point["bucket"], "endpoint":point["endpoint"]})
}

/// One transaction and cached statements for the entire candidate manifest. Only
/// verified incarnations are reused; a pending or deleting incarnation never blocks
/// a publisher and can never delete a replacement with the same content hash.
pub(crate) fn reserve(db: &Db<'_>, point: &Value, manifest: &mut Value) -> Result<Vec<Object>> {
    let location = serde_json::to_string(&location(point))?;
    let destination = hex::encode(Sha256::digest(location.as_bytes()));
    let publication = text(point, "id");
    db.0.execute(
        "INSERT INTO shared_publications VALUES(?1,?2)",
        params![publication, text(point, "runId")],
    )?;
    let mut lookup = db.0.prepare_cached(
        "SELECT id,size FROM shared_objects WHERE destination=?1 AND hash=?2 AND state='ready'",
    )?;
    let mut insert = db.0.prepare_cached("INSERT INTO shared_objects(id,destination,hash,size,location,state) VALUES(?1,?2,?3,?4,?5,'pending')")?;
    let mut reference =
        db.0.prepare_cached("INSERT INTO shared_references VALUES(?1,?2)")?;
    let mut revive = db.0.prepare_cached(
        "UPDATE shared_objects SET unused_at=NULL WHERE id=?1 AND unused_at IS NOT NULL",
    )?;
    let mut objects: HashMap<String, Object> = HashMap::new();
    for block in manifest["blocks"].as_array_mut().unwrap() {
        let Some(hash) = block["hash"].as_str().map(str::to_owned) else {
            continue;
        };
        let size = block["size"].as_u64().unwrap();
        if !objects.contains_key(&hash) {
            let existing = lookup
                .query_row(params![destination, hash], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .optional()?;
            let ready = existing.is_some();
            let object = if let Some((id, stored_size)) = existing {
                if stored_size != size as i64 {
                    return Err(Error::bad("Shared block size mismatch."));
                }
                revive.execute([&id])?;
                id
            } else {
                let id = id();
                insert.execute(params![id, destination, hash, size as i64, location])?;
                id
            };
            reference.execute(params![publication, object])?;
            objects.insert(
                hash.clone(),
                Object {
                    id: object,
                    hash: hash.clone(),
                    size,
                    ready,
                },
            );
        }
        let object = &objects[&hash];
        if object.size != size {
            return Err(Error::bad("Shared block size mismatch."));
        }
        block["object"] = object.id.clone().into();
    }
    Ok(objects.into_values().collect())
}

pub(crate) fn verified(db: &Db<'_>, publication: &str) -> Result<()> {
    let invalid: bool = db.0.query_row("SELECT EXISTS(SELECT 1 FROM shared_references r JOIN shared_objects o ON o.id=r.object WHERE r.publication=?1 AND o.state NOT IN ('pending','ready'))",[publication],|r|r.get(0))?;
    if invalid {
        return Err(Error::new(
            409,
            "A shared block was invalidated during publication.",
        ));
    }
    db.0.execute("UPDATE shared_objects SET state='ready' WHERE state='pending' AND id IN (SELECT object FROM shared_references WHERE publication=?1)",[publication])?;
    Ok(())
}

/// Called only after the owner publication has stopped and its readers drained.
pub(crate) fn release(db: &Db<'_>, publication: &str) -> Result<()> {
    // Mark only this publication's objects. A concurrent/new reference clears the
    // timestamp in reserve, in the same SQLite serialization order as GC claims.
    db.0.execute("UPDATE shared_objects SET unused_at=?2 WHERE id IN (SELECT object FROM shared_references WHERE publication=?1) AND NOT EXISTS (SELECT 1 FROM shared_references r WHERE r.object=shared_objects.id AND r.publication<>?1)",params![publication,now()])?;
    db.0.execute("DELETE FROM shared_publications WHERE id=?1", [publication])?;
    Ok(())
}

pub(crate) fn release_run(
    db: &Db<'_>,
    run: &str,
    keep: &std::collections::HashSet<String>,
) -> Result<()> {
    let mut stmt =
        db.0.prepare_cached("SELECT id FROM shared_publications WHERE run_id=?1")?;
    let ids = stmt
        .query_map([run], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in ids {
        if !keep.contains(&id) {
            release(db, &id)?;
        }
    }
    Ok(())
}

pub(crate) fn queue(db: &Db<'_>, point: &Value, key: &str, prefix: bool) -> Result<()> {
    let location = serde_json::to_string(&location(point))?;
    let id = hex::encode(Sha256::digest(format!("{location}\n{key}\n{prefix}")));
    db.0.execute(
        "INSERT OR IGNORE INTO remote_deletions(id,location,key,prefix) VALUES(?1,?2,?3,?4)",
        params![id, location, key, prefix],
    )?;
    Ok(())
}

pub(crate) async fn invalidate(s: &Service, object: &str) -> Result<()> {
    let object = object.to_owned();
    s.store
        .write(move |db| {
            db.0.execute(
                "UPDATE shared_objects SET state='invalid' WHERE id=?1 AND state='ready'",
                [object],
            )?;
            Ok(())
        })
        .await
}

#[derive(Clone)]
struct Deletion {
    id: String,
    location: Value,
    key: String,
    prefix: bool,
    shared: bool,
}

fn claim(db: &Db<'_>, cutoff: i64) -> Result<Vec<Deletion>> {
    let mut stmt=db.0.prepare_cached("SELECT id,hash,location FROM shared_objects WHERE unused_at<=?1 AND retry_at<=?3 AND NOT EXISTS (SELECT 1 FROM shared_references WHERE object=shared_objects.id) ORDER BY retry_at,unused_at LIMIT ?2")?;
    let rows = stmt
        .query_map(params![cutoff, BATCH, now()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut result = Vec::new();
    for (id, hash, location) in rows {
        db.0.execute(
            "UPDATE shared_objects SET state='deleting',retry_at=?2 WHERE id=?1",
            params![id, now() + 60_000],
        )?;
        result.push(Deletion {
            key: key(&hash, &id),
            id,
            location: serde_json::from_str(&location)?,
            prefix: false,
            shared: true,
        });
    }
    let mut stmt = db.0.prepare_cached(
        "SELECT id,location,key,prefix FROM remote_deletions WHERE retry_at<=?2 ORDER BY retry_at,rowid LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![BATCH, now()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, location, key, prefix) in rows {
        db.0.execute(
            "UPDATE remote_deletions SET retry_at=?2 WHERE id=?1",
            params![id, now() + 60_000],
        )?;
        result.push(Deletion {
            id,
            location: serde_json::from_str(&location)?,
            key,
            prefix,
            shared: false,
        });
    }
    Ok(result)
}

/// Bounded network work runs without publication/read locks. The separate worker
/// prevents slow S3 deletion from delaying scheduled publication or authorized GETs.
pub async fn collect(s: &Service) -> Result<usize> {
    let Ok(_collector) = s.shared_block_collection.try_lock() else {
        return Ok(0);
    };
    // Empty maintenance must not publish a database-change notification to every
    // subscribed UI. Recheck eligibility transactionally in claim after this hint.
    let due=s.store.read(|db| {
        Ok(db.0.query_row("SELECT EXISTS(SELECT 1 FROM shared_objects WHERE unused_at<=?1 AND retry_at<=?2) OR EXISTS(SELECT 1 FROM remote_deletions WHERE retry_at<=?2)",params![now()-GRACE_MS,now()],|r|r.get::<_,bool>(0))?)
    }).await?;
    if !due {
        return Ok(0);
    }
    let deletions = s
        .store
        .transaction(|db| claim(db, now() - GRACE_MS))
        .await?;
    use futures_util::{StreamExt, stream};
    let mut pending = stream::iter(deletions.into_iter().map(|deletion| {
        let storage = super::publication::storage_for(s, &deletion.location);
        async move {
            let result = async {
                let storage = storage?;
                if deletion.prefix {
                    storage.purge(&deletion.key).await?;
                } else {
                    storage.purge_key(&deletion.key).await?;
                }
                Ok::<(), Error>(())
            }
            .await;
            (deletion, result)
        }
    }))
    .buffer_unordered(4);
    let mut completed = Vec::new();
    let mut failure = None;
    while let Some((deletion, result)) = pending.next().await {
        match result {
            Ok(()) => completed.push(deletion),
            Err(error) => failure = Some(error),
        }
    }
    let count = completed.len();
    s.store.transaction(move |db| {
        for d in completed {
            if d.shared {db.0.execute("DELETE FROM shared_objects WHERE id=?1 AND state='deleting' AND NOT EXISTS (SELECT 1 FROM shared_references WHERE object=?1)",[d.id])?;}
            else {db.0.execute("DELETE FROM remote_deletions WHERE id=?1",[d.id])?;}
        }Ok(())
    }).await?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(count)
}

pub(crate) async fn maintain(s: Arc<Service>) {
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {_ = s.shutdown.cancelled()=>return, _=timer.tick()=>{}}
        if let Err(error) = collect(&s).await {
            tracing::warn!(message=%error.message,"Shared block cleanup will retry");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use tempfile::TempDir;

    fn point(run: &str, bucket: &str) -> Value {
        json!({"id":id(),"runId":run,"bucket":bucket,"endpoint":null})
    }

    fn manifest(hashes: &[&str]) -> Value {
        json!({"blocks":hashes.iter().map(|h|json!({"hash":h,"size":4096})).collect::<Vec<_>>()})
    }

    async fn reserve_point(store: &Store, point: &Value, hashes: &[&str]) -> Vec<Object> {
        let point = point.clone();
        let mut manifest = manifest(hashes);
        store
            .transaction(move |db| reserve(db, &point, &mut manifest))
            .await
            .unwrap()
    }

    async fn ready(store: &Store, point: &Value) {
        let id = text(point, "id").to_owned();
        store
            .transaction(move |db| verified(db, &id))
            .await
            .unwrap();
    }

    async fn retire(store: &Store, point: &Value) {
        let id = text(point, "id").to_owned();
        store.transaction(move |db| release(db, &id)).await.unwrap();
    }

    async fn garbage(store: &Store) -> Vec<Deletion> {
        store.transaction(|db| claim(db, i64::MAX)).await.unwrap()
    }

    #[tokio::test]
    async fn references_share_verified_objects_and_only_last_release_collects() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = point("a", "bucket");
        let b = point("b", "bucket");
        let first = reserve_point(&store, &a, &["hash", "hash"]).await;
        assert_eq!(first.len(), 1);
        assert!(!first[0].ready);
        ready(&store, &a).await;
        let second = reserve_point(&store, &b, &["hash"]).await;
        assert!(second[0].ready);
        assert_eq!(first[0].id, second[0].id);
        retire(&store, &a).await;
        assert!(garbage(&store).await.is_empty());
        retire(&store, &b).await;
        let deleted = garbage(&store).await;
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].key, key("hash", &first[0].id));
    }

    #[tokio::test]
    async fn deleting_incarnation_cannot_block_or_delete_resurrected_content() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = point("a", "bucket");
        let b = point("b", "bucket");
        let old = reserve_point(&store, &a, &["hash"]).await;
        ready(&store, &a).await;
        retire(&store, &a).await;
        let deletion = garbage(&store).await.remove(0);
        let new = reserve_point(&store, &b, &["hash"]).await;
        assert_ne!(old[0].id, new[0].id);
        assert!(!new[0].ready);
        ready(&store, &b).await;
        store
            .transaction(move |db| {
                db.0.execute(
                    "DELETE FROM shared_objects WHERE id=?1 AND state='deleting'",
                    [deletion.id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let c = point("c", "bucket");
        assert_eq!(reserve_point(&store, &c, &["hash"]).await[0].id, new[0].id);
        assert!(garbage(&store).await.is_empty());
    }

    #[tokio::test]
    async fn pending_refs_survive_restart_and_destination_and_size_are_checked() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = point("a", "bucket");
        let b = point("b", "different");
        let old = reserve_point(&store, &a, &["hash"]).await;
        let restarted = Store::open(dir.path()).unwrap();
        assert!(garbage(&restarted).await.is_empty());
        ready(&restarted, &a).await;
        let other = reserve_point(&restarted, &b, &["hash"]).await;
        assert_ne!(old[0].id, other[0].id);
        let c = point("c", "bucket");
        let mut bad = manifest(&["hash"]);
        bad["blocks"][0]["size"] = 8192.into();
        assert!(
            restarted
                .transaction(move |db| reserve(db, &c, &mut bad))
                .await
                .is_err()
        );
        retire(&restarted, &b).await;
        assert_eq!(garbage(&restarted).await.len(), 1);
        assert!(reserve_point(&restarted, &point("d", "bucket"), &["hash"]).await[0].ready);
    }

    #[tokio::test]
    async fn grace_allows_reuse_before_claim_and_abandoned_uploads_are_not_reused() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = point("a", "bucket");
        let b = point("b", "bucket");
        let c = point("c", "bucket");
        let pending = reserve_point(&store, &a, &["hash"]).await;
        let live = reserve_point(&store, &b, &["hash"]).await;
        assert_ne!(pending[0].id, live[0].id);
        ready(&store, &b).await;
        retire(&store, &a).await;
        retire(&store, &b).await;
        assert!(
            store
                .transaction(|db| claim(db, now() - GRACE_MS))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(reserve_point(&store, &c, &["hash"]).await[0].id, live[0].id);
        let garbage = garbage(&store).await;
        assert_eq!(garbage.len(), 1);
        assert_eq!(garbage[0].id, pending[0].id);
    }

    #[tokio::test]
    async fn invalidation_during_publication_rejects_commit_and_forces_new_incarnation() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = point("a", "bucket");
        let b = point("b", "bucket");
        let old = reserve_point(&store, &a, &["hash"]).await;
        ready(&store, &a).await;
        reserve_point(&store, &b, &["hash"]).await;
        store
            .transaction(|db| {
                db.0.execute("UPDATE shared_objects SET state='invalid'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let b_id = text(&b, "id").to_owned();
        assert!(
            store
                .transaction(move |db| verified(db, &b_id))
                .await
                .is_err()
        );
        assert_ne!(
            reserve_point(&store, &point("c", "bucket"), &["hash"]).await[0].id,
            old[0].id
        );
    }

    #[tokio::test]
    async fn failed_claims_back_off_without_starving_later_garbage() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = point("a", "bucket");
        let hashes = (0..64).map(|i| format!("hash-{i}")).collect::<Vec<_>>();
        reserve_point(
            &store,
            &a,
            &hashes.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .await;
        retire(&store, &a).await;
        store
            .transaction(|db| {
                for i in 0..64 {
                    queue(
                        db,
                        &json!({"bucket":"bucket"}),
                        &format!("legacy-{i}"),
                        false,
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let first = garbage(&store).await;
        assert_eq!(first.len(), 64);
        // Simulate every network deletion failing: do not acknowledge any claim.
        let second = garbage(&store).await;
        assert_eq!(second.len(), 64);
        assert!(second.iter().all(|b| !first.iter().any(|a| a.id == b.id)));
        assert!(
            garbage(&store).await.is_empty(),
            "failures are not hammered every tick"
        );
        let restarted = Store::open(dir.path()).unwrap();
        assert!(
            garbage(&restarted).await.is_empty(),
            "backoff survives restart"
        );
    }

    #[tokio::test]
    #[ignore = "manual metadata performance measurement"]
    async fn benchmark_shared_manifest_inventory() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let hashes = (0..4096).map(|i| format!("{i:064x}")).collect::<Vec<_>>();
        let refs = hashes.iter().map(String::as_str).collect::<Vec<_>>();
        let mut samples = Vec::new();
        for run in 0..20 {
            let point = point(&run.to_string(), "bucket");
            let start = std::time::Instant::now();
            let objects = reserve_point(&store, &point, &refs).await;
            ready(&store, &point).await;
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(
                objects.iter().filter(|o| !o.ready).count(),
                if run == 0 { 4096 } else { 0 }
            );
        }
        eprintln!(
            "4096-block manifests: initial {:.2}ms; 19 reused {:?}ms",
            samples[0],
            &samples[1..]
        );
    }
}
