//! Binary envelopes around the existing JSON frames on a reliable ordered DataChannel.
use crate::{Frame, MAX_FRAME, MAX_IN_FLIGHT, REQUEST_TIMEOUT, Role};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

pub const MAX_PACKET: usize = 16_384;
pub const HEADER: usize = 13;

/// Version byte, transfer ID, total JSON length and offset (three network-order u32s).
/// A header with total=offset=0 aborts an incomplete transfer without dispatching it.
pub struct EncodedFrame {
    id: u32,
    bytes: Vec<u8>,
}

impl EncodedFrame {
    pub fn new(id: u32, frame: &Frame) -> Result<Self, &'static str> {
        let bytes = serde_json::to_vec(frame).map_err(|_| "Invalid application frame")?;
        if id == 0 || bytes.len() > MAX_FRAME {
            return Err("Application frame too large");
        }
        Ok(Self { id, bytes })
    }

    pub fn packets(&self) -> impl Iterator<Item = Vec<u8>> + '_ {
        self.bytes
            .chunks(MAX_PACKET - HEADER)
            .enumerate()
            .map(|(index, chunk)| {
                let mut packet = Vec::with_capacity(HEADER + chunk.len());
                packet.push(1);
                packet.extend(self.id.to_be_bytes());
                packet.extend((self.bytes.len() as u32).to_be_bytes());
                packet.extend(((index * (MAX_PACKET - HEADER)) as u32).to_be_bytes());
                packet.extend(chunk);
                packet
            })
    }
}

/// Two maximum frames per installation, with one reserved for the owner.
/// Each account can reserve at most one maximum frame across all its peers.
#[derive(Clone, Default)]
pub struct ReassemblyBudget {
    state: Arc<Mutex<Reservations>>,
    account: String,
    member: bool,
}

#[derive(Default)]
struct Reservations {
    total: usize,
    members: usize,
    accounts: HashMap<String, usize>,
}

struct Reservation {
    budget: ReassemblyBudget,
    bytes: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Busy,
    Rejected(Option<String>),
    Invalid(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("Reassembly capacity busy"),
            Self::Rejected(_) => formatter.write_str("Reassembly transfer rejected"),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<&'static str> for DecodeError {
    fn from(message: &'static str) -> Self {
        Self::Invalid(message)
    }
}

impl ReassemblyBudget {
    pub fn for_account(&self, account: &str, role: Role) -> Self {
        Self {
            state: self.state.clone(),
            account: account.to_owned(),
            member: role == Role::Member,
        }
    }

    fn reserve(&self, bytes: usize) -> Result<Reservation, DecodeError> {
        let mut reserved = self.state.lock().unwrap();
        let account = reserved.accounts.get(&self.account).copied().unwrap_or(0);
        if reserved.total + bytes > 2 * MAX_FRAME
            || account + bytes > MAX_FRAME
            || (self.member && reserved.members + bytes > MAX_FRAME)
        {
            return Err(DecodeError::Busy);
        }

        reserved.total += bytes;
        if self.member {
            reserved.members += bytes;
        }
        reserved
            .accounts
            .insert(self.account.clone(), account + bytes);

        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut reserved = self.budget.state.lock().unwrap();
        reserved.total -= self.bytes;
        if self.budget.member {
            reserved.members -= self.bytes;
        }
        let account = reserved.accounts.get_mut(&self.budget.account).unwrap();
        *account -= self.bytes;
        if *account == 0 {
            reserved.accounts.remove(&self.budget.account);
        }
    }
}

// Retain only sequence metadata after expiry or rejection, so fragments are drained
// without allocating or dispatching an expired request, and without closing ICE.
struct Discarded {
    total: usize,
    received: usize,
}

struct Assembly {
    _reservation: Reservation,
    total: usize,
    bytes: Vec<u8>,
    last_progress: Instant,
}

#[derive(Default)]
pub struct FrameDecoder {
    pending: HashMap<u32, Assembly>,
    discarded: HashMap<u32, Discarded>,
    buffered: usize,
    budget: ReassemblyBudget,
}

impl FrameDecoder {
    pub fn with_budget(budget: ReassemblyBudget) -> Self {
        Self {
            budget,
            ..Self::default()
        }
    }

    pub fn expire(&mut self) {
        self.pending.retain(|id, frame| {
            if frame.last_progress.elapsed() < REQUEST_TIMEOUT {
                return true;
            }

            self.buffered -= frame.bytes.len();
            self.discarded.insert(
                *id,
                Discarded {
                    total: frame.total,
                    received: frame.bytes.len(),
                },
            );
            false
        });
    }

    pub fn push(&mut self, packet: &[u8]) -> Result<Option<Frame>, DecodeError> {
        self.expire();
        if packet.len() < HEADER || packet.len() > MAX_PACKET || packet[0] != 1 {
            return Err("Invalid DataChannel fragment".into());
        }
        let id = u32::from_be_bytes(packet[1..5].try_into().unwrap());
        let total = u32::from_be_bytes(packet[5..9].try_into().unwrap()) as usize;
        let offset = u32::from_be_bytes(packet[9..13].try_into().unwrap()) as usize;
        let payload = &packet[HEADER..];
        if id == 0 || total > MAX_FRAME {
            return Err("Invalid transfer size".into());
        }
        if total == 0 && offset == 0 && payload.is_empty() {
            if let Some(frame) = self.pending.remove(&id) {
                self.buffered -= frame.bytes.len();
            }
            self.discarded.remove(&id);
            return Ok(None);
        }
        if payload.is_empty() || offset + payload.len() > total {
            return Err("DataChannel reassembly limit exceeded".into());
        }
        if let Some(frame) = self.discarded.get_mut(&id) {
            if frame.total != total || frame.received != offset {
                return Err("Invalid discarded fragment sequence".into());
            }
            frame.received += payload.len();
            if frame.received == total {
                self.discarded.remove(&id);
            }
            return Ok(None);
        }
        if offset == 0 {
            if self.pending.contains_key(&id)
                || self.pending.len() + self.discarded.len() >= MAX_IN_FLIGHT
            {
                return Err("Too many incomplete transfers".into());
            }
            let reserved: usize = self.pending.values().map(|frame| frame.total).sum();
            let reservation = if reserved + total > MAX_FRAME {
                Err(DecodeError::Busy)
            } else {
                self.budget.reserve(total)
            };
            let reservation = match reservation {
                Ok(reservation) => reservation,
                Err(DecodeError::Busy) if !self.pending.is_empty() => {
                    // Backpressure here would prevent this ordered channel from
                    // delivering the fragments that release its own reservation.
                    if payload.len() < total {
                        self.discarded.insert(
                            id,
                            Discarded {
                                total,
                                received: payload.len(),
                            },
                        );
                    }
                    return Err(DecodeError::Rejected(request_id_prefix(payload)));
                }
                Err(error) => return Err(error),
            };
            self.pending.insert(
                id,
                Assembly {
                    _reservation: reservation,
                    total,
                    bytes: Vec::new(),
                    last_progress: Instant::now(),
                },
            );
        }
        let frame = self.pending.get_mut(&id).ok_or("Unknown transfer")?;
        if frame.total != total || frame.bytes.len() != offset {
            return Err("Invalid fragment sequence".into());
        }
        let received = frame.bytes.len() + payload.len();
        if received > frame.bytes.capacity() {
            // Amortized growth, capped by the declared-size reservation.
            let capacity = (frame.bytes.capacity() * 2).max(received).min(total);
            frame.bytes.reserve_exact(capacity - frame.bytes.len());
        }
        frame.bytes.extend(payload);
        frame.last_progress = Instant::now();
        self.buffered += payload.len();
        if frame.bytes.len() != total {
            return Ok(None);
        }
        let frame = self.pending.remove(&id).unwrap();
        self.buffered -= frame.bytes.len();
        serde_json::from_slice(&frame.bytes)
            .map(Some)
            .map_err(|_| DecodeError::Invalid("Invalid application frame"))
    }
}

// Canonical request frames put type and id before their body. Read that bounded
// prefix solely to report a rejected request; never dispatch a partial frame.
fn request_id_prefix(packet: &[u8]) -> Option<String> {
    struct Prefix<'a>(&'a mut Option<String>);

    impl<'de> serde::de::Visitor<'de> for Prefix<'_> {
        type Value = ();

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a request frame prefix")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut kind = None;
            let mut id: Option<String> = None;
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "type" => kind = Some(map.next_value::<String>()?),
                    "id" => id = Some(map.next_value::<String>()?),
                    _ => {
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                if let (Some(kind), Some(id)) = (&kind, &id) {
                    *self.0 = (kind == "request" && !id.is_empty() && id.len() <= 128)
                        .then(|| id.clone());
                    return Ok(());
                }
            }
            Ok(())
        }
    }

    let mut id = None;
    // deserialize_map checks the closing brace after the visitor returns. The
    // fragment need not contain it; only the identity already read is retained.
    let _ = serde::de::Deserializer::deserialize_map(
        &mut serde_json::Deserializer::from_slice(packet),
        Prefix(&mut id),
    );
    id
}
