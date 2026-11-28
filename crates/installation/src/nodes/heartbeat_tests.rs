use super::*;
use crate::{
    agent_fixture, attempt_fixture::DbAttemptFixture, node_fixture::DbNodeFixture,
    run_fixture::DbRunFixture, store::Store,
};

#[tokio::test]
async fn native_lease_uses_the_proof_captured_before_refresh_published() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let account = id();
            let node_id = id();
            db.put_node_fixture(&json!({
                "id": node_id,
                "nativeEnabled": true,
                "nativeAccounts": [account],
                "capabilities": { "backend": "native", "kvm": false, "nativeModeProtocol": 1 },
                "nativeModeObserved": { "mode": "protected", "status": "applied" },
            }))?;
            let agent = agent_fixture::profile(json!({
                "id": id(),
                "access": { "backend": "native", "nodes": [node_id] },
            }))?;
            db.save_agent(&agent)?;

            let run_id = id();
            db.add_run_fixture(
                &json!({
                    "id": run_id,
                    "taskId": id(),
                    "status": "running",
                    "snapshot": { "agent": agent },
                    "nativePrincipals": [account],
                    "nativeAuthorityNodes": [node_id],
                }),
                None,
            )?;
            let attempt = id();
            db.save_checkpoint_boundary(
                &run_id,
                &json!({ "runnerId": attempt, "nodeId": node_id }),
            )?;
            db.put_attempt_fixture(&json!({ "id": attempt, "runId": run_id, "nodeId": node_id }))?;

            let observed_at = now() - 30_000;
            db.set(
                "native-authority",
                &json!({
                    "observedAt": observed_at, "accounts": { (account.as_str()): { "accessId": "one" } },
                }),
                None,
            )?;
            let previous = native_policy::AuthorityProof::load(db)?;
            // The background refresh publishes before the heartbeat's lease decision.
            db.set(
                "native-authority",
                &json!({ "observedAt": now(), "accounts": {} }),
                None,
            )?;

            let key = "node-token:fixture";
            db.set(key, &json!(node_id), None)?;
            let body = json!({ "runtimeId": component_runtime(), "nodeProtocol": 2 });
            let expected = Expectations {
                data_root: String::new(),
                image: None,
                lease_ms: 60_000,
            };

            let first = record_heartbeat(db, key, &body, None, &expected, Some(&previous))??;
            assert_eq!(first.leases.len(), 1);
            assert_eq!(first.leases[0].id, attempt);
            assert!((1..=30_000).contains(&first.leases[0].remaining_ms));
            assert_eq!(
                db.attempt(&attempt)?.unwrap().lease_expires_at,
                Some(observed_at + 60_000),
            );

            let current = native_policy::AuthorityProof::load(db)?;
            let second = record_heartbeat(db, key, &body, None, &expected, Some(&current))??;
            assert!(second.leases.is_empty());

            let renewed_at = now();
            db.set(
                "native-authority",
                &json!({
                    "observedAt": renewed_at,
                    "accounts": { (account.as_str()): { "accessId": "one" } },
                }),
                None,
            )?;
            let current = native_policy::AuthorityProof::load(db)?;
            let newer = record_heartbeat(db, key, &body, None, &expected, Some(&current))??;
            assert_eq!(newer.leases.len(), 1);

            let delayed = record_heartbeat(db, key, &body, None, &expected, Some(&previous))??;
            assert!(delayed.leases.is_empty());
            assert_eq!(
                db.attempt(&attempt)?.unwrap().lease_expires_at,
                Some(renewed_at + 60_000),
            );
            Ok(())
        })
        .await
        .unwrap();
}
