use cairn_protocol::TaskAuthorPolicy;
use serde_json::json;

#[test]
fn membership_policy_uses_account_and_membership_identity_only() {
    let policy: TaskAuthorPolicy = serde_json::from_value(json!({
        "owner": { "account_id": "owner", "access_id": "owner-access" },
        "members": [{
            "account_id": "member",
            "access_id": "membership",
            "joined_at": 1000,
        }],
    }))
    .unwrap();
    let grant = policy.grant("member").unwrap();
    assert_eq!(grant.access_id, "membership");

    let wire = serde_json::to_value(policy).unwrap();
    assert!(wire["members"][0].get("joined_at").is_none());
}
