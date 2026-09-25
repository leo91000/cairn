//! Bounded, versioned control messages. Guest messages never select host paths.
use crate::error::{Error, Result};
use serde_json::Value;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};

pub const PORT: u32 = 5200;
pub const MAX_MESSAGE: usize = 2_000_000;
pub const MAX_CHUNK: usize = 65536;

/// Archive data stays binary on its dedicated import connection. A zero length
/// terminates the archive; EOF before that marker is an interrupted transfer.
pub async fn write_chunk(writer: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_CHUNK {
        return Err(Error::bad("VM archive chunk exceeds limit."));
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_chunk(reader: &mut (impl AsyncRead + Unpin), buffer: &mut [u8]) -> Result<usize> {
    let count = reader.read_u32().await? as usize;
    if count > MAX_CHUNK || count > buffer.len() {
        return Err(Error::bad("VM archive chunk exceeds limit."));
    }
    reader.read_exact(&mut buffer[..count]).await?;
    Ok(count)
}

pub async fn read(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Value>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(Error::bad("Truncated VM message."))
            };
        }
        let end = chunk.iter().position(|b| *b == b'\n');
        let count = end.map_or(chunk.len(), |n| n + 1);
        if bytes.len() + count > MAX_MESSAGE {
            return Err(Error::bad("VM message exceeds limit."));
        }
        bytes.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(serde_json::from_slice(&bytes)?));
        }
    }
}

pub async fn write(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() >= MAX_MESSAGE {
        return Err(Error::bad("VM message exceeds limit."));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archive_frames_preserve_bytes_and_reject_truncated_or_oversized_data() {
        let data = (0..MAX_CHUNK).map(|i| i as u8).collect::<Vec<_>>();
        let mut encoded = Vec::new();
        write_chunk(&mut encoded, &data).await.unwrap();
        write_chunk(&mut encoded, b"\0\n{}").await.unwrap();
        write_chunk(&mut encoded, &[]).await.unwrap();
        let mut reader = encoded.as_slice();
        let mut buffer = vec![0; MAX_CHUNK];
        assert_eq!(
            read_chunk(&mut reader, &mut buffer).await.unwrap(),
            MAX_CHUNK
        );
        assert_eq!(buffer, data);
        assert_eq!(read_chunk(&mut reader, &mut buffer).await.unwrap(), 4);
        assert_eq!(&buffer[..4], b"\0\n{}");
        assert_eq!(read_chunk(&mut reader, &mut buffer).await.unwrap(), 0);
        assert!(read_chunk(&mut reader, &mut buffer).await.is_err());
        for bytes in [
            &encoded[..2],
            &encoded[..MAX_CHUNK],
            &(MAX_CHUNK as u32 + 1).to_be_bytes(),
        ] {
            assert!(read_chunk(&mut &bytes[..], &mut buffer).await.is_err());
        }
        assert!(
            write_chunk(&mut Vec::new(), &vec![0; MAX_CHUNK + 1])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn fragmented_archive_frames_do_not_consume_the_following_control_message() {
        let mut encoded = Vec::new();
        write_chunk(&mut encoded, b"archive\0\n{}").await.unwrap();
        write_chunk(&mut encoded, &[]).await.unwrap();
        write(
            &mut encoded,
            &serde_json::json!({
                "ok": true
            }),
        )
        .await
        .unwrap();
        let (mut sender, receiver) = tokio::io::duplex(5);
        let writing = async move {
            for chunk in encoded.chunks(3) {
                sender.write_all(chunk).await.unwrap();
            }
        };
        let reading = async move {
            let mut reader = tokio::io::BufReader::new(receiver);
            let mut buffer = vec![0; MAX_CHUNK];
            let count = read_chunk(&mut reader, &mut buffer).await.unwrap();
            assert_eq!(&buffer[..count], b"archive\0\n{}");
            assert_eq!(read_chunk(&mut reader, &mut buffer).await.unwrap(), 0);
            assert_eq!(read(&mut reader).await.unwrap().unwrap()["ok"], true);
        };
        tokio::join!(writing, reading);
    }

    #[tokio::test]
    async fn rejects_truncation_and_bounds_frames() {
        assert!(read(&mut &b"{\"x\":1}"[..]).await.is_err());
        assert!(
            read(&mut vec![b'x'; MAX_MESSAGE + 1].as_slice())
                .await
                .is_err()
        );
        let mut source = &b"{\"v\":1}\n{\"v\":2}\n"[..];
        assert_eq!(read(&mut source).await.unwrap().unwrap()["v"], 1);
        assert_eq!(read(&mut source).await.unwrap().unwrap()["v"], 2);
        assert!(read(&mut source).await.unwrap().is_none());
    }
}
