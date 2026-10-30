//! Optional byte cursors for reconnecting to one attempt without replaying output.
use crate::{
    error::{Error, Result},
    microvm::wire,
};
use axum::{
    body::Body,
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{future::Future, path::PathBuf, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub(crate) const CONTENT_TYPE: &str = "application/vnd.cairn.log-cursor+ndjson";

pub(crate) fn offset(query: Option<&str>) -> Result<Option<u64>> {
    let Some(query) = query else {
        return Ok(None);
    };
    let mut cursor = None;
    let mut offset = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "cursor" if cursor.is_none() && value == "1" => cursor = Some(()),
            "offset" if offset.is_none() => {
                offset = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| Error::bad("Invalid log cursor."))?,
                );
            }
            _ => return Err(Error::bad("Invalid log cursor.")),
        }
    }
    if cursor.is_none() {
        return Err(Error::bad("Missing log cursor protocol."));
    }
    Ok(Some(offset.unwrap_or(0)))
}

struct Reader<F> {
    file: tokio::fs::File,
    offset: u64,
    pending: Vec<u8>,
    retired: bool,
    ended: bool,
    finished: F,
}

pub(crate) async fn response<F, Fut>(path: PathBuf, offset: u64, finished: F) -> Result<Response>
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = bool> + Send,
{
    let mut file = tokio::fs::File::open(path).await?;
    let length = file.metadata().await?.len();
    if offset > length {
        return Err(Error::bad("Log cursor exceeds available output."));
    }
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset - 1)).await?;
        let mut previous = [0];
        file.read_exact(&mut previous).await?;
        if previous[0] != b'\n' {
            return Err(Error::bad("Log cursor is not a record boundary."));
        }
    }
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let reader = Reader {
        file,
        offset,
        pending: Vec::new(),
        retired: false,
        ended: false,
        finished,
    };
    let stream = futures_util::stream::try_unfold(reader, |mut reader| async move {
        let next = reader.next().await?;
        Ok::<_, std::io::Error>(next.map(|bytes| (bytes, reader)))
    });
    Ok(([("content-type", CONTENT_TYPE)], Body::from_stream(stream)).into_response())
}

impl<F, Fut> Reader<F>
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    async fn next(&mut self) -> std::io::Result<Option<Bytes>> {
        if self.ended {
            return Ok(None);
        }
        let heartbeat = tokio::time::Instant::now();
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                let event: Value =
                    serde_json::from_slice(&self.pending[..=end]).map_err(std::io::Error::other)?;
                let tail = self.pending.split_off(end + 1);
                self.pending = tail;
                let offset = self.offset - self.pending.len() as u64;
                let mut packet = serde_json::to_vec(
                    &json!({ "type": "log", "offset": offset, "event": event }),
                )?;
                packet.push(b'\n');
                return Ok(Some(Bytes::from(packet)));
            }
            if self.pending.len() > wire::MAX_MESSAGE - 256 {
                return Err(std::io::Error::other("Attempt log record exceeds limit."));
            }
            let mut chunk = [0; 65536];
            let count = self.file.read(&mut chunk).await?;
            if count > 0 {
                self.offset += count as u64;
                self.pending.extend_from_slice(&chunk[..count]);
                continue;
            }
            if self.retired {
                if !self.pending.is_empty() {
                    return Err(std::io::Error::other(
                        "Attempt log ends with an incomplete record.",
                    ));
                }
                self.ended = true;
                return Ok(Some(Bytes::from(format!(
                    "{{\"type\":\"end\",\"offset\":{}}}\n",
                    self.offset
                ))));
            }
            if (self.finished)().await {
                // Retirement follows the last append; read once more after observing it.
                self.retired = true;
                continue;
            }
            if heartbeat.elapsed() >= Duration::from_secs(1) {
                return Ok(Some(Bytes::from_static(b"{\"type\":\"heartbeat\"}\n")));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn cursors_reject_ambiguous_or_unknown_parameters() {
        assert_eq!(offset(None).unwrap(), None);
        assert_eq!(offset(Some("cursor=1&offset=42")).unwrap(), Some(42));
        for query in [
            "offset=2",
            "cursor=2",
            "cursor=1&offset=-1",
            "cursor=1&offset=2&offset=3",
            "cursor=1&other=2",
        ] {
            assert!(offset(Some(query)).is_err(), "{query}");
        }
    }

    #[tokio::test]
    async fn reconnect_reads_only_the_unacknowledged_record_and_explicit_end() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("attempt.log");
        let first = b"{\"type\":\"output\",\"stderr\":false,\"data\":\"YQ==\"}\n";
        let second = b"{\"type\":\"output\",\"stderr\":false,\"data\":\"Yg==\"}\n";
        tokio::fs::write(&path, [first.as_slice(), second.as_slice()].concat())
            .await
            .unwrap();
        assert!(response(path.clone(), 2, || async { true }).await.is_err());
        let response = response(path, first.len() as u64, || async { true })
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 10000)
            .await
            .unwrap();
        let mut reader = bytes.as_ref();
        let record = wire::read(&mut reader).await.unwrap().unwrap();
        assert_eq!(record["event"]["data"], "Yg==");
        assert_eq!(record["offset"], (first.len() + second.len()) as u64);
        assert_eq!(
            wire::read(&mut reader).await.unwrap().unwrap()["type"],
            "end"
        );
        assert!(wire::read(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn partial_records_keep_transport_alive_and_never_claim_clean_completion() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("attempt.log");
        let mut file = tokio::fs::File::create(&path).await.unwrap();
        file.write_all(b"{\"type\":").await.unwrap();
        let response = response(path.clone(), 0, || async { false }).await.unwrap();
        let mut stream = response.into_body().into_data_stream();
        let chunk = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(chunk, b"{\"type\":\"heartbeat\"}\n".as_slice());
        assert!(
            axum::body::to_bytes(
                super::response(path, 0, || async { true })
                    .await
                    .unwrap()
                    .into_body(),
                10000
            )
            .await
            .is_err()
        );
    }
}
