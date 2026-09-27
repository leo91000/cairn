//! Cancellable block transfers; a transient outage never becomes a zero-filled read.
use super::BlockSource;
use serde_json::{Value, json};
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub struct RemoteSource {
    origin: url::Url,
    credential: String,
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
    stop: CancellationToken,
    waiting: AtomicUsize,
}
struct Waiting<'a>(&'a AtomicUsize);
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl RemoteSource {
    pub fn new(
        context: &Value,
        runtime: tokio::runtime::Handle,
        stop: CancellationToken,
    ) -> crate::error::Result<Self> {
        let credential = context["grant"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 1024)
            .ok_or_else(|| crate::error::Error::bad("Missing disk read authorization."))?
            .to_owned();
        Ok(Self {
            origin: crate::nodes::connector::master(crate::validation::text(context, "master"))?,
            credential,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(crate::error::Error::internal)?,
            runtime,
            stop,
            waiting: AtomicUsize::new(0),
        })
    }
    pub fn grant_id(&self) -> String {
        crate::auth::digest(&self.credential)
    }
    pub fn waiting(&self) -> bool {
        self.waiting.load(Ordering::SeqCst) > 0
    }
    fn interrupted() -> io::Error {
        io::Error::new(io::ErrorKind::Interrupted, "Disk read cancelled")
    }
    async fn renew(&self) -> io::Result<()> {
        let request = self
            .client
            .post(
                self.origin
                    .join("internal/node-restore/renew")
                    .map_err(io::Error::other)?,
            )
            .bearer_auth(&self.credential)
            .json(&json!({}))
            .send();
        let response = tokio::select! { _=self.stop.cancelled()=>return Err(Self::interrupted()), result=request=>result.map_err(|_| io::Error::new(io::ErrorKind::WouldBlock,"Storage authorization temporarily unavailable"))? };
        if response.status().is_success() {
            Ok(())
        } else if matches!(response.status().as_u16(), 401 | 403 | 404) {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Disk read authorization revoked",
            ))
        } else {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Storage authorization temporarily unavailable",
            ))
        }
    }
}
impl BlockSource for RemoteSource {
    fn fetch(&self, hash: &str) -> io::Result<Vec<u8>> {
        if !crate::nodes::snapshots::valid_hash(hash) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid block digest",
            ));
        }
        self.runtime.block_on(async {
            let mut waiting = None;
            let mut delay = Duration::from_millis(100);
            loop {
                let request = self.client.get(self.origin.join(&format!("internal/node-restore/{hash}")).map_err(io::Error::other)?).bearer_auth(&self.credential).send();
                let response = tokio::select! { _=self.stop.cancelled()=>return Err(Self::interrupted()), result=request=>result };
                if let Ok(response) = response {
                    let status = response.status();
                    if status.is_success() {
                        let transfer = crate::nodes::snapshots::response_block(response);
                        let result = tokio::select! { _=self.stop.cancelled()=>return Err(Self::interrupted()), result=transfer=>result };
                        match result {
                            Ok(bytes)=>return Ok(bytes),
                            Err(error) if error.status == 400 =>return Err(io::Error::new(io::ErrorKind::InvalidData,"Invalid remote disk block")),
                            Err(_)=>{}
                        }
                    } else if status.as_u16()==401 {
                        match self.renew().await { Ok(())=>{}, Err(error) if error.kind()==io::ErrorKind::WouldBlock=>{}, Err(error)=>return Err(error) }
                    } else if matches!(status.as_u16(),403|404) {
                        return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Disk read authorization revoked"));
                    } else if !status.is_server_error() && !matches!(status.as_u16(),408|429) {
                        return Err(io::Error::new(io::ErrorKind::InvalidData,"Remote disk block rejected"));
                    }
                }
                if waiting.is_none() { self.waiting.fetch_add(1,Ordering::SeqCst); waiting=Some(Waiting(&self.waiting)); }
                tokio::select! { _=self.stop.cancelled()=>return Err(Self::interrupted()), _=tokio::time::sleep(delay)=>{} }
                delay=(delay*2).min(Duration::from_secs(5));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};
    #[tokio::test]
    async fn outage_waits_then_recovers_and_cancellation_interrupts_a_waiting_read() {
        use axum::{Router, http::StatusCode, routing::get};
        let offline = Arc::new(AtomicBool::new(true));
        let flag = offline.clone();
        let server = Router::new().route(
            "/internal/node-restore/{hash}",
            get(move || {
                let flag = flag.clone();
                async move {
                    if flag.load(Ordering::SeqCst) {
                        (StatusCode::SERVICE_UNAVAILABLE, b"".as_slice())
                    } else {
                        (StatusCode::OK, b"verified later by the disk".as_slice())
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let serving = tokio::spawn(async { axum::serve(listener, server).await.unwrap() });
        let stop = CancellationToken::new();
        let source = Arc::new(
            RemoteSource::new(
                &json!({"master":origin,"grant":"fixture-scoped-token"}),
                tokio::runtime::Handle::current(),
                stop.clone(),
            )
            .unwrap(),
        );
        let read = {
            let source = source.clone();
            tokio::task::spawn_blocking(move || source.fetch(&"a".repeat(64)))
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !source.waiting() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(!read.is_finished());
        offline.store(false, Ordering::SeqCst);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), read)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            b"verified later by the disk"
        );
        assert!(!source.waiting());
        offline.store(true, Ordering::SeqCst);
        let read = {
            let source = source.clone();
            tokio::task::spawn_blocking(move || source.fetch(&"a".repeat(64)))
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !source.waiting() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        stop.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), read)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(!source.waiting());
        serving.abort();
    }
}
