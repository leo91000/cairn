use super::*;
use crate::store::Store;
use cairn_contracts::{Agent, RunSnapshot, RunState};

#[tokio::test]
async fn native_lease_uses_the_proof_captured_before_refresh_published() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let account = id();
            let node_id = id();
            let node = ExecutionNode {
                id: NodeId(uuid::Uuid::parse_str(&node_id).map_err(Error::internal)?),
                name: "Native node".into(),
                revision: id(),
                native_enabled: true,
                native_accounts: vec![account.clone()],
                capabilities: Capabilities {
                    backend: NodeBackend::Native,
                    native_mode_protocol: 1,
                    ..Default::default()
                },
                limits: Resources { cpu: 1, memory_mi_b: 128, disk_mi_b: 128 },
                native_mode_observed: Some(decode(json!({
                    "mode": "protected",
                    "status": "applied",
                }))?),
                ..Default::default()
            };
            db.save_node(&node)?;

            let agent: Agent = decode(json!({
                "id": id(),
                "name": "Native agent",
                "revision": id(),
                "createdAt": 0,
                "access": { "backend": "native", "nodes": [node_id], "mcpTools": {} },
                "exclusiveNodes": [],
                "plugins": [],
            }))?;
            db.save_agent(&agent)?;
            let run: RunState = decode(json!({ "id": id(), "taskId": id(), "status": "running" }))?;
            let snapshot: RunSnapshot = decode(json!({
                "agent": agent,
                "task": { "id": run.task_id, "name": "Fixture", "prompt": "Fixture", "agentId": agent.id },
            }))?;
            db.admit_run(&run, Some(&snapshot), &json!({
                "nativePrincipals": [account],
                "nativeAuthorityNodes": [node_id],
            }), None)?;
            let attempt = id();
            db.save_checkpoint_boundary(&run.id, &json!({ "runnerId": attempt, "nodeId": node_id }))?;
            db.save_attempt(&decode(json!({
                "id": attempt, "runId": run.id, "nodeId": node_id,
                "resources": node.limits, "createdAt": now(), "runtimeId": "development",
            }))?)?;

            let observed_at = now() - 30_000;
            db.set("native-authority", &json!({
                "observedAt": observed_at, "accounts": { account: { "accessId": "one" } },
            }), None)?;
            let previous = native_policy::AuthorityProof::load(db)?;
            // The background refresh publishes before the heartbeat's lease decision.
            db.set("native-authority", &json!({ "observedAt": now(), "accounts": {} }), None)?;

            let key = "node-token:fixture";
            db.set(key, &json!(node_id), None)?;
            let body = json!({ "runtimeId": component_runtime(), "nodeProtocol": 2 });
            let expected = Expectations { data_root: String::new(), image: None, lease_ms: 60_000 };

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
            Ok(())
        })
        .await
        .unwrap();
}
