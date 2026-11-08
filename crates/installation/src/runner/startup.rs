//! Opt-in startup replies keep transport alive while admission retains ownership.
use crate::error::Result;
use axum::{
    body::Body,
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::{future::Future, time::Duration};

pub(crate) const CONTENT_TYPE: &str = "application/vnd.cairn.start-result+json";

pub(crate) const STORAGE_ADMISSION_BUSY: &str =
    "Native disk ownership was not released before the preparation deadline.";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reply {
    pub status: u16,
    pub error: Option<String>,
}

/// The operation belongs to the controller, including after the response reader
/// disappears. Only its completed result acknowledges acceptance or refusal.
pub(crate) fn response(operation: impl Future<Output = Result<()>> + Send + 'static) -> Response {
    let operation = tokio::spawn(operation);
    let stream = futures_util::stream::unfold(Some(operation), |operation| async move {
        let mut operation = operation?;
        tokio::select! {
            result = &mut operation => {
                let result = match result {
                    Ok(result) => result,
                    Err(error) => return Some((Err(std::io::Error::other(error)), None)),
                };
                let reply = match result {
                    Ok(()) => Reply { status: 200, error: None },
                    Err(error) => Reply { status: error.status, error: Some(error.message) },
                };
                Some((serde_json::to_vec(&reply).map(Bytes::from).map_err(std::io::Error::other), None))
            }
            () = tokio::time::sleep(Duration::from_secs(1)) => {
                Some((Ok(Bytes::from_static(b" ")), Some(operation)))
            }
        }
    });
    ([("content-type", CONTENT_TYPE)], Body::from_stream(stream)).into_response()
}
