//! Binary envelopes around the existing JSON frames on a reliable ordered DataChannel.
use crate::{Frame, MAX_FRAME, MAX_IN_FLIGHT, REQUEST_TIMEOUT};
use std::{collections::HashMap, time::Instant};

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

struct Assembly {
    total: usize,
    bytes: Vec<u8>,
    started: Instant,
}

#[derive(Default)]
pub struct FrameDecoder {
    pending: HashMap<u32, Assembly>,
    buffered: usize,
}

impl FrameDecoder {
    pub fn expired(&self) -> bool {
        self.pending
            .values()
            .any(|frame| frame.started.elapsed() >= REQUEST_TIMEOUT)
    }

    pub fn push(&mut self, packet: &[u8]) -> Result<Option<Frame>, &'static str> {
        if packet.len() < HEADER || packet.len() > MAX_PACKET || packet[0] != 1 || self.expired() {
            return Err("Invalid or expired DataChannel fragment");
        }
        let id = u32::from_be_bytes(packet[1..5].try_into().unwrap());
        let total = u32::from_be_bytes(packet[5..9].try_into().unwrap()) as usize;
        let offset = u32::from_be_bytes(packet[9..13].try_into().unwrap()) as usize;
        let payload = &packet[HEADER..];
        if id == 0 || total > MAX_FRAME {
            return Err("Invalid transfer size");
        }
        if total == 0 && offset == 0 && payload.is_empty() {
            if let Some(frame) = self.pending.remove(&id) {
                self.buffered -= frame.bytes.len();
            }
            return Ok(None);
        }
        if payload.is_empty()
            || offset + payload.len() > total
            || self.buffered + payload.len() > MAX_FRAME
        {
            return Err("DataChannel reassembly limit exceeded");
        }
        if offset == 0 {
            if self.pending.contains_key(&id) || self.pending.len() >= MAX_IN_FLIGHT {
                return Err("Too many incomplete transfers");
            }
            self.pending.insert(
                id,
                Assembly {
                    total,
                    bytes: Vec::new(),
                    started: Instant::now(),
                },
            );
        }
        let frame = self.pending.get_mut(&id).ok_or("Unknown transfer")?;
        if frame.total != total || frame.bytes.len() != offset {
            return Err("Invalid fragment sequence");
        }
        frame.bytes.extend(payload);
        self.buffered += payload.len();
        if frame.bytes.len() != total {
            return Ok(None);
        }
        let frame = self.pending.remove(&id).unwrap();
        self.buffered -= frame.bytes.len();
        serde_json::from_slice(&frame.bytes)
            .map(Some)
            .map_err(|_| "Invalid application frame")
    }
}
