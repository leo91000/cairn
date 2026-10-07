//! Binding-only address discovery, in the single official process. No TURN or content.
use std::{
    collections::HashMap,
    io,
    net::IpAddr,
    time::{Duration, Instant},
};
use tokio::net::UdpSocket;

pub fn url(origin: &str) -> Result<String, &'static str> {
    let origin = url::Url::parse(origin).map_err(|_| "Invalid STUN origin")?;
    let host = origin.host_str().ok_or("STUN origin has no host")?;
    Ok(format!("stun:{host}:3478"))
}

pub async fn serve(socket: UdpSocket) -> io::Result<()> {
    let mut input = [0_u8; 513];
    let mut window = Instant::now();
    let mut counts = HashMap::<IpAddr, u16>::new();
    let mut total = 0_u16;
    loop {
        let (length, source) = socket.recv_from(&mut input).await?;
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
        response[2..4].copy_from_slice(&(address_length + 4).to_be_bytes());
        let _ = socket.send_to(&response, source).await;
    }
}
