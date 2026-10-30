use super::{MAX_CHAT, MAX_FILE};
use crate::{
    error::{Error, Result},
    store::Db,
};
use cairn_contracts::ChatAttachment;
use rusqlite::{OptionalExtension, params};

pub(crate) const SCHEMA: &str = include_str!("schema.sql");

pub struct UploadedAttachment {
    pub metadata: ChatAttachment,
    pub storage_name: String,
}

pub(crate) enum Reservation {
    Existing(ChatAttachment),
    Acquired,
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UploadedAttachment> {
    Ok(UploadedAttachment {
        metadata: ChatAttachment {
            id: row.get(0)?,
            chat_id: row.get(1)?,
            name: row.get(2)?,
            size: row.get::<_, u32>(3)?.into(),
            media_type: row.get(4)?,
            kind: row.get(5)?,
            digest: row.get(6)?,
            created_at: row.get(7)?,
            width: row.get(8)?,
            height: row.get(9)?,
        },
        storage_name: row.get(10)?,
    })
}

const COLUMNS: &str =
    "id,chat_id,name,size,media_type,kind,digest,created_at,width,height,storage_name";

impl Db<'_> {
    pub fn uploaded_attachment(&self, chat: &str, id: &str) -> Result<Option<UploadedAttachment>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM attachments WHERE chat_id=? AND id=?"
            ))?
            .query_row(params![chat, id], row)
            .optional()?)
    }

    pub fn attachments(&self, chat: &str) -> Result<Vec<ChatAttachment>> {
        Ok(self
            .0
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM attachments WHERE chat_id=? ORDER BY created_at,id"
            ))?
            .query_map([chat], row)?
            .map(|row| row.map(|upload| upload.metadata))
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn insert_attachment(&self, a: &ChatAttachment, storage_name: &str) -> Result<()> {
        self.0.execute(
            "INSERT OR IGNORE INTO attachment_quotas(chat_id) VALUES(?)",
            [&a.chat_id],
        )?;
        self.0
            .prepare_cached("INSERT INTO attachments VALUES(?,?,?,?,?,?,?,?,?,?,?)")?
            .execute(params![
                a.id,
                a.chat_id,
                a.name,
                i64::try_from(a.size).map_err(Error::internal)?,
                a.media_type,
                a.kind,
                a.digest,
                a.created_at,
                a.width,
                a.height,
                storage_name
            ])?;
        Ok(())
    }

    pub(crate) fn reserve_attachment(
        &self,
        a: &ChatAttachment,
        token: &str,
        expires: i64,
    ) -> Result<Reservation> {
        if let Some(existing) = self.uploaded_attachment(&a.chat_id, &a.id)? {
            if existing.metadata.digest != a.digest || existing.metadata.name != a.name {
                return Err(Error::conflict(
                    "This attachment identifier has already been used.",
                ));
            }
            return Ok(Reservation::Existing(existing.metadata));
        }
        if self
            .0
            .prepare_cached("SELECT 1 FROM attachment_reservations WHERE chat_id=? AND id=?")?
            .exists(params![a.chat_id, a.id])?
        {
            return Err(Error::conflict(
                "This attachment is being uploaded. Retry with the same identifier.",
            ));
        }
        self.0.execute(
            "INSERT OR IGNORE INTO attachment_quotas(chat_id) VALUES(?)",
            [&a.chat_id],
        )?;
        let used: u32 = self.0.query_row(
            "SELECT committed_bytes+reserved_bytes FROM attachment_quotas WHERE chat_id=?",
            [&a.chat_id],
            |row| row.get(0),
        )?;
        if a.size > MAX_FILE as u64 || u64::from(used) + a.size > MAX_CHAT {
            return Err(Error::too_large(
                "This chat has reached its 200 MB attachment limit. Start a new chat.",
            ));
        }
        self.0.execute("INSERT INTO attachment_reservations(chat_id,id,token,digest,size,name,expires_at) VALUES(?,?,?,?,?,?,?)",
            params![a.chat_id,a.id,token,a.digest,i64::try_from(a.size).map_err(Error::internal)?,a.name,expires])?;
        Ok(Reservation::Acquired)
    }

    pub(crate) fn commit_attachment(&self, a: &ChatAttachment, token: &str) -> Result<()> {
        let removed = self.0.execute("DELETE FROM attachment_reservations WHERE chat_id=? AND id=? AND token=? AND expires_at>?",
            params![a.chat_id,a.id,token,crate::config::now()])?;
        if removed != 1 {
            return Err(Error::conflict(
                "The attachment upload reservation expired. Retry the upload.",
            ));
        }
        self.insert_attachment(a, &format!("{}.{}", a.id, token))
    }
}
