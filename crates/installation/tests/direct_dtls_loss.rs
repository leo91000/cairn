use rtc::{
    crypto::default_provider,
    dtls::{
        config::{ClientAuthType, ConfigBuilder},
        crypto::Certificate,
        endpoint::{Endpoint, EndpointEvent},
    },
    shared::TransportProtocol,
};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Association {
    client: Endpoint,
    server: Endpoint,
    client_addr: SocketAddr,
    server_addr: SocketAddr,
    start: Instant,
}

impl Association {
    /// Starts a mutually authenticated handshake; the ClientHello is queued.
    fn connect() -> Result<Self, Box<dyn std::error::Error>> {
        let provider = default_provider()?;
        let certificate =
            Certificate::generate_self_signed(vec!["localhost".into()], provider.crypto())?;
        let configuration = ConfigBuilder::default()
            .with_crypto_provider(provider)
            .with_certificates(vec![certificate])
            // Test-only self-signed peers, as in WebRTC; installation grant and
            // observed fingerprint checks are exercised by the authenticated bench.
            .with_insecure_skip_verify(true)
            .with_client_auth(ClientAuthType::RequireAnyClientCert);
        let client_addr: SocketAddr = "127.0.0.1:44001".parse()?;
        let server_addr: SocketAddr = "127.0.0.1:44002".parse()?;
        let mut client = Endpoint::new(client_addr, TransportProtocol::UDP, None);
        let server = Endpoint::new(
            server_addr,
            TransportProtocol::UDP,
            Some(Arc::new(configuration.clone().build(false, None)?)),
        );
        let start = Instant::now();

        client.connect(
            start,
            server_addr,
            Arc::new(configuration.build(true, None)?),
            None,
        )?;

        Ok(Self {
            client,
            server,
            client_addr,
            server_addr,
            start,
        })
    }
}

fn transfer(
    source: &mut Endpoint,
    destination: &mut Endpoint,
    source_addr: SocketAddr,
    now: Instant,
) -> Result<bool, Box<dyn std::error::Error>> {
    transfer_recorded(source, destination, source_addr, now, &mut Vec::new())
}

/// Delivers every queued datagram and appends a copy of each to `recorded`.
fn transfer_recorded(
    source: &mut Endpoint,
    destination: &mut Endpoint,
    source_addr: SocketAddr,
    now: Instant,
    recorded: &mut Vec<Vec<u8>>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut completed = false;

    while let Some(packet) = source.poll_transmit() {
        recorded.push(packet.message.to_vec());
        completed |= destination
            .read(now, source_addr, packet.transport.ecn, packet.message)?
            .iter()
            .any(|event| matches!(event, EndpointEvent::HandshakeComplete));
    }

    Ok(completed)
}

#[test]
fn lost_server_flight_recovers_after_repeated_client_hello() -> TestResult {
    recover_lost_flight(LostFlight::Server)
}

#[test]
fn lost_client_certificate_flight_recovers_after_repeated_server_flight() -> TestResult {
    recover_lost_flight(LostFlight::ClientCertificate)
}

#[test]
fn lost_final_server_flight_recovers_after_repeated_client_finished() -> TestResult {
    recover_lost_flight(LostFlight::ServerFinished)
}

#[derive(Clone, Copy)]
enum LostFlight {
    Server,
    ClientCertificate,
    ServerFinished,
}

fn recover_lost_flight(lost_flight: LostFlight) -> TestResult {
    let drop_client_flight = matches!(lost_flight, LostFlight::ClientCertificate);
    let Association {
        mut client,
        mut server,
        client_addr,
        server_addr,
        start,
    } = Association::connect()?;

    // ClientHello -> HelloVerifyRequest -> ClientHello with the cookie.
    transfer(&mut client, &mut server, client_addr, start)?;
    transfer(&mut server, &mut client, server_addr, start)?;
    transfer(&mut client, &mut server, client_addr, start)?;

    let mut client_completed = false;
    let mut server_completed = false;

    if drop_client_flight || matches!(lost_flight, LostFlight::ServerFinished) {
        client_completed |= transfer(&mut server, &mut client, server_addr, start)?;
    }
    if matches!(lost_flight, LostFlight::ServerFinished) {
        server_completed |= transfer(&mut client, &mut server, client_addr, start)?;
        assert!(
            server_completed,
            "the server considers DTLS established before the lost final flight"
        );
    }

    // Lose one complete flight. Receiving the peer's repeated previous flight
    // must not disable our retransmission while waiting for its next flight.
    let lost_sender = if drop_client_flight {
        &mut client
    } else {
        &mut server
    };
    let mut dropped = 0;

    while lost_sender.poll_transmit().is_some() {
        dropped += 1;
    }

    assert!(dropped > 0, "a handshake flight must actually be lost");

    for tick in 1..=50 {
        let now = start + Duration::from_millis(tick * 100);

        // Deliver the peer's repeated previous flight before our own timer:
        // the precise race that cancels retransmission on the affected side.
        if drop_client_flight {
            if server
                .poll_timeout(&client_addr)
                .is_some_and(|deadline| deadline <= now)
            {
                server.handle_timeout(client_addr, now)?;
            }
            client_completed |= transfer(&mut server, &mut client, server_addr, now)?;
        }
        if client
            .poll_timeout(&server_addr)
            .is_some_and(|deadline| deadline <= now)
        {
            client.handle_timeout(server_addr, now)?;
        }
        server_completed |= transfer(&mut client, &mut server, client_addr, now)?;
        if server
            .poll_timeout(&client_addr)
            .is_some_and(|deadline| deadline <= now)
        {
            server.handle_timeout(client_addr, now)?;
        }
        client_completed |= transfer(&mut server, &mut client, server_addr, now)?;

        if client_completed && server_completed {
            break;
        }
    }
    assert!(
        client_completed && server_completed,
        "DTLS must recover a lost flight after the peer repeats its previous flight (lost client flight: {drop_client_flight})"
    );

    client.write(
        start + Duration::from_secs(5),
        server_addr,
        b"direct after packet loss",
    )?;
    let response = client
        .poll_transmit()
        .expect("an established association sends application data");
    let replay = response.message.clone();
    let events = server.read(
        start + Duration::from_secs(5),
        client_addr,
        response.transport.ecn,
        response.message,
    )?;
    let application_delivered = events.iter().any(|event| {
        matches!(event, EndpointEvent::ApplicationData(data) if data.as_ref() == b"direct after packet loss")
    });
    assert!(
        application_delivered,
        "the recovered DTLS association delivers application data"
    );
    assert!(
        server
            .read(start + Duration::from_secs(5), client_addr, None, replay)?
            .is_empty(),
        "retransmission recovery must preserve application replay protection"
    );
    Ok(())
}

const HANDSHAKE_CONTENT_TYPE: u8 = 22;
const SERVER_HELLO_DONE: u8 = 14;
const RECORD_HEADER_LENGTH: usize = 13;

/// Fresh epoch-0 record sequence numbers, above any used by the handshake.
const INJECTED_SEQUENCE_START: u64 = 1_000;

/// A 25-byte epoch-0 ServerHelloDone record: never valid once DTLS is established.
fn unexpected_handshake_record(message_sequence: u16, record_sequence: u64) -> Vec<u8> {
    let mut record = vec![HANDSHAKE_CONTENT_TYPE, 0xfe, 0xfd, 0, 0];

    record.extend_from_slice(&record_sequence.to_be_bytes()[2..]);
    record.extend_from_slice(&12u16.to_be_bytes());
    record.extend_from_slice(&[SERVER_HELLO_DONE, 0, 0, 0]);
    record.extend_from_slice(&message_sequence.to_be_bytes());
    record.extend_from_slice(&[0; 6]);
    record
}

/// The epoch-0 handshake records of `datagrams`, as the peer would repeat
/// them: same content under fresh record sequence numbers.
fn repeated_handshake_records(datagrams: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut repeated = Vec::new();

    for datagram in datagrams {
        let mut offset = 0;

        while offset + RECORD_HEADER_LENGTH <= datagram.len() {
            let length = u16::from_be_bytes([datagram[offset + 11], datagram[offset + 12]]);
            let end = offset + RECORD_HEADER_LENGTH + usize::from(length);
            let record = &datagram[offset..end];
            let is_epoch_zero_handshake =
                record[0] == HANDSHAKE_CONTENT_TYPE && record[3..5] == [0, 0];

            if is_epoch_zero_handshake {
                let record_sequence = INJECTED_SEQUENCE_START + 100 + repeated.len() as u64;
                let mut record = record.to_vec();

                record[5..11].copy_from_slice(&record_sequence.to_be_bytes()[2..]);
                repeated.push(record);
            }
            offset = end;
        }
    }

    repeated
}

/// Unexpected records for every plausible message number, then the peer's
/// own epoch-0 handshake records repeated.
fn stray_handshake_records(peer_datagrams: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut records: Vec<_> = (0..=16)
        .map(|message_sequence| {
            unexpected_handshake_record(
                message_sequence,
                INJECTED_SEQUENCE_START + u64::from(message_sequence),
            )
        })
        .collect();

    records.extend(repeated_handshake_records(peer_datagrams));
    records
}

fn assert_application_data_flows(association: &mut Association) -> TestResult {
    let now = association.start + Duration::from_secs(5);
    let directions = [
        (
            true,
            b"client to server after stray handshake records".as_slice(),
        ),
        (
            false,
            b"server to client after stray handshake records".as_slice(),
        ),
    ];

    for (from_client, payload) in directions {
        let (source, destination, source_addr, destination_addr) = if from_client {
            (
                &mut association.client,
                &mut association.server,
                association.client_addr,
                association.server_addr,
            )
        } else {
            (
                &mut association.server,
                &mut association.client,
                association.server_addr,
                association.client_addr,
            )
        };

        source.write(now, destination_addr, payload)?;

        let mut delivered = false;

        while let Some(packet) = source.poll_transmit() {
            delivered |= destination
                .read(now, source_addr, packet.transport.ecn, packet.message)?
                .iter()
                .any(|event| {
                    matches!(event, EndpointEvent::ApplicationData(data) if data.as_ref() == payload)
                });
        }
        assert!(
            delivered,
            "the established association still delivers application data (client sends: {from_client})"
        );
    }

    Ok(())
}

#[test]
fn handshake_records_after_completion_keep_the_session_open() -> TestResult {
    let mut association = Association::connect()?;
    let mut client_datagrams = Vec::new();
    let mut server_datagrams = Vec::new();
    let mut client_completed = false;
    let mut server_completed = false;

    for _ in 0..4 {
        server_completed |= transfer_recorded(
            &mut association.client,
            &mut association.server,
            association.client_addr,
            association.start,
            &mut client_datagrams,
        )?;
        client_completed |= transfer_recorded(
            &mut association.server,
            &mut association.client,
            association.server_addr,
            association.start,
            &mut server_datagrams,
        )?;
    }
    assert!(
        client_completed && server_completed,
        "DTLS completes without loss"
    );

    // Each side receives records its peer could still emit or a path could
    // inject: an unknown later message and repeats of earlier flights. The
    // concluded verification must not be revisited, so neither side fails or
    // alerts, whichever records reach it first.
    let now = association.start + Duration::from_secs(1);
    let records_for_client = stray_handshake_records(&server_datagrams);
    let records_for_server = stray_handshake_records(&client_datagrams);

    for record in records_for_client {
        let result =
            association
                .client
                .read(now, association.server_addr, None, record.as_slice().into());

        if let Err(error) = result {
            panic!("the client keeps its session after a stray handshake record: {error:?}");
        }

        transfer(
            &mut association.client,
            &mut association.server,
            association.client_addr,
            now,
        )?;
    }
    for record in records_for_server {
        let result =
            association
                .server
                .read(now, association.client_addr, None, record.as_slice().into());

        if let Err(error) = result {
            panic!("the server keeps its session after a stray handshake record: {error:?}");
        }

        let result = transfer(
            &mut association.server,
            &mut association.client,
            association.server_addr,
            now,
        );

        if let Err(error) = result {
            panic!("the client keeps its session after the server's answer: {error:?}");
        }
    }

    assert_application_data_flows(&mut association)
}

#[test]
fn completed_server_resends_final_flight_only_for_a_repeated_finished() -> TestResult {
    let mut association = Association::connect()?;
    let mut client_datagrams = Vec::new();

    // ClientHello -> HelloVerifyRequest -> ClientHello -> server flight 4 ->
    // client flight 5, which completes the server.
    transfer(
        &mut association.client,
        &mut association.server,
        association.client_addr,
        association.start,
    )?;
    transfer(
        &mut association.server,
        &mut association.client,
        association.server_addr,
        association.start,
    )?;
    transfer(
        &mut association.client,
        &mut association.server,
        association.client_addr,
        association.start,
    )?;
    transfer(
        &mut association.server,
        &mut association.client,
        association.server_addr,
        association.start,
    )?;

    let server_completed = transfer_recorded(
        &mut association.client,
        &mut association.server,
        association.client_addr,
        association.start,
        &mut client_datagrams,
    )?;

    assert!(
        server_completed,
        "the server completes on the client's Finished"
    );

    // Lose the server's final flight; it is delivered late below.
    let mut late_final_flight = Vec::new();

    while let Some(packet) = association.server.poll_transmit() {
        late_final_flight.push(packet);
    }

    let final_flight_datagrams = late_final_flight.len();

    assert!(
        final_flight_datagrams > 0,
        "the final flight must actually be lost"
    );

    // Only the peer repeating its final flight warrants a retransmission
    // (RFC 6347 4.2.4): any other handshake record is left unanswered.
    let now = association.start + Duration::from_millis(500);

    for record in stray_handshake_records(&client_datagrams) {
        association
            .server
            .read(now, association.client_addr, None, record.as_slice().into())?;
        assert!(
            association.server.poll_transmit().is_none(),
            "the completed server answers only a repeated Finished"
        );
    }

    // The client's timer repeats its whole flight 5; its Finished triggers
    // exactly one copy of the cached final flight.
    let deadline = association
        .client
        .poll_timeout(&association.server_addr)
        .expect("the client waits for the final flight");

    association
        .client
        .handle_timeout(association.server_addr, deadline)?;

    let mut repeated_client_flight = Vec::new();

    while let Some(packet) = association.client.poll_transmit() {
        repeated_client_flight.push(packet.message.to_vec());
    }

    let mut resent = Vec::new();

    for datagram in &repeated_client_flight {
        association.server.read(
            deadline,
            association.client_addr,
            None,
            datagram.as_slice().into(),
        )?;
        while let Some(packet) = association.server.poll_transmit() {
            resent.push(packet);
        }
    }
    assert_eq!(
        resent.len(),
        final_flight_datagrams,
        "one repeated Finished resends the final flight once"
    );

    // Replayed records are discarded before they can trigger anything.
    for datagram in &repeated_client_flight {
        association.server.read(
            deadline,
            association.client_addr,
            None,
            datagram.as_slice().into(),
        )?;
    }
    assert!(
        association.server.poll_transmit().is_none(),
        "a replayed Finished does not trigger another retransmission"
    );

    let mut client_completed = false;

    for packet in resent {
        client_completed |= association
            .client
            .read(
                deadline,
                association.server_addr,
                packet.transport.ecn,
                packet.message,
            )?
            .iter()
            .any(|event| matches!(event, EndpointEvent::HandshakeComplete));
    }
    assert!(
        client_completed,
        "the resent final flight completes the client"
    );

    // The receiver of the final flight ignores a repeated Finished: it neither
    // re-verifies nor answers.
    for packet in late_final_flight {
        association.client.read(
            deadline,
            association.server_addr,
            packet.transport.ecn,
            packet.message,
        )?;
    }
    assert!(
        association.client.poll_transmit().is_none(),
        "the completed client does not answer a repeated final flight"
    );

    assert_application_data_flows(&mut association)
}
