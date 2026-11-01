//! Short-lived private transfer files. Handles belong to one instance, never name
//! host paths, and keep large API payloads outside the Luau protocol/memory limit.
use super::connections::Connection;
use crate::{
    config::{id, now},
    error::{Error, Result},
    validation::text,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex},
};

pub const MAX_BYTES: usize = 40 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const TTL: i64 = 60 * 60 * 1000;

struct File {
    file: std::fs::File,
    instance: String,
    owner: Option<String>,
    name: String,
    media_type: String,
    size: usize,
    expires: i64,
    origin: Option<Arc<super::grant_authorization::TransferOrigin>>,
    uploaded_by: Option<String>,
    // Appending scoped bytes must preserve the original raw upload's boundary.
    unscoped_bytes: bool,
}

#[cfg(all(test, target_os = "linux"))]
mod measurements {
    use super::*;
    use std::{os::fd::AsRawFd, time::Instant};

    fn cpu_ms() -> f64 {
        let mut clock = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock points to a writable timespec and the clock ID is valid.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut clock) },
            0
        );
        clock.tv_sec as f64 * 1000.0 + clock.tv_nsec as f64 / 1_000_000.0
    }

    /// Explicit opt-in: includes fsync and POSIX_FADV_DONTNEED before cold reads;
    /// measurements are local temporary-file I/O, never production network latency.
    #[test]
    #[ignore = "local storage measurement; run explicitly with --nocapture --test-threads=1"]
    fn plugin_transfer_local_measurement() {
        let owner = Connection::draft("measurement", "files", json!({}), vec![]);
        let bytes = (0..3 * 1024 * 1024)
            .map(|n| (n % 251) as u8)
            .collect::<Vec<_>>();
        let files = Files::default();
        let inline_bytes = serde_json::to_vec(&json!({ "data": URL_SAFE_NO_PAD.encode(&bytes) }))
            .unwrap()
            .len();
        let mut write = Vec::new();
        let mut cold = Vec::new();
        let mut warm = Vec::new();
        let mut reference = Vec::new();
        let cpu_start = cpu_ms();
        let mut metadata_bytes = 0;
        for _ in 0..20 {
            let started = Instant::now();
            let metadata = files
                .create(&owner, "binary.bin", "application/octet-stream", &bytes)
                .unwrap();
            write.push(started.elapsed().as_secs_f64() * 1000.0);
            metadata_bytes = serde_json::to_vec(&metadata).unwrap().len();
            let key = text(&metadata, "id");
            let file = files.get(&owner, key).unwrap();
            {
                let file = file.lock().unwrap();
                file.file.sync_all().unwrap();
                // SAFETY: a live regular file descriptor; advice does not invalidate it.
                assert_eq!(
                    unsafe {
                        libc::posix_fadvise(file.file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED)
                    },
                    0
                );
            }
            let started = Instant::now();
            assert_eq!(files.bytes(&owner, key).unwrap(), bytes);
            cold.push(started.elapsed().as_secs_f64() * 1000.0);
            let started = Instant::now();
            assert_eq!(files.bytes(&owner, key).unwrap(), bytes);
            warm.push(started.elapsed().as_secs_f64() * 1000.0);
            files
                .call(&owner, "files.delete", &json!({ "id": key }))
                .unwrap();
            let started = Instant::now();
            assert_eq!(
                URL_SAFE_NO_PAD
                    .decode(URL_SAFE_NO_PAD.encode(&bytes))
                    .unwrap(),
                bytes
            );
            reference.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let median = |mut values: Vec<f64>| {
            values.sort_by(f64::total_cmp);
            values[values.len() / 2]
        };
        eprintln!(
            "{}",
            json!({ "samples": 20, "fileBytes": bytes.len(), "inlineJsonBytes": inline_bytes,
            "handleJsonBytes": metadata_bytes, "writeMedianMs": median(write), "coldReadMedianMs": median(cold),
            "warmReadMedianMs": median(warm), "inlineBase64RoundtripMedianMs": median(reference), "processCpuMs": cpu_ms() - cpu_start,
            "coldMethod": "fsync then POSIX_FADV_DONTNEED; kernel advice is best effort", "network": false })
        );
    }
}

#[derive(Default)]
pub struct Files {
    files: Mutex<HashMap<String, Arc<Mutex<File>>>>,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn transfer_bytes_are_unlinked_private_and_bound_to_the_instance() {
        let files = Files::default();
        let owner = Connection::draft("owner", "mail", json!({}), vec![]);
        let other = Connection::draft("other", "mail", json!({}), vec![]);
        let metadata = files
            .create(&owner, "invoice.pdf", "application/pdf", b"private invoice")
            .unwrap();
        let key = text(&metadata, "id");
        let file = files.get(&owner, key).unwrap();
        let stat = file.lock().unwrap().file.metadata().unwrap();
        assert_eq!(
            stat.nlink(),
            0,
            "No named file can survive a crash or process kill"
        );
        assert_eq!(stat.permissions().mode() & 0o777, 0o600);
        assert!(files.bytes(&other, key).is_err());
        assert_eq!(files.bytes(&owner, key).unwrap(), b"private invoice");
        files.forget("owner");
        assert!(files.bytes(&owner, key).is_err());
    }
}

fn encoding(bytes: &[u8], format: &str) -> Result<Vec<u8>> {
    match format {
        "base64url" => URL_SAFE_NO_PAD
            .decode(bytes)
            .or_else(|_| URL_SAFE.decode(bytes)),
        "base64" => STANDARD.decode(bytes),
        "text" | "raw" | "" => return Ok(bytes.to_vec()),
        _ => return Err(Error::bad("Choose text, base64 or base64url encoding.")),
    }
    .map_err(|_| Error::bad("Invalid encoded file data."))
}

impl Files {
    pub(crate) fn expire(&self) {
        Self::prune(&mut self.files.lock().unwrap());
    }

    pub(crate) fn forget(&self, instance: &str) {
        self.files
            .lock()
            .unwrap()
            .retain(|_, file| file.lock().unwrap().instance != instance);
    }

    fn prune(files: &mut HashMap<String, Arc<Mutex<File>>>) {
        files.retain(|_, file| file.lock().unwrap().expires > now());
    }

    fn get(&self, connection: &Connection, id: &str) -> Result<Arc<Mutex<File>>> {
        let mut files = self.files.lock().unwrap();
        Self::prune(&mut files);
        let file = files
            .get(id)
            .ok_or_else(|| Error::not_found("The transfer file expired or does not exist."))?;
        {
            let file = file.lock().unwrap();
            if file.instance != connection.id || file.owner != connection.owner_access_id {
                return Err(Error::forbidden(
                    "This transfer file belongs to another instance.",
                ));
            }
        }
        Ok(file.clone())
    }

    pub(crate) fn bind(
        &self,
        context: &super::capabilities::Context,
        value: &Value,
        created: bool,
    ) -> Result<()> {
        let file = self.get(&context.connection, text(value, "id"))?;
        let mut file = file.lock().unwrap();
        if let Some(authority) = &context.management {
            if let Some(origin) = authority.transfer_origin() {
                file.origin = Some(origin);
            }
            if created && file.origin.is_none() {
                file.uploaded_by = authority.snapshot().map(|snapshot| snapshot.agent);
            }
        }
        if created {
            file.unscoped_bytes = file.origin.is_none();
        }
        Ok(())
    }

    pub(crate) async fn authorize(
        &self,
        s: &crate::service::Service,
        context: &super::capabilities::Context,
        id: &str,
    ) -> Result<()> {
        let file = self.get(&context.connection, id)?;
        let (origin, uploaded_by, unscoped_bytes) = {
            let file = file.lock().unwrap();
            (
                file.origin.clone(),
                file.uploaded_by.clone(),
                file.unscoped_bytes,
            )
        };
        let snapshot = context
            .management
            .as_ref()
            .and_then(super::management::Authority::snapshot);
        if let Some(origin) = origin {
            origin
                .authorize(s, &context.connection, snapshot.clone())
                .await?;
            if let Some(authority) = &context.management {
                authority.remember_transfer(origin)?;
            }
        }
        if unscoped_bytes
            && let Some(snapshot) = snapshot
            && uploaded_by.as_deref() != Some(snapshot.agent.as_str())
            && !context.connection.grants(&snapshot.agent)
        {
            return Err(Error::forbidden(
                "This transfer has no contribution authorized for this agent.",
            ));
        }
        Ok(())
    }

    pub fn create(
        &self,
        connection: &Connection,
        name: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<Value> {
        if bytes.len() > MAX_BYTES {
            return Err(Error::too_large("Transfer files are limited to 40 MB."));
        }
        if name.len() > 200
            || name.chars().any(char::is_control)
            || media_type.len() > 150
            || media_type.chars().any(char::is_control)
        {
            return Err(Error::bad("Invalid file name or media type."));
        }
        let mut files = self.files.lock().unwrap();
        Self::prune(&mut files);
        let mut total = 0;
        let mut own = 0;
        for file in files.values() {
            let file = file.lock().unwrap();
            total += file.size;
            if file.instance == connection.id {
                own += file.size;
            }
        }
        if files.len() >= 256
            || total + bytes.len() > 1024 * 1024 * 1024
            || own + bytes.len() > 200 * 1024 * 1024
        {
            return Err(Error::too_large(
                "Transfer file quota exceeded. Remove unused files.",
            ));
        }
        let key = id();
        let mut file = tempfile::NamedTempFile::new()?.into_file();
        file.write_all(bytes)?;
        let expires = now() + TTL;
        let name = if name.is_empty() { "download" } else { name };
        let media_type = if media_type.is_empty() {
            "application/octet-stream"
        } else {
            media_type
        };
        files.insert(
            key.clone(),
            Arc::new(Mutex::new(File {
                file,
                instance: connection.id.clone(),
                owner: connection.owner_access_id.clone(),
                name: name.to_owned(),
                media_type: media_type.to_owned(),
                size: bytes.len(),
                expires,
                origin: None,
                uploaded_by: None,
                unscoped_bytes: true,
            })),
        );
        Ok(
            json!({ "id": key, "name": name, "mediaType": media_type, "size": bytes.len(), "expiresAt": expires }),
        )
    }

    pub fn bytes(&self, connection: &Connection, id: &str) -> Result<Vec<u8>> {
        let file = self.get(connection, id)?;
        let mut file = file.lock().unwrap();
        file.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::with_capacity(file.size);
        file.file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn call(&self, connection: &Connection, method: &str, args: &Value) -> Result<Value> {
        if method == "files.composeMime" {
            return super::mime::compose(self, connection, args);
        }
        if method == "files.create" {
            let bytes = encoding(text(args, "data").as_bytes(), text(args, "encoding"))?;
            if bytes.len() > CHUNK {
                return Err(Error::too_large("Append file data in chunks up to 64 KB."));
            }
            return self.create(
                connection,
                text(args, "name"),
                text(args, "mediaType"),
                &bytes,
            );
        }
        let key = text(args, "id");
        let file = self.get(connection, key)?;
        if method == "files.delete" {
            self.files.lock().unwrap().remove(key);
            return Ok(json!({ "deleted": true }));
        }
        // Quotas and appends share the map lock; concurrent files cannot exceed a
        // connection's quota by each observing its former size.
        let files = self.files.lock().unwrap();
        if method == "files.append" {
            let bytes = encoding(text(args, "data").as_bytes(), text(args, "encoding"))?;
            if bytes.len() > CHUNK {
                return Err(Error::too_large("Append file data in chunks up to 64 KB."));
            }
            let mut own = 0;
            let mut total = 0;
            for entry in files.values() {
                let entry = entry.lock().unwrap();
                total += entry.size;
                if entry.instance == connection.id {
                    own += entry.size;
                }
            }
            let mut file = file.lock().unwrap();
            if args["offset"].as_u64() != Some(file.size as u64) {
                return Err(Error::bad(
                    "Append at the current file size; do not repeat a confirmed chunk.",
                ));
            }
            if file.size + bytes.len() > MAX_BYTES
                || own + bytes.len() > 200 * 1024 * 1024
                || total + bytes.len() > 1024 * 1024 * 1024
            {
                return Err(Error::too_large("Transfer file quota exceeded."));
            }
            file.file.seek(SeekFrom::End(0))?;
            if let Err(error) = file.file.write_all(&bytes) {
                // A partial local write must never become a readable attachment.
                file.expires = 0;
                return Err(error.into());
            }
            file.size += bytes.len();
            return Ok(json!({ "id": key, "size": file.size, "expiresAt": file.expires }));
        }
        let mut file = file.lock().unwrap();
        let offset = args["offset"].as_u64().unwrap_or(0);
        let limit = args["limit"].as_u64().unwrap_or(CHUNK as u64);
        if offset > file.size as u64 || limit == 0 || limit > CHUNK as u64 {
            return Err(Error::bad("Invalid file range."));
        }
        let mut bytes = vec![0; (file.size - offset as usize).min(limit as usize)];
        file.file.seek(SeekFrom::Start(offset))?;
        file.file.read_exact(&mut bytes)?;
        Ok(
            json!({ "id": key, "name": file.name, "mediaType": file.media_type, "size": file.size,
                "data": URL_SAFE_NO_PAD.encode(&bytes), "encoding": "base64url", "offset": offset,
                "nextOffset": if offset + (bytes.len() as u64) < file.size as u64 { json!(offset + bytes.len() as u64) } else { Value::Null },
            }),
        )
    }

    pub fn response(
        &self,
        connection: &Connection,
        spec: &Value,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<Value> {
        let (data, format) = if let Some(pointer) = spec["jsonPointer"].as_str() {
            let value: Value = serde_json::from_slice(bytes)
                .map_err(|_| Error::bad_gateway("The service returned invalid JSON."))?;
            let data = value
                .pointer(pointer)
                .and_then(Value::as_str)
                .ok_or_else(|| Error::bad_gateway("The response has no requested file data."))?;
            (
                encoding(data.as_bytes(), text(spec, "encoding"))?,
                text(spec, "mediaType"),
            )
        } else {
            (bytes.to_vec(), media_type)
        };
        self.create(connection, text(spec, "name"), format, &data)
    }
}
