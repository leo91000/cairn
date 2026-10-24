//! The path through Beacon when no direct connection is possible. Beacon
//! pairs this WebSocket with one the installation opens and copies the bytes:
//! TLS that ends on the pinned manager certificate, which Beacon cannot read.
use crate::Config;
use futures_util::{SinkExt, StreamExt};
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

const HANDSHAKE: Duration = Duration::from_secs(15);
const CHUNK: usize = 65_536;
const BUFFER: usize = 256 * 1024;
// Beacon pings every 15 seconds; silence beyond this means the path is gone.
const SILENCE: Duration = Duration::from_secs(60);

pub struct Refused {
    pub status: u16,
    pub message: String,
}

impl Refused {
    fn new(status: u16, message: &str) -> Self {
        Self {
            status,
            message: message.to_owned(),
        }
    }
}

/// Opens a channel to the manager through Beacon and completes TLS with it.
pub async fn open(config: &Config) -> Result<TlsStream<DuplexStream>, Refused> {
    let unreachable = || {
        Refused::new(
            502,
            "Cannot reach Beacon. Check outbound HTTPS connectivity.",
        )
    };
    let mut url = crate::beacon_origin(&config.beacon)
        .map_err(|_| unreachable())?
        .join(&format!(
            "api/relay/{}/nodes/connect",
            config.installation_id
        ))
        .map_err(|_| unreachable())?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme).map_err(|()| unreachable())?;
    let limits = WebSocketConfig::default()
        .max_message_size(Some(cairn_protocol::MAX_NODE_CHANNEL_MESSAGE))
        .max_frame_size(Some(cairn_protocol::MAX_NODE_CHANNEL_MESSAGE));
    let connected = tokio::time::timeout(
        HANDSHAKE,
        tokio_tungstenite::connect_async_with_config(url.as_str(), Some(limits), false),
    )
    .await
    .map_err(|_| unreachable())?;
    let socket = match connected {
        Ok((socket, _)) => socket,
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            let status = response.status().as_u16();
            let message = response
                .body()
                .as_deref()
                .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
                .and_then(|body| body["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| "Beacon refused the node connection.".to_owned());
            let status = if matches!(status, 429 | 503) {
                status
            } else {
                502
            };
            return Err(Refused { status, message });
        }
        Err(_) => return Err(unreachable()),
    };

    let (tls_side, channel_side) = tokio::io::duplex(BUFFER);
    tokio::spawn(pump(socket, channel_side));
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls(&config.pin)));
    let name = ServerName::try_from("cairn-manager").expect("valid name");
    match tokio::time::timeout(HANDSHAKE, connector.connect(name, tls_side)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(error))
            if matches!(
                error
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<rustls::Error>()),
                Some(rustls::Error::InvalidCertificate(_))
            ) =>
        {
            Err(Refused::new(
                502,
                "The manager's certificate does not match this node. Copy a fresh command from Cairn → Nodes.",
            ))
        }
        _ => Err(Refused::new(
            502,
            "The manager did not complete a secure connection.",
        )),
    }
}

/// Copies bytes between the WebSocket and the TLS side until either ends.
async fn pump<S>(socket: tokio_tungstenite::WebSocketStream<S>, channel: DuplexStream)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut from_tls, mut to_tls) = tokio::io::split(channel);
    let (mut sink, mut stream) = socket.split();
    let inbound = async move {
        // Pings are answered by tungstenite while reading.
        while let Ok(Some(Ok(message))) = tokio::time::timeout(SILENCE, stream.next()).await {
            match message {
                Message::Binary(bytes) => {
                    if to_tls.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Message::Ping(_) | Message::Pong(_) => {}
                _ => break,
            }
        }
        let _ = to_tls.shutdown().await;
    };
    let outbound = async move {
        let mut buffer = vec![0; CHUNK];
        loop {
            match from_tls.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let message = Message::Binary(buffer[..read].to_vec().into());
                    if sink.send(message).await.is_err() {
                        return;
                    }
                }
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    };
    tokio::join!(inbound, outbound);
}

fn tls(pin: &str) -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3 is supported")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            pin: pin.to_owned(),
            provider,
        }))
        .with_no_client_auth()
}

/// Trusts exactly the manager certificate whose digest is `pin`.
#[derive(Debug)]
struct Pinned {
    pin: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if cairn_protocol::nodes::certificate_pin(end_entity) == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _certificate: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
