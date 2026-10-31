//! A prepared capability batch owns its control request through receipt validation
//! and durable settlement. Deliberately no Debug: plans contain scoped secrets.
use super::*;
use crate::{
    storage::direct::{BATCH, Batch, Receipt, TRANSFER_SECONDS},
    store::Store,
};

pub(super) struct Prepared {
    request: reqwest::RequestBuilder,
    store: Store,
    publication_id: String,
    objects: Vec<String>,
    plaintext_bytes: u64,
}

impl Prepared {
    pub(super) async fn new(
        publication: &Publication<'_>,
        objects: &[&shared_blocks::Object],
        final_batch: bool,
    ) -> Result<Self> {
        if objects.is_empty() || objects.len() > BATCH {
            return Err(Error::bad("Invalid direct publication batch."));
        }
        if publication.snapshot["manifest"]["onDemand"] == true {
            // Preserve the external policy check and revalidate its authoritative
            // run/node/grant view in the durable capability transaction below.
            super::super::disk_grants::authorize_id(
                publication.s,
                text(publication.snapshot, "grantId"),
            )
            .await?
            .ok_or_else(|| Error::forbidden("Disk publication authorization was revoked."))?;
        }
        let mut plans = Vec::new();
        for object in objects {
            plans.push(
                publication
                    .storage
                    .direct_upload(&publication.s.vault, &object.hash, &object.id, object.size)
                    .await?,
            );
        }
        let ids = objects
            .iter()
            .map(|object| object.id.clone())
            .collect::<Vec<_>>();
        let until = plans.iter().map(|plan| plan.read.expires_at).max().unwrap()
            + TRANSFER_SECONDS as i64 * 1000;
        let retained = ids.clone();
        let owner = publication.owner();
        publication.s.store.transaction(move |db| {
            owner.validate(db)?;
            for object in retained {
                db.0.execute("INSERT INTO direct_storage_grants(object,expires_at,settled) VALUES(?1,?2,0) ON CONFLICT(object) DO UPDATE SET expires_at=MAX(expires_at,excluded.expires_at),settled=0", rusqlite::params![object, until])?;
            }
            Ok(())
        }).await?;
        let publication_id = publication.snapshot_id.to_owned();
        let request = publication
            .s
            .http
            .post(format!(
                "{}/snapshots/{}/direct-upload",
                publication.base, publication.snapshot_id,
            ))
            .bearer_auth(publication.credential)
            .json(&Batch {
                require_reader: true,
                publication_id: publication_id.clone(),
                objects: plans,
                diagnostics: diagnostic::request(final_batch),
            })
            .timeout(Duration::from_secs(TRANSFER_SECONDS + 60));
        Ok(Self {
            request,
            store: publication.s.store.clone(),
            publication_id,
            objects: ids,
            plaintext_bytes: objects.iter().map(|object| object.size).sum(),
        })
    }

    pub(super) async fn upload(self) -> Result<u64> {
        let headers = diagnostic::start(Stage::NodeHeaders);
        let response = self.request.send().await.map_err(|_| {
            Error::unavailable("Direct publication interrupted; local journal retained.")
        })?;
        headers.finish(0);
        if !response.status().is_success() {
            if response.status().as_u16() == 400 {
                return Err(Error::bad(
                    "Node rejected a direct publication block's integrity.",
                ));
            }
            return Err(Error::unavailable(
                "Node direct upload failed; no manager fallback, local journal retained.",
            ));
        }
        let bytes = snapshots::response_block(response).await?;
        let mut receipt: Receipt = serde_json::from_slice(&bytes)
            .map_err(|_| Error::bad("Invalid direct publication receipt."))?;
        let mut expected = self.objects.clone();
        expected.sort();
        receipt.objects.sort();
        if receipt.publication_id != self.publication_id
            || receipt.objects != expected
            || receipt.plaintext_bytes != self.plaintext_bytes
            || receipt.encoded_bytes
                > self.objects.len() as u64 * crate::storage::block_codec::MAX_ENCODED_BYTES
        {
            return Err(Error::bad(
                "Direct publication receipt does not match its plan.",
            ));
        }
        self.store
            .write(move |db| {
                for object in self.objects {
                    db.0.execute(
                        "UPDATE direct_storage_grants SET settled=1 WHERE object=?1",
                        [object],
                    )?;
                }
                Ok(())
            })
            .await?;
        Ok(receipt.plaintext_bytes)
    }
}

pub(super) async fn complete_one(uploads: &mut JoinSet<Result<u64>>) -> Result<u64> {
    uploads
        .join_next()
        .await
        .ok_or_else(|| Error::internal("No direct publication batch to complete."))?
        .map_err(Error::internal)?
}
