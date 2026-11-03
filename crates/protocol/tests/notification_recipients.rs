use cairn_protocol::{
    NotificationEvent, NotificationKind, NotificationMember, NotificationRecipients,
};
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

#[test]
fn notification_event_keeps_legacy_events_fail_closed_and_round_trips_question_audience() {
    let legacy: NotificationEvent = serde_json::from_value(json!({
        "id": "question",
        "chatId": "chat",
        "kind": "question",
        "questionId": "question",
        "members": []
    }))
    .unwrap();
    assert!(legacy.question_recipients.is_none());

    let event = NotificationEvent {
        id: "question".into(),
        chat_id: "chat".into(),
        kind: NotificationKind::Question {
            question_id: "question".into(),
        },
        members: Some(Vec::new()),
        question_recipients: Some(vec![NotificationMember {
            account_id: "member".into(),
            access_id: "membership".into(),
        }]),
    };
    let encoded = serde_json::to_value(&event).unwrap();
    assert_eq!(
        encoded["questionRecipients"][0],
        json!({ "account_id": "member", "access_id": "membership" })
    );
    let decoded: NotificationEvent = serde_json::from_value(encoded).unwrap();
    let audience = decoded.question_recipients.unwrap();
    assert_eq!(audience.len(), 1);
    assert_eq!(audience[0].account_id, "member");
    assert_eq!(audience[0].access_id, "membership");
}
