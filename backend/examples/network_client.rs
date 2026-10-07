//! Test-only network probes. These never serve installation data or bypass authorization.
use std::{
    env,
    io::{Read, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        return Err("Use udp-server ADDRESS, udp-probe LOCAL TARGET TARGET, or http ADDRESS PATH STATUS [MARKER]".into());
    }

    match args.get(1).map(String::as_str) {
        Some("udp-server") => {
            let socket = UdpSocket::bind(&args[2])?;
            if let Some(ready) = args.get(3) {
                std::fs::write(ready, b"ready")?;
            }
            let mut bytes = [0; 512];
            loop {
                let (length, peer) = socket.recv_from(&mut bytes)?;
                let reply = format!("{} {}", peer, String::from_utf8_lossy(&bytes[..length]));
                socket.send_to(reply.as_bytes(), peer)?;
            }
        }
        Some("udp-probe") => {
            if args.len() != 5 {
                return Err("udp-probe requires a local address and two destinations".into());
            }
            let socket = UdpSocket::bind(&args[2])?;
            socket.set_read_timeout(Some(Duration::from_millis(150)))?;
            let mut received = 0;
            let mut peers = Vec::new();
            for sequence in 0..10 {
                let target = &args[3 + sequence % 2];
                let message = format!("leo-network-probe-{sequence}");
                socket.send_to(message.as_bytes(), target)?;
                let mut bytes = [0; 512];
                let deadline = Instant::now() + Duration::from_millis(150);
                while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                    socket.set_read_timeout(Some(remaining))?;
                    let Ok((length, source)) = socket.recv_from(&mut bytes) else {
                        break;
                    };
                    if source != target.parse::<SocketAddr>()? {
                        continue;
                    }
                    let reply = std::str::from_utf8(&bytes[..length])?;
                    if let Some(peer) = reply.strip_suffix(&format!(" {message}")) {
                        received += 1;
                        peers.push(peer.to_owned());
                        break;
                    }
                }
            }
            let local = socket.local_addr()?.to_string();
            let translated = peers.iter().any(|peer| peer != &local);
            peers.sort();
            peers.dedup();
            println!(
                "{{\"sent\":10,\"received\":{received},\"translated\":{translated},\"mappings\":{}}}",
                peers.len()
            );
        }
        Some("http") => {
            if !(5..=6).contains(&args.len()) {
                return Err("http requires an address, path and expected status".into());
            }
            // The bench forwards its loopback development origin through the topology.
            // Credentials come from stdin and are never printed or passed in argv.
            let mut cookie = String::new();
            std::io::stdin().read_to_string(&mut cookie)?;
            if cookie.contains(['\r', '\n']) {
                return Err("Invalid fixture cookie".into());
            }
            let started = Instant::now();
            let address: SocketAddr = args[2].parse()?;
            if !address.ip().is_loopback() || args[3].contains(['\r', '\n']) {
                return Err("HTTP probe accepts only a loopback fixture and a valid path".into());
            }
            let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            write!(
                stream,
                "GET {} HTTP/1.1\r\nHost: localhost\r\nCookie: {}\r\nConnection: close\r\n\r\n",
                args[3], cookie
            )?;
            let mut response = String::new();
            stream.take(9_000_001).read_to_string(&mut response)?;
            if response.len() > 9_000_000 {
                return Err("Fixture response exceeded the probe limit".into());
            }
            let status = response
                .split_whitespace()
                .nth(1)
                .ok_or("Missing HTTP status")?
                .parse::<u16>()?;
            let expected_status = args[4].parse::<u16>()?;
            if status != expected_status {
                return Err(format!("Expected HTTP {expected_status}, received {status}").into());
            }
            if let Some(marker) = args.get(5)
                && !response.contains(marker)
            {
                return Err("Expected conversation marker missing".into());
            }
            println!(
                "{{\"route\":\"relay\",\"status\":{status},\"elapsedMs\":{}}}",
                started.elapsed().as_millis()
            );
        }
        _ => return Err("Use udp-server, udp-probe or http".into()),
    }
    Ok(())
}
