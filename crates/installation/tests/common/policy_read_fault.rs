//! One-shot reader failure without changing the authoritative stored grant.
use cairn_installation::store::Store;
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, Ordering},
};

pub async fn fail_next_grant_read(store: &Store) {
    // Occupy every reader once so each connection gets the same temporary view.
    // The writer and durable tables remain untouched. Exactly the next grant
    // lookup fails; later lookups exercise successful ordinary authorization.
    let barrier = Arc::new(Barrier::new(4));
    let fail = Arc::new(AtomicBool::new(true));
    let mut jobs = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let (store, barrier, fail) = (store.clone(), barrier.clone(), fail.clone());
        jobs.spawn(async move {
            store
                .read(move |db| {
                    db.0.create_scalar_function(
                        "plan_policy_read",
                        0,
                        rusqlite::functions::FunctionFlags::SQLITE_UTF8
                            | rusqlite::functions::FunctionFlags::SQLITE_INNOCUOUS,
                        move |_| {
                            if fail.swap(false, Ordering::SeqCst) {
                                return Err(rusqlite::Error::UserFunctionError(
                                    std::io::Error::other("injected Plan policy read failure").into(),
                                ));
                            }
                            Ok(true)
                        },
                    )?;
                    db.0.execute_batch(
                        "PRAGMA query_only=OFF;
                         CREATE TEMP VIEW mcp_run_grants AS SELECT * FROM main.mcp_run_grants WHERE plan_policy_read();
                         PRAGMA query_only=ON;",
                    )?;
                    barrier.wait();
                    Ok(())
                })
                .await
                .unwrap();
        });
    }
    while let Some(result) = jobs.join_next().await {
        result.unwrap();
    }
}
