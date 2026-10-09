//! Binding-only address discovery, in the single beacon process. No TURN or content.
use std::{
    collections::HashMap,
    io,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::net::UdpSocket;

pub fn url(origin: &str) -> Result<String, &'static str> {
    let origin = url::Url::parse(origin).map_err(|_| "Invalid STUN origin")?;
    let host = origin.host_str().ok_or("STUN origin has no host")?;
    Ok(format!("stun:{host}:3478"))
}

const STOPPED: u8 = 0;
const RUNNING: u8 = 1;
const RETRYING: u8 = 2;

#[derive(Clone, Default)]
pub struct Status(Arc<Counters>);

#[derive(Default)]
struct Counters {
    state: AtomicU8,
    receive_errors: AtomicU64,
    send_errors: AtomicU64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub status: &'static str,
    pub receive_errors: u64,
    pub send_errors: u64,
}

impl Status {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            status: match self.0.state.load(Ordering::Relaxed) {
                RUNNING => "running",
                RETRYING => "retrying",
                _ => "stopped",
            },
            receive_errors: self.0.receive_errors.load(Ordering::Relaxed),
            send_errors: self.0.send_errors.load(Ordering::Relaxed),
        }
    }
}

struct Running(Status);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.0.state.store(STOPPED, Ordering::Relaxed);
    }
}

pub async fn serve(socket: UdpSocket) -> io::Result<()> {
    serve_with_status(socket, Status::default()).await
}

pub async fn serve_with_status(socket: UdpSocket, status: Status) -> io::Result<()> {
    let _running = Running(status.clone());
    status.0.state.store(RUNNING, Ordering::Relaxed);
    let mut input = [0_u8; 513];
    let mut window = Instant::now();
    let mut counts = HashMap::<IpAddr, u16>::new();
    let mut total = 0_u16;
    let mut last_receive_warning: Option<Instant> = None;
    let mut last_send_warning: Option<Instant> = None;
    loop {
        let (length, source) = match socket.recv_from(&mut input).await {
            Ok(packet) => {
                status.0.state.store(RUNNING, Ordering::Relaxed);
                packet
            }
            Err(_) => {
                status.0.state.store(RETRYING, Ordering::Relaxed);
                status.0.receive_errors.fetch_add(1, Ordering::Relaxed);
                if last_receive_warning.is_none_or(|last| last.elapsed() >= Duration::from_secs(30))
                {
                    tracing::warn!("Beacon STUN receive failed; retrying");
                    last_receive_warning = Some(Instant::now());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        if window.elapsed() >= Duration::from_secs(1) {
            window = Instant::now();
            counts.clear();
            total = 0;
        }
        if !(20..=512).contains(&length)
            || input[..2] != [0, 1]
            || input[4..8] != [0x21, 0x12, 0xa4, 0x42]
            || usize::from(u16::from_be_bytes([input[2], input[3]])) + 20 != length
            || length % 4 != 0
            || total >= 1000
        {
            continue;
        }
        let mut offset = 20;
        let mut fingerprint = false;
        let mut valid = true;
        while offset < length {
            let kind = u16::from_be_bytes([input[offset], input[offset + 1]]);
            let size = usize::from(u16::from_be_bytes([input[offset + 2], input[offset + 3]]));
            let end = offset + 4 + size.next_multiple_of(4);
            if end > length {
                valid = false;
                break;
            }
            if kind == 0x8028 {
                if size != 4
                    || end != length
                    || u32::from_be_bytes(input[offset + 4..end].try_into().unwrap())
                        != crc32fast::hash(&input[..offset]) ^ 0x5354554e
                {
                    valid = false;
                    break;
                }
                fingerprint = true;
            } else if kind < 0x8000 {
                // This address-discovery usage supports no authenticated/ICE/TURN extensions.
                valid = false;
                break;
            }
            offset = end;
        }
        if !valid {
            continue;
        }
        // No unbounded IP tracking, tasks, retransmissions or amplification loop.
        if !counts.contains_key(&source.ip()) && counts.len() >= 1000 {
            continue;
        }
        let count = counts.entry(source.ip()).or_default();
        if *count >= 20 {
            continue;
        }
        *count += 1;
        total += 1;
        let mut response = input[..20].to_vec();
        response[..2].copy_from_slice(&[1, 1]);
        response.extend([0, 0x20]); // XOR-MAPPED-ADDRESS (RFC 8489).
        let address_length = if source.is_ipv4() { 8_u16 } else { 20 };
        response.extend(address_length.to_be_bytes());
        response.extend([0, if source.is_ipv4() { 1 } else { 2 }]);
        response.extend((source.port() ^ 0x2112).to_be_bytes());
        match source.ip() {
            IpAddr::V4(address) => response.extend(
                address
                    .octets()
                    .iter()
                    .zip(input[4..8].iter())
                    .map(|(byte, mask)| byte ^ mask),
            ),
            IpAddr::V6(address) => response.extend(
                address
                    .octets()
                    .iter()
                    .zip(input[4..20].iter())
                    .map(|(byte, mask)| byte ^ mask),
            ),
        }
        let message_length = address_length + 4 + if fingerprint { 8 } else { 0 };
        response[2..4].copy_from_slice(&message_length.to_be_bytes());
        if fingerprint {
            let crc = crc32fast::hash(&response) ^ 0x5354554e;
            response.extend([0x80, 0x28, 0, 4]);
            response.extend(crc.to_be_bytes());
        }
        if socket.send_to(&response, source).await.is_err() {
            status.0.send_errors.fetch_add(1, Ordering::Relaxed);
            if last_send_warning.is_none_or(|last| last.elapsed() >= Duration::from_secs(30)) {
                tracing::warn!("Beacon STUN send failed; continuing");
                last_send_warning = Some(Instant::now());
            }
        }
    }
}
