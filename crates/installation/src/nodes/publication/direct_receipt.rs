//! Settle the object grants only after the caller verifies a complete batch receipt.
use crate::{error::Result, store::Store};

pub(super) async fn settle(store: &Store, objects: Vec<String>) -> Result<()> {
    store
        .transaction(move |db| {
            for object in objects {
                db.0.execute(
                    "UPDATE direct_storage_grants SET settled=1 WHERE object=?1",
                    [object],
                )?;
            }
            Ok(())
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn grants(store: &Store) -> Vec<String> {
        let objects: Vec<_> = (0..crate::storage::direct::BATCH)
            .map(|index| format!("synthetic-receipt-object-{index}"))
            .collect();
        let retained = objects.clone();
        store
            .transaction(move |db| {
                for object in retained {
                    db.0.execute(
                        "INSERT INTO shared_objects(id,destination,hash,size,location,state)
                         VALUES(?1,'synthetic',?1,1,'{}','pending')",
                        [&object],
                    )?;
                    db.0.execute(
                        "INSERT INTO direct_storage_grants VALUES(?1,123,0)",
                        [&object],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();

        objects
    }

    async fn settled(store: &Store) -> Vec<(i64, i64)> {
        store
            .read(|db| {
                Ok(db
                    .0
                    .prepare(
                        "SELECT settled,expires_at FROM direct_storage_grants ORDER BY object",
                    )?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn complete_receipt_uses_one_full_durability_commit() {
        static COMMITS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn count_commit(_: *mut libc::c_void) -> libc::c_int {
            COMMITS.fetch_add(1, Ordering::SeqCst);
            0
        }

        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let objects = grants(&store).await;
        store
            .write(|db| {
                assert_eq!(
                    db.0.pragma_query_value::<i64, _>(None, "synchronous", |row| row.get(0))?,
                    2
                );
                // SAFETY: the writer owns this connection. The static callback ignores
                // its null context, never unwinds, and accepts the normal commit.
                unsafe {
                    rusqlite::ffi::sqlite3_commit_hook(
                        db.0.handle(),
                        Some(count_commit),
                        std::ptr::null_mut(),
                    );
                }
                Ok(())
            })
            .await
            .unwrap();
        settle(&store, objects.clone()).await.unwrap();
        assert_eq!(COMMITS.load(Ordering::SeqCst), 1);
        assert_eq!(settled(&store).await, vec![(1, 123); objects.len()]);
    }

    #[tokio::test]
    async fn failed_settlement_retains_the_entire_batch_and_can_retry() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let objects = grants(&store).await;
        store
            .write(|db| {
                db.0.execute_batch(
                    "CREATE TRIGGER fail_receipt_settlement
                     BEFORE UPDATE ON direct_storage_grants
                     WHEN NEW.object='synthetic-receipt-object-1'
                     BEGIN SELECT RAISE(ABORT,'synthetic receipt settlement failure'); END",
                )?;
                Ok(())
            })
            .await
            .unwrap();

        assert!(settle(&store, objects.clone()).await.is_err());
        assert_eq!(settled(&store).await, vec![(0, 123); objects.len()]);

        store
            .write(|db| {
                db.0.execute_batch("DROP TRIGGER fail_receipt_settlement")?;
                Ok(())
            })
            .await
            .unwrap();
        settle(&store, objects.clone()).await.unwrap();
        assert_eq!(settled(&store).await, vec![(1, 123); objects.len()]);
        settle(&store, objects.clone()).await.unwrap();
        assert_eq!(settled(&store).await, vec![(1, 123); objects.len()]);
    }
}
