//! Diagnostic launcher: the network bench runs the production Binding responder.
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let address = arguments.next().expect("listen address");
    let ready = arguments.next().expect("readiness file");
    let socket = tokio::net::UdpSocket::bind(address).await?;
    std::fs::write(ready, b"ready")?;
    cairn_beacon::stun::serve(socket).await
}
