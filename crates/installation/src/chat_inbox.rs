//! Bounded reads of the atomically replaced chat inbox, before JSON decoding.
use crate::error::{Error, Result};
use std::path::Path;

pub(crate) const ADAPTER_LIMIT: usize = 2_000_000;
// Transport reads allow headroom above the adapters’ 2 MB validation limit.
pub(crate) const TRANSPORT_LIMIT: usize = 16_000_000;

pub(crate) async fn read(directory: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
