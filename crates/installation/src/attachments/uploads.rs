//! Owns upload I/O and cancellation. SQL tokens fence expired writers; distinct
//! conversations and files never hold a common lock over their I/O.
use super::{sql::Reservation, *};
use crate::store::Store;
use cairn_contracts::ChatAttachment;
use rusqlite::OptionalExtension;
use std::sync::{Arc, Mutex, Weak};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use tokio_util::sync::CancellationToken;

const LEASE_MS: i64 = 5 * 60 * 1000;

#[derive(Default)]
struct ConversationUploads {
    lifecycle: RwLock<()>,
    files: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

#[derive(Default)]
pub struct Attachments {
    conversations: Mutex<HashMap<String, Weak<ConversationUploads>>>,
    #[cfg(test)]
    preparation: Mutex<HashMap<String, Arc<PreparationGate>>>,
    #[cfg(test)]
    writing: Mutex<HashMap<String, Arc<PreparationGate>>>,
}

impl Attachments {
    fn conversation(&self, chat: &str) -> Arc<ConversationUploads> {
        let mut chats = self.conversations.lock().unwrap();
        if let Some(chat) = chats.get(chat).and_then(Weak::upgrade) {
            return chat;
        }
        chats.retain(|_, chat| chat.strong_count() > 0);
        let state = Arc::new(ConversationUploads::default());
        chats.insert(chat.into(), Arc::downgrade(&state));
        state
    }

    pub(crate) async fn upload<F>(
        self: &Arc<Self>,
        store: Store,
        root: PathBuf,
        attachment: ChatAttachment,
        bytes: Vec<u8>,
        authorize: F,
    ) -> Result<ChatAttachment>
    where
        F: Fn(&Db<'_>) -> Result<()> + Send + Sync + 'static,
    {
        // Preserve the caller's preparation cancellation explicitly across spawn.
        // Cancelling this upload never cancels its parent preparation or siblings.
        let cancelled = crate::plugins::operations::publication_cancellation()
            .map_or_else(CancellationToken::new, |parent| parent.child_token());
        let _cancel_on_drop = cancelled.clone().drop_guard();
        let domain = self.clone();
        // This task retains ownership until asynchronous filesystem work settles,
        // even when its HTTP/tool caller disappears. No detached write can race
        // cleanup or publish after cancellation has been observed by the writer.
        tokio::spawn(async move {
            let state = domain.conversation(&attachment.chat_id);
            let _lifecycle = state.lifecycle.read().await;
            let file = {
                let mut files = state.files.lock().unwrap();
                files.retain(|_, file| file.strong_count() > 0);
                let file = files
                    .get(&attachment.id)
                    .and_then(Weak::upgrade)
                    .unwrap_or_else(|| Arc::new(AsyncMutex::new(())));
                files.insert(attachment.id.clone(), Arc::downgrade(&file));
                file
            };
            let _file = file.lock().await;
            let token = crate::config::id();
            let a = attachment.clone();
            let claim = token.clone();
            let cancelled_check = cancelled.clone();
            let authorize = Arc::new(authorize);
            let check = authorize.clone();
            let reservation = store
                .transaction(move |db| {
                    ensure_live(db, &a.chat_id, &cancelled_check, &*check)?;
                    db.reserve_attachment(&a, &claim, now() + LEASE_MS)
                })
                .await?;
            if let Reservation::Existing(existing) = reservation {
                return Ok(existing);
            }
            let directory = root.join(&attachment.chat_id);
            let path = directory.join(format!("{}.{}", attachment.id, token));
            let publication = async {
                #[cfg(test)]
                wait_at(&domain.preparation, &attachment.id, &cancelled).await?;
                private_dir(&directory).await?;
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .await?;
                for chunk in bytes.chunks(64 * 1024) {
                    if cancelled.is_cancelled() {
                        return Err(Error::conflict("Attachment upload cancelled."));
                    }
                    file.write_all(chunk).await?;
                    #[cfg(test)]
                    wait_at(&domain.writing, &attachment.id, &cancelled).await?;
                }
                file.sync_all().await?;
                drop(file);
                tokio::fs::File::open(&directory).await?.sync_all().await?;
                tokio::fs::File::open(&root).await?.sync_all().await?;
                let a = attachment.clone();
                let claim = token.clone();
                store
                    .transaction(move |db| {
                        ensure_live(db, &a.chat_id, &cancelled, &*authorize)?;
                        db.commit_attachment(&a, &claim)
                    })
                    .await
            }
            .await;
            if let Err(error) = publication {
                if let Err(cleanup) = tokio::fs::remove_file(&path).await
                    && cleanup.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(error = %cleanup, "could not remove an uncommitted attachment");
                }
                store
                    .transaction(move |db| {
                        db.0.execute("DELETE FROM attachment_reservations WHERE token=?", [token])?;
                        Ok(())
                    })
                    .await?;
                return Err(error);
            }
            Ok(attachment)
        })
        .await
        .map_err(Error::internal)?
    }

    pub(crate) async fn purge(&self, store: &Store, root: &Path, chat: &str) -> Result<()> {
        let state = self.conversation(chat);
        let _lifecycle = state.lifecycle.write().await;
        let id = chat.to_owned();
        store
            .transaction(move |db| {
                db.0.execute("DELETE FROM attachment_reservations WHERE chat_id=?", [&id])?;
                db.0.execute("DELETE FROM attachments WHERE chat_id=?", [&id])?;
                db.0.execute("DELETE FROM attachment_quotas WHERE chat_id=?", [&id])?;
                Ok(())
            })
            .await?;
        match tokio::fs::remove_dir_all(root.join(chat)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn ensure_live<F>(
    db: &Db<'_>,
    chat: &str,
    cancelled: &CancellationToken,
    authorize: &F,
) -> Result<()>
where
    F: Fn(&Db<'_>) -> Result<()> + ?Sized,
{
    if cancelled.is_cancelled() {
        return Err(Error::conflict("Attachment upload cancelled."));
    }
    authorize(db)?;
    let lifecycle = required(
        db.0.prepare_cached("SELECT lifecycle FROM conversations WHERE id=?")?
            .query_row([chat], |row| row.get::<_, String>(0))
            .optional()?,
        "Chat not found",
    )?;
    if lifecycle != "active" {
        return Err(Error::conflict("This conversation has been deleted."));
    }
    Ok(())
}

/// Expiry releases crash leftovers. Enumerate disk entries before checking each
/// unique filename against SQL, so a newly reserved writer is never collected
/// from a stale snapshot. Migrated stable filenames are never speculative files.
pub(crate) async fn reconcile(store: &Store, root: &Path) -> Result<()> {
    private_dir(root).await?;
    if let Some(parent) = root.parent() {
        tokio::fs::File::open(parent).await?.sync_all().await?;
    }
    expire(store, root).await?;
    let mut chats = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    while let Some(chat) = chats.next_entry().await? {
        if !chat.file_type().await?.is_dir() {
            continue;
        }
        let chat_id = chat.file_name().to_string_lossy().into_owned();
        if uuid(&chat_id).is_err() {
            continue;
        }
        let mut files = match tokio::fs::read_dir(chat.path()).await {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        while let Some(file) = files.next_entry().await? {
            let name = file.file_name().to_string_lossy().into_owned();
            let Some((id, token)) = name.split_once('.') else {
                continue;
            };
            if uuid(id).is_err() || uuid(token).is_err() {
                continue;
            }
            let check_chat = chat_id.clone();
            let check_name = name.clone();
            let check_id = id.to_owned();
            let check_token = token.to_owned();
            let retained = store.read(move |db| Ok(db.0.query_row(
                "SELECT EXISTS(SELECT 1 FROM attachments WHERE chat_id=? AND id=? AND storage_name=?) OR EXISTS(SELECT 1 FROM attachment_reservations WHERE token=?)",
                rusqlite::params![check_chat,check_id,check_name,check_token], |row| row.get::<_, bool>(0))?)).await?;
            if !retained {
                match tokio::fs::remove_file(file.path()).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(())
}

pub(crate) async fn expire(store: &Store, root: &Path) -> Result<()> {
    let expired = store.transaction(|db| {
        let expired = db.0.prepare("DELETE FROM attachment_reservations WHERE expires_at<=? RETURNING chat_id,id,token")?
            .query_map([now()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(expired)
    }).await?;
    for (chat, id, token) in expired {
        match tokio::fs::remove_file(root.join(chat).join(format!("{id}.{token}"))).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
async fn wait_at(
    gates: &Mutex<HashMap<String, Arc<PreparationGate>>>,
    id: &str,
    cancelled: &CancellationToken,
) -> Result<()> {
    let gate = gates.lock().unwrap().remove(id);
    if let Some(gate) = gate {
        gate.reached.notify_one();
        tokio::select! {
            _ = gate.resume.notified() => {},
            _ = cancelled.cancelled() => return Err(Error::conflict("Attachment upload cancelled.")),
        }
    }
    Ok(())
}

#[cfg(test)]
#[derive(Default)]
struct PreparationGate {
    reached: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

#[cfg(test)]
mod tests;
