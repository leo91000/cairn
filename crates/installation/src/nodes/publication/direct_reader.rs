//! A drained control response owns the node reader until all PUT batches settle.
use super::*;

pub(super) struct Lease {
    task: tokio::task::JoinHandle<Result<()>>,
    url: String,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Lease {
    pub(super) async fn open(publication: &Publication<'_>) -> Result<Self> {
        let url = format!(
            "{}/snapshots/{}/direct-reader",
            publication.base, publication.snapshot_id
        );
        let headers = diagnostic::start(Stage::NodeHeaders);
        let response = tokio::time::timeout(
            snapshots::DIRECT_PUBLICATION_STARTUP,
            publication
                .s
                .disk_http
                .post(&url)
                .bearer_auth(publication.credential)
                .json(&json!({ "diagnostics": diagnostic::request(false) }))
                .send(),
        )
        .await
        .map_err(|_| Error::unavailable("Publication reader startup timed out."))?
        .map_err(|_| Error::unavailable("Publication reader unavailable."))?;
        headers.finish(0);
        if !response.status().is_success() {
            return Err(Error::unavailable(
                "Publication reader unavailable; local journal retained.",
            ));
        }
        let task = tokio::spawn(async move {
            let mut response = response;
            loop {
                let chunk = tokio::time::timeout(Duration::from_secs(60), response.chunk())
                    .await
                    .map_err(|_| Error::unavailable("Publication reader stopped responding."))?
                    .map_err(|_| Error::unavailable("Publication reader disconnected."))?;
                let Some(chunk) = chunk else { return Ok(()) };
                if chunk.len() > 16_384 || chunk.iter().any(|byte| *byte != b' ') {
                    return Err(Error::bad("Invalid publication reader response."));
                }
            }
        });
        Ok(Self { task, url })
    }

    pub(super) fn check(&self) -> Result<()> {
        if self.task.is_finished() {
            return Err(Error::unavailable(
                "Publication reader disconnected; local journal retained.",
            ));
        }
        Ok(())
    }

    pub(super) async fn close(&mut self, publication: &Publication<'_>) -> Result<()> {
        self.check()?;
        release(&publication.s.http, &self.url, publication.credential).await?;
        // The explicit controller acknowledgement proves reader/physical-owner
        // release. Cancel its metadata subscription; a relayed final heartbeat
        // frame is not another disk-publication acknowledgement.
        self.task.abort();
        Ok(())
    }
}

async fn release(client: &reqwest::Client, url: &str, credential: &str) -> Result<()> {
    let mut response = client
        .delete(url)
        .bearer_auth(credential)
        .send()
        .await
        .map_err(|_| Error::unavailable("Publication reader close interrupted."))?;
    if !response.status().is_success() {
        return Err(Error::unavailable(
            "Publication reader is still finishing transfers.",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error::unavailable("Publication reader close interrupted."))?
    {
        if chunk.len() > 4096usize.saturating_sub(bytes.len()) {
            return Err(Error::bad("Invalid publication reader acknowledgement."));
        }
        bytes.extend_from_slice(&chunk);
    }
    let receipt: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::unavailable("Publication reader close was not acknowledged."))?;
    if receipt["closed"] != true {
        return Err(Error::unavailable(
            "Publication reader close was not acknowledged.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn close_requires_a_bounded_explicit_release_acknowledgement() {
        let app = axum::Router::new().fallback(|request: axum::extract::Request| async move {
            assert_eq!(request.method(), "DELETE");
            assert_eq!(request.headers()["authorization"], "Bearer fixture");
            match request.uri().path() {
                "/released" => axum::Json(json!({ "closed": true })).into_response(),
                "/generic" => axum::Json(json!({ "ok": true })).into_response(),
                "/busy" => axum::Json(json!({ "closed": false })).into_response(),
                "/oversized" => " ".repeat(4097).into_response(),
                _ => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        for path in ["generic", "busy", "oversized", "unavailable"] {
            assert!(
                release(&client, &format!("{origin}/{path}"), "fixture")
                    .await
                    .is_err()
            );
        }
        release(&client, &format!("{origin}/released"), "fixture")
            .await
            .unwrap();
        server.abort();
    }
}
