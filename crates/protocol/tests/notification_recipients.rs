use cairn_protocol::{NotificationMember, NotificationRecipients};
use serde_json::json;

#[test]
fn recipient_evidence_requires_both_the_account_and_its_membership() {
    let recipients = NotificationRecipients {
        members: vec![NotificationMember {
            account_id: "member".into(),
            access_id: "membership".into(),
        }],
    };
    let encoded = serde_json::to_value(recipients).unwrap();
    let decoded: NotificationRecipients = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.members[0].access_id, "membership");
    for missing in [
        json!({}),
        json!({ "members": ["member"] }),
        json!({ "members": [{ "account_id": "member" }] }),
    ] {
        assert!(serde_json::from_value::<NotificationRecipients>(missing).is_err());
    }
}
