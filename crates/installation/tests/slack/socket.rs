//! Actual Luau/manager/adapter journeys over loopback WS and verified WSS.
//! Synthetic certificates and APIs do not prove connectivity to a real workspace.
use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{
            Callback, ErrorResponse, Request as UpgradeRequest, Response as UpgradeResponse,
        },
    },
};

/// The upstream callback fixes the error-response type; the fixture only
/// validates the upgrade and always returns its unchanged success response.
struct SlackUpgrade;

impl Callback for SlackUpgrade {
    fn on_request(
        self,
        request: &UpgradeRequest,
        response: UpgradeResponse,
    ) -> std::result::Result<UpgradeResponse, ErrorResponse> {
        assert_eq!(request.uri().path(), "/link/");
        assert_eq!(request.uri().query(), Some("ticket=synthetic-ticket"));
        assert!(!request.headers().contains_key("authorization"));
        Ok(response)
    }
}

trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Io for T {}

fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
    let cert = rustls::pki_types::CertificateDer::from(
        include_bytes!("../../../../tests/fixtures/slack-socket/server.der").to_vec(),
    );
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(
        include_bytes!("../../../../tests/fixtures/slack-socket/server-key.der").to_vec(),
    );
    // Workspace feature unification enables both crypto backends. The fixture
    // must select its provider independently of other tests or the host binary.
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())
        .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

pub(super) struct Fixture {
    pub origin: String,
    outgoing: mpsc::Sender<FixtureCommand>,
    incoming: mpsc::Receiver<Message>,
    connections: Arc<AtomicUsize>,
    pings: Arc<AtomicUsize>,
    server: JoinHandle<()>,
}

enum FixtureCommand {
    Frame(Message),
    Blackhole,
    WrongPongs,
}

impl Fixture {
    pub async fn start(tls: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "{}://{}",
            if tls { "wss" } else { "ws" },
            listener.local_addr().unwrap()
        );
        let (outgoing, mut send) = mpsc::channel(16);
        let (received, incoming) = mpsc::channel(16);
        let connections = Arc::new(AtomicUsize::new(0));
        let opened = connections.clone();
        let pings = Arc::new(AtomicUsize::new(0));
        let counted_pings = pings.clone();
        let server = tokio::spawn(async move {
            let mut stranded = Vec::new();
            'connections: loop {
                let (stream, _) = listener.accept().await.unwrap();
                let stream: Box<dyn Io> = if tls {
                    Box::new(tls_acceptor().accept(stream).await.unwrap())
                } else {
                    Box::new(stream)
                };
                let mut socket = accept_hdr_async(stream, SlackUpgrade).await.unwrap();
                let mut wrong_pongs = false;
                opened.fetch_add(1, Ordering::SeqCst);
                socket
                    .send(Message::text(
                        json!({"type": "hello", "connection_info": {"app_id": "ATEST"}})
                            .to_string(),
                    ))
                    .await
                    .unwrap();
                loop {
                    tokio::select! {
                        frame = send.recv() => {
                            match frame {
                                Some(FixtureCommand::Frame(frame)) => {
                                    if socket.send(frame).await.is_err() {
                                        break;
                                    }
                                }
                                Some(FixtureCommand::Blackhole) => {
                                    // Retain the TCP stream but stop reading or writing it:
                                    // no close, auto-Pong, FIN or RST can reveal the outage.
                                    stranded.push(socket);
                                    continue 'connections;
                                }
                                Some(FixtureCommand::WrongPongs) => wrong_pongs = true,
                                None => break 'connections,
                            }
                        }
                        frame = socket.next() => {
                            match frame {
                                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => continue 'connections,
                                Some(Ok(Message::Ping(_))) => {
                                    counted_pings.fetch_add(1, Ordering::SeqCst);
                                    if wrong_pongs {
                                        // Replaces tungstenite's queued automatic Pong.
                                        socket
                                            .send(Message::Pong(b"unrelated-pong".to_vec().into()))
                                            .await
                                            .unwrap();
                                    } else {
                                        socket.flush().await.unwrap();
                                    }
                                }
                                Some(Ok(frame)) => {
                                    if received.send(frame).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        Self {
            origin,
            outgoing,
            incoming,
            connections,
            pings,
            server,
        }
    }

    pub async fn send(&self, payload: Value, id: &str) {
        self.outgoing
            .send(FixtureCommand::Frame(Message::text(
                json!({
                    "envelope_id": id, "type": "events_api", "accepts_response_payload": false,
                    "payload": payload,
                })
                .to_string(),
            )))
            .await
            .unwrap();
    }

    pub async fn ack(&mut self, id: &str) {
        let frame = tokio::time::timeout(Duration::from_secs(3), self.incoming.recv())
            .await
            .expect("Slack envelope was not acknowledged within the protocol deadline")
            .expect("Socket stopped before its ACK");
        let Message::Text(text) = frame else {
            panic!("Expected an envelope ACK")
        };
        let ack: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(ack, json!({"envelope_id": id}));
    }

    pub async fn ping_while_metadata_is_held(&mut self) -> std::result::Result<(), String> {
        self.outgoing
            .send(FixtureCommand::Frame(Message::Ping(
                b"fixture-ping".to_vec().into(),
            )))
            .await
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(1), self.incoming.recv())
            .await
            .map_err(|_| "peer Ping waited for the blocked metadata handler".to_owned())?;
        match frame {
            Some(Message::Pong(bytes)) if bytes.as_ref() == b"fixture-ping" => Ok(()),
            _ => Err("unadmitted event was ACKed, or unexpected peer control frame".into()),
        }
    }

    pub async fn unexpected_ack(&mut self) -> bool {
        matches!(
            tokio::time::timeout(Duration::from_millis(500), self.incoming.recv()).await,
            Ok(Some(Message::Text(_)))
        )
    }

    pub async fn stop(self) {
        drop(self.outgoing);
        // A blackholed connection may be waiting for a replacement at cleanup.
        self.server.abort();
        match self.server.await {
            Err(error) if error.is_cancelled() => (),
            result => result.unwrap(),
        }
    }

    pub async fn fail_heartbeats_and_wait_for_replacement(&self, wrong_pongs: bool) {
        assert_eq!(self.connections.load(Ordering::SeqCst), 1);
        self.outgoing
            .send(if wrong_pongs {
                FixtureCommand::WrongPongs
            } else {
                FixtureCommand::Blackhole
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(14), async {
            while self.connections.load(Ordering::SeqCst) != 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Slack stayed connected to a silent half-open socket; no replacement was opened");
    }

    pub async fn wait_for_peer_ping_timeout(&self) {
        assert_eq!(self.connections.load(Ordering::SeqCst), 1);
        tokio::time::timeout(Duration::from_secs(35), async {
            while self.connections.load(Ordering::SeqCst) != 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Client Pongs concealed the absence of required server Pings");
        assert!(self.pings.load(Ordering::SeqCst) >= 5);
    }

    pub async fn stay_healthy(&mut self) {
        // Cross the 30s peer-Ping deadline. Both directions must remain healthy;
        // answering only client Pings would fail this witness.
        for index in 0..4 {
            tokio::time::sleep(Duration::from_secs(8)).await;
            let payload = format!("healthy-peer-{index}");
            self.outgoing
                .send(FixtureCommand::Frame(Message::Ping(
                    payload.clone().into_bytes().into(),
                )))
                .await
                .unwrap();
            let pong = tokio::time::timeout(Duration::from_secs(1), self.incoming.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(pong, Message::Pong(bytes) if bytes.as_ref() == payload.as_bytes()));
        }
        assert_eq!(
            self.connections.load(Ordering::SeqCst),
            1,
            "A healthy Socket Mode connection was replaced"
        );
        assert!(
            self.pings.load(Ordering::SeqCst) >= 6,
            "The client never checked the quiet healthy connection"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn socket_mention_uses_the_same_requester_route_and_real_answer_as_signed_http() {
    exchange(DeliveryScenario::Socket).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slack_metadata_cannot_block_peer_pings_or_ack_an_unadmitted_envelope() {
    exchange(DeliveryScenario::SocketSlowMetadata).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verified_wss_delivers_the_actual_requester_bound_answer() {
    exchange(DeliveryScenario::SocketTls).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overdue_metadata_does_not_late_ack_or_import_the_slack_event() {
    exchange(DeliveryScenario::SocketOverdueMetadata).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_untrusted_wss_certificate_never_reaches_a_slack_envelope() {
    use cairn_installation::plugins::{connections::Connection, socket as transport};
    use tokio_util::sync::CancellationToken;
    // This test starts the ordinary transport in-process instead of through
    // Cairn's main, which installs this same provider before any TLS client.
    cairn_installation::tls::install_provider();
    let root = TempDir::new().unwrap();
    let service = Service::new(common::config(root.path())).await.unwrap();
    common::relay_fixture::claimed(&service).await.unwrap();
    service
        .plugins
        .use_worker(PathBuf::from(env!("CARGO_BIN_EXE_cairn")));
    let slack = Arc::new(Slack::default());
    let provider = Router::new()
        .route("/api/{method}", post(api).get(api))
        .with_state(slack.clone());
    let (origin, server) = common::serve_locally(provider).await;
    service.plugins.route_http_to(Some(origin));
    let app = cairn_installation::http::router(service.clone())
        .await
        .unwrap();
    let (instance, _) = configure_transport(&app, &service, true).await;
    let connection: Connection = serde_json::from_value(instance).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    // Only change the destination: do not install the synthetic CA in this client.
    service
        .plugins
        .route_sockets_to(Some(format!("wss://{}", listener.local_addr().unwrap())));
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        assert!(
            tls_acceptor().accept(stream).await.is_err(),
            "The ordinary client trusted a private test CA without configuration"
        );
    });
    let package = registry::active(&service, "slack").await.unwrap();
    let stop = CancellationToken::new();
    let (running_service, running_stop) = (service.clone(), stop.clone());
    let running = tokio::spawn(async move {
        transport::run(&running_service, &connection, &package, &running_stop).await
    });
    tokio::time::timeout(Duration::from_secs(5), upstream)
        .await
        .unwrap()
        .unwrap();
    stop.cancel();
    running.await.unwrap().unwrap();
    assert_eq!(slack.socket_opens.load(Ordering::SeqCst), 1);
    assert!(
        service
            .store
            .list("plugin-events")
            .await
            .unwrap()
            .is_empty()
    );
    service.shutdown.cancel();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_first_connection_response_recovers_before_receiving_slack_work() {
    exchange(DeliveryScenario::SocketOpenInterrupted).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_route_does_not_stop_other_socket_channels() {
    exchange(DeliveryScenario::SocketStaleRoute).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_socket_recovers_the_next_requester_bound_exchange() {
    exchange(DeliveryScenario::SocketSilent).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrelated_pongs_cannot_keep_a_broken_connection_alive() {
    exchange(DeliveryScenario::SocketWrongPong).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quiet_healthy_socket_keeps_its_connection_and_requester() {
    exchange(DeliveryScenario::SocketHealthy).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_pongs_do_not_hide_missing_server_pings() {
    exchange(DeliveryScenario::SocketPeerSilent).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_metadata_errors_are_not_permanently_acknowledged() {
    exchange(DeliveryScenario::SocketTransientMetadata).await;
}
