//! A framed request multiplexer on the environment's inherited private socket.
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::{
    io::BufReader,
    net::UnixStream,
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

use super::protocol::{Event, Request};
use crate::{
    error::{Error, Result},
    microvm::wire,
};

struct Pending {
    replies: Mutex<HashMap<String, Option<oneshot::Sender<Event>>>>,
    invalid_frame: AtomicBool,
}

struct RequestGuard {
    pending: Arc<Pending>,
    id: String,
    stop: CancellationToken,
    completed: bool,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.pending.replies.lock().unwrap().remove(&self.id);
        if !self.completed {
            self.stop.cancel();
        }
    }
}

/// Controller-owned endpoint. A disconnected controller stops its environment;
/// provider detach never drops this channel.
#[derive(Clone)]
pub struct Channel {
    output: mpsc::Sender<Request>,
    pending: Arc<Pending>,
    stop: CancellationToken,
}

impl Channel {
    pub fn attach(stream: UnixStream) -> (Self, mpsc::Receiver<Event>) {
        let (read, mut write) = stream.into_split();
        let (output, mut requests) = mpsc::channel(32);
        let (events, received) = mpsc::channel(32);
        let pending = Arc::new(Pending {
            replies: Mutex::new(HashMap::new()),
            invalid_frame: AtomicBool::new(false),
        });
        let stop = CancellationToken::new();
        let writing_stop = stop.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = writing_stop.cancelled() => break,
                    request = requests.recv() => {
                        let Some(request) = request else { break; };
                        let result = tokio::select! {
                            () = writing_stop.cancelled() => break,
                            result = wire::write(&mut write, &request) => result,
                        };
                        if result.is_err() { break; }
                    }
                }
            }
            writing_stop.cancel();
        });
        let reading_stop = stop.clone();
        let reading_pending = pending.clone();
        tokio::spawn(async move {
            let mut read = BufReader::new(read);
            loop {
                let value = tokio::select! {
                    () = reading_stop.cancelled() => break,
                    value = wire::read(&mut read) => value,
                };
                let event = match value {
                    Ok(Some(value)) => serde_json::from_value::<Event>(value).ok(),
                    Ok(None) => break,
                    Err(error) if error.is_unavailable() => break,
                    Err(_) => None,
                };
                let Some(event) = event else {
                    reading_pending.invalid_frame.store(true, Ordering::Release);
                    break;
                };
                let request = match &event {
                    Event::JobStarted { request_id, .. }
                    | Event::JobStatus { request_id, .. }
                    | Event::JobLookup { request_id, .. }
                    | Event::Activity { request_id, .. }
                    | Event::Accepted { request_id }
                    | Event::Refused { request_id, .. } => Some(request_id),
                    _ => None,
                };
                if let Some(request) = request {
                    let reply = reading_pending
                        .replies
                        .lock()
                        .unwrap()
                        .get_mut(request)
                        .and_then(Option::take);
                    if let Some(reply) = reply {
                        let _ = reply.send(event);
                    }
                    continue;
                }
                let delivered = tokio::select! {
                    () = reading_stop.cancelled() => break,
                    result = events.send(event) => result,
                };
                if delivered.is_err() {
                    break;
                }
            }
            reading_stop.cancel();
            reading_pending.replies.lock().unwrap().clear();
        });
        (
            Self {
                output,
                pending,
                stop,
            },
            received,
        )
    }

    pub(crate) fn invalid_frame(&self) -> bool {
        self.pending.invalid_frame.load(Ordering::Acquire)
    }

    pub(crate) fn disconnect_error(&self) -> Error {
        if self.invalid_frame() {
            return super::protocol::invalid_control_frame();
        }
        Error::unavailable("Execution environment disconnected.")
    }

    pub async fn request(&self, request: Request) -> Result<Event> {
        let id = match &request {
            Request::StartJob { request_id, .. }
            | Request::JobStatus { request_id, .. }
            | Request::LookupJob { request_id, .. }
            | Request::ResumeTurn { request_id, .. }
            | Request::Close { request_id }
            | Request::Activity { request_id } => request_id.clone(),
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut replies = self.pending.replies.lock().unwrap();
            if self.stop.is_cancelled() {
                return Err(self.disconnect_error());
            }
            if replies.len() >= 32 {
                return Err(Error::too_many_requests(
                    "Execution environment control is busy.",
                ));
            }
            if replies.contains_key(&id) {
                return Err(Error::conflict(
                    "This control request is already in flight.",
                ));
            }
            replies.insert(id.clone(), Some(sender));
        }
        let mut guard = RequestGuard {
            pending: self.pending.clone(),
            id: id.clone(),
            stop: self.stop.clone(),
            completed: false,
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            self.output
                .send(request)
                .await
                .map_err(|_| self.disconnect_error())?;
            receiver.await.map_err(|_| self.disconnect_error())
        })
        .await
        .map_err(|_| Error::timeout("Execution environment request timed out."));
        guard.completed = true;
        if result.is_err() {
            // The response boundary is ambiguous. Do not reuse request IDs on
            // this stream: a delayed response could belong to the timed-out call.
            self.stop.cancel();
        }
        result?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn pending_request_distinguishes_invalid_frames_from_transport_loss() {
        for (reply, status) in [
            (b"invalid\n".as_slice(), 502),
            (b"{}\n".as_slice(), 502),
            (b"{".as_slice(), 503),
        ] {
            let (host, guest) = UnixStream::pair().unwrap();
            let (channel, _events) = Channel::attach(host);
            let request = channel.request(Request::Activity {
                request_id: "activity".into(),
            });
            let respond = async move {
                let mut guest = BufReader::new(guest);
                assert!(wire::read(&mut guest).await.unwrap().is_some());
                guest.get_mut().write_all(reply).await.unwrap();
            };
            let (reply, ()) = tokio::join!(request, respond);
            assert_eq!(reply.unwrap_err().status, status);
        }
    }

    #[test]
    fn replying_keeps_request_id_reserved_until_owner_finishes() {
        let pending = Arc::new(Pending {
            replies: Mutex::new(HashMap::new()),
            invalid_frame: AtomicBool::new(false),
        });
        let (sender, _receiver) = oneshot::channel();
        pending
            .replies
            .lock()
            .unwrap()
            .insert("call".into(), Some(sender));
        let guard = RequestGuard {
            pending: pending.clone(),
            id: "call".into(),
            stop: CancellationToken::new(),
            completed: true,
        };
        let sender = pending
            .replies
            .lock()
            .unwrap()
            .get_mut("call")
            .unwrap()
            .take();
        assert!(sender.is_some());
        assert!(pending.replies.lock().unwrap().contains_key("call"));
        drop(guard);
        assert!(!pending.replies.lock().unwrap().contains_key("call"));
    }
}
