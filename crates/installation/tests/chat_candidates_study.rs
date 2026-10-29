//! Store-level experiment only: no candidate policy is installed in the worker.
use cairn_installation::store::Store;
use serde_json::json;
use std::collections::HashSet;

#[tokio::test]
async fn grouped_pending_ids_match_native_histories_in_existing_chat_order() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            for (i, status) in ["delivered", "cancelled", "queued", "sending"]
                .into_iter()
                .enumerate()
            {
                let chat_id = format!("chat-{i}");
                db.save_conversation_boundary(&json!({
                    "id": chat_id,
                    "updatedAt": i,
                    "slug": format!("synthetic-{i}"),
                }))?;
                // The writer uses the wall clock for persisted_at. Set a
                // deterministic fixture order independently of public updatedAt.
                let updated_at = i as i64;
                db.0.execute(
                    "UPDATE conversations SET persisted_at=? WHERE id=?",
                    rusqlite::params![updated_at, chat_id],
                )?;
                for j in 0..2 {
                    db.put_message(&json!({
                        "id": format!("message-{i}-{j}"),
                        "chatId": chat_id,
                        "createdAt": 0,
                        "status": status,
                        "text": "Synthetic candidate fixture",
                    }))?;
                }
            }
            db.save_conversation_boundary(&json!({
                "id": "no-messages",
                "updatedAt": 10,
                "slug": "synthetic-empty",
            }))?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .read(|db| {
            let grouped: HashSet<String> = db
                .json_rows_as(
                    "SELECT DISTINCT json_quote(chat_id) FROM chat_messages
                     WHERE status IN ('queued','sending')",
                    [],
                )?
                .into_iter()
                .collect();
            assert_eq!(grouped.len(), 2);

            let chats = db.conversations_boundary()?;
            let mut reference = Vec::new();
            for chat in &chats {
                let id = chat["id"].as_str().unwrap();
                if db
                    .messages(id)?
                    .iter()
                    .any(|message| message["status"] == "queued" || message["status"] == "sending")
                {
                    reference.push(id.to_owned());
                }
            }

            let selected: Vec<_> = chats
                .iter()
                .map(|chat| chat["id"].as_str().unwrap().to_owned())
                .filter(|id| grouped.contains(id))
                .collect();
            assert_eq!(selected, reference);
            assert_eq!(selected, ["chat-3", "chat-2"]);

            // Equal timestamps retain the store's created_at,rowid ordering.
            let sending = db.messages("chat-3")?;
            assert_eq!(sending[0]["id"], "message-3-0");
            assert_eq!(sending[1]["id"], "message-3-1");
            Ok(())
        })
        .await
        .unwrap();
}
