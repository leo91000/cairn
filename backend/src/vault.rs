use crate::{
    error::{Error, Result},
    store::{Db, Store},
};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::RngCore;
use serde_json::Value;
use std::{io::Write, os::unix::fs::OpenOptionsExt, path::Path, sync::Arc};

#[derive(Clone)]
pub struct Vault {
    key: Arc<[u8; 32]>,
    store: Store,
}

impl Vault {
    pub fn new(store: Store, directory: &Path) -> Result<Self> {
        let file = directory.join("mcp-encryption-key");
        let mut key = [0; 32];
        rand::rng().fill_bytes(&mut key);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
        {
            Ok(mut file) => file.write_all(&key)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        };
        let bytes = std::fs::read(&file)?;
        let key = bytes
            .try_into()
            .map_err(|_| Error::internal("Invalid application encryption key."))?;
        Ok(Self {
            key: Arc::new(key),
            store,
        })
    }

    pub fn encrypt(&self, id: &str, value: &Value) -> Result<Value> {
        Ok(STANDARD
            .encode(self.encrypt_bytes(id, &serde_json::to_vec(value)?)?)
            .into())
    }

    /// Authenticated binary record: nonce, tag, ciphertext. The id binds its owner/purpose.
    pub fn encrypt_bytes(&self, id: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut iv = [0; 12];
        rand::rng().fill_bytes(&mut iv);
        let cipher = Aes256Gcm::new_from_slice(self.key.as_ref()).unwrap();
        let mut encrypted = cipher
            .encrypt(
                Nonce::from_slice(&iv),
                Payload {
                    msg: bytes,
                    aad: id.as_bytes(),
                },
            )
            .map_err(|_| Error::internal("Encryption failed"))?;
        let tag = encrypted.split_off(encrypted.len() - 16);
        let mut result = iv.to_vec();
        result.extend(tag);
        result.extend(encrypted);
        Ok(result)
    }

    pub fn decrypt(&self, id: &str, value: &Value) -> Result<Value> {
        let bytes = STANDARD
            .decode(value.as_str().unwrap_or(""))
            .map_err(|_| Error::internal("Invalid encrypted record"))?;
        Ok(serde_json::from_slice(&self.decrypt_bytes(id, &bytes)?)?)
    }

    pub fn decrypt_bytes(&self, id: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        if bytes.len() < 28 {
            return Err(Error::internal("Invalid encrypted record"));
        }
        let mut ciphertext = bytes[28..].to_vec();
        ciphertext.extend(&bytes[12..28]);
        let cipher = Aes256Gcm::new_from_slice(self.key.as_ref()).unwrap();
        let plain = cipher
            .decrypt(
                Nonce::from_slice(&bytes[..12]),
                Payload {
                    msg: &ciphertext,
                    aad: id.as_bytes(),
                },
            )
            .map_err(|_| Error::internal("Unable to decrypt credential"))?;
        Ok(plain)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Value>> {
        self.store
            .kv(&format!("mcp-secret:{id}"))
            .await?
            .map(|v| self.decrypt(id, &v))
            .transpose()
    }

    pub async fn set(&self, id: &str, value: &Value) -> Result<()> {
        self.store
            .set(&format!("mcp-secret:{id}"), self.encrypt(id, value)?, None)
            .await
    }

    pub fn set_in(&self, db: &Db<'_>, id: &str, value: &Value) -> Result<()> {
        db.set(&format!("mcp-secret:{id}"), &self.encrypt(id, value)?, None)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        self.store.delete(&format!("mcp-secret:{id}")).await
    }
}
