//! Test-only network probes. These never serve installation data or bypass authorization.
use std::{
    env,
    io::{Read, Write},
    net::{TcpStream, UdpSocket},
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("udp-server") => {
            let socket = UdpSocket::bind(&args[2])?;
            let mut bytes = [0; 512];
            loop {
                let (length, peer) = socket.recv_from(&mut bytes)?;
                let reply = format!("{} {}", peer, String::from_utf8_lossy(&bytes[..length]));
                socket.send_to(reply.as_bytes(), peer)?;
            }
        }
        Some("udp-probe") => {
            let socket = UdpSocket::bind(&args[2])?;
            socket.set_read_timeout(Some(Duration::from_millis(150)))?;
            let mut received = 0;
            let mut peers = Vec::new();
            for sequence in 0..10 {
                let target = &args[3 + sequence % 2];
                let message = format!("leo-network-probe-{sequence}");
                socket.send_to(message.as_bytes(), target)?;
                let mut bytes = [0; 512];
                if let Ok((length, _)) = socket.recv_from(&mut bytes) {
                    let reply = std::str::from_utf8(&bytes[..length])?;
                    if let Some(peer) = reply.strip_suffix(&format!(" {message}")) {
                        received += 1;
                        peers.push(peer.to_owned());
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
            // The bench forwards its loopback development origin through the topology.
            // Credentials come from stdin and are never printed or passed in argv.
            let mut cookie = String::new();
            std::io::stdin().read_to_string(&mut cookie)?;
            if cookie.contains(['\r', '\n']) {
                return Err("Invalid fixture cookie".into());
            }
            let started = Instant::now();
            let mut stream = TcpStream::connect(&args[2])?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            write!(
                stream,
                "GET {} HTTP/1.1\r\nHost: localhost\r\nCookie: {}\r\nConnection: close\r\n\r\n",
                args[3], cookie
            )?;
            let mut response = String::new();
            stream.read_to_string(&mut response)?;
            let status = response
                .split_whitespace()
                .nth(1)
                .ok_or("Missing HTTP status")?;
            let expected_status = &args[4];
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
