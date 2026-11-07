//! Bounded reads of the atomically replaced chat inbox, before JSON decoding.
use crate::error::{Error, Result};
use std::path::Path;

pub(crate) const ADAPTER_LIMIT: usize = 2_000_000;
// Transport reads allow headroom above the adapters’ 2 MB validation limit.
pub(crate) const TRANSPORT_LIMIT: usize = 16_000_000;

async fn messages(directory: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let Ok(file) = tokio::fs::File::open(directory.join("messages.json")).await else {
        return Ok(None);
    };
    match crate::process::read_bounded(file, limit).await {
        Ok(bytes) => Ok(Some(bytes)),
        // read_bounded reports overflow as 502; adapt its executable-output error
        // to the inbox protocol. Other errors are I/O failures, ignored as before.
        Err(error) if error.status == 502 => Err(Error::bad("Chat inbox is too large.")),
        Err(_) => Ok(None),
    }
}

/// Whether an inbox message may join the running turn of `plan`. A plan request never
/// joins a turn that can change files: it stays queued for its own plan turn, even when
/// it was written to the inbox for an earlier turn. Other messages join any turn, which
/// keeps its own restrictions.
pub(crate) fn joins_turn(message: &serde_json::Value, plan: &serde_json::Value) -> bool {
    message["plan"] != true || plan["planning"] == true
}

/// The controller owns a separate notice lane. Replacing user steering cannot
/// lose a disk warning, and a warning never creates a user message or authority.
pub(crate) async fn read(directory: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let original = messages(directory, limit).await?;
    let Ok(file) = tokio::fs::File::open(directory.join("disk-notice.json")).await else {
        return Ok(original);
    };
    let Ok(bytes) = crate::process::read_bounded(file, 16384).await else {
        return Ok(original);
    };
    let Ok(notice) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(original);
    };
    if notice["operationalNotice"] != true
        || notice["id"].as_str().is_none()
        || notice["text"].as_str().is_none()
    {
        return Ok(original);
    }
    let mut inbox: Vec<serde_json::Value> = match original.as_deref() {
        Some(bytes) => match serde_json::from_slice(bytes) {
            Ok(messages) => messages,
            Err(_) => return Ok(original),
        },
        None => Vec::new(),
    };
    inbox.push(notice);
    let encoded = serde_json::to_vec(&inbox)?;
    if encoded.len() > limit {
        return Ok(original);
    }
    Ok(Some(encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn operational_notices_survive_steering_replacement_without_becoming_user_messages() {
        let root = tempfile::tempdir().unwrap();
        let notice = serde_json::json!({ "id": "notice", "operationalNotice": true, "text": "Disk pressure" });
        tokio::fs::write(root.path().join("disk-notice.json"), notice.to_string())
            .await
            .unwrap();
        for messages in ["[]", "[{\"id\":\"user\",\"text\":\"Continue\"}]"] {
            tokio::fs::write(root.path().join("messages.json"), messages)
                .await
                .unwrap();
            let bytes = read(root.path(), ADAPTER_LIMIT).await.unwrap().unwrap();
            let inbox: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(inbox.last(), Some(&notice));
            assert_eq!(
                tokio::fs::read(root.path().join("messages.json"))
                    .await
                    .unwrap(),
                messages.as_bytes()
            );
        }
    }

    #[tokio::test]
    async fn missing_and_unreadable_inboxes_remain_absent() {
        let root = tempfile::tempdir().unwrap();
        assert!(read(root.path(), ADAPTER_LIMIT).await.unwrap().is_none());
        tokio::fs::create_dir(root.path().join("messages.json"))
            .await
            .unwrap();
        assert!(read(root.path(), ADAPTER_LIMIT).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reads_exact_limit_and_rejects_one_more_byte() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("messages.json");
        for limit in [ADAPTER_LIMIT, TRANSPORT_LIMIT] {
            let bytes = vec![b' '; limit];
            tokio::fs::write(&path, &bytes).await.unwrap();
            assert_eq!(read(root.path(), limit).await.unwrap().unwrap(), bytes);
            tokio::fs::write(&path, vec![b' '; limit + 1])
                .await
                .unwrap();
            let error = read(root.path(), limit).await.unwrap_err();
            assert_eq!(error.status, 400);
            assert_eq!(error.message, "Chat inbox is too large.");
        }
    }
}
