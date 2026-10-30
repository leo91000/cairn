use cairn_protocol::TaskAuthorPolicy;
use serde::Deserialize;
use serde_json::json;

#[test]
fn older_beacon_memberships_remain_valid_without_rollout_evidence() {
    let policy: TaskAuthorPolicy = serde_json::from_value(json!({
        "owner": { "account_id": "owner", "access_id": "owner-access" },
        "members": [{ "account_id": "member", "access_id": "membership" }],
    }))
    .unwrap();
    let grant = policy.grant("member").unwrap();
    assert_eq!(grant.access_id, "membership");
    assert!(grant.joined_at.is_none());
}

#[test]
fn older_installations_ignore_membership_rollout_evidence() {
    #[derive(Deserialize)]
    struct LegacyGrant {
        account_id: String,
        access_id: String,
    }

    let grant: LegacyGrant = serde_json::from_value(json!({
        "account_id": "member",
        "access_id": "membership",
        "joined_at": 1000,
    }))
    .unwrap();
    assert_eq!(grant.account_id, "member");
    assert_eq!(grant.access_id, "membership");
}
