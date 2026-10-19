use bytes::Bytes;
use rtc::sctp::{
    Association, AssociationHandle, ClientConfig, DatagramEvent, Endpoint, EndpointConfig, Payload,
    PayloadProtocolIdentifier, ServerConfig, TransportConfig,
};
use rtc::shared::TransportProtocol;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

struct Node {
    endpoint: Endpoint,
    association: Option<(AssociationHandle, Association)>,
    address: SocketAddr,
}

impl Node {
    fn new(port: u16, server: bool) -> Self {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        Self {
            endpoint: Endpoint::new(
                address,
                TransportProtocol::UDP,
                Arc::new(EndpointConfig::default()),
                server.then(|| Arc::new(ServerConfig::new(TransportConfig::default()))),
            ),
            association: None,
            address,
        }
    }

    fn receive(&mut self, now: Instant, remote: SocketAddr, packet: Bytes) {
        if let Some((handle, event)) = self.endpoint.handle(now, remote, None, packet) {
            match event {
                DatagramEvent::NewAssociation(association) => {
                    assert!(self.association.is_none());
                    self.association = Some((handle, association));
                }
                DatagramEvent::AssociationEvent(event) => {
                    let (ours, association) =
                        self.association.as_mut().expect("association exists");
                    assert_eq!(handle, *ours);
                    association.handle_event(event);
                }
                _ => panic!("unexpected endpoint event"),
            }
        }
    }

    fn outgoing(&mut self, now: Instant) -> Vec<Bytes> {
        let mut packets = Vec::new();
        if let Some((handle, association)) = &mut self.association {
            while let Some(event) = association.poll_endpoint_event() {
                self.endpoint.handle_event(*handle, event);
            }
            while let Some(transmit) = association.poll_transmit(now) {
                match transmit.message {
                    Payload::RawEncode(encoded) => packets.extend(encoded),
                    _ => panic!("expected encoded SCTP"),
                }
            }
        }
        packets
    }

    fn timeout(&self) -> Option<Instant> {
        self.association
            .as_ref()
            .and_then(|(_, a)| a.poll_timeout())
    }

    fn expire(&mut self, now: Instant) {
        if self.timeout().is_some_and(|t| t <= now) {
            self.association.as_mut().unwrap().1.handle_timeout(now);
        }
    }

    fn read(&mut self) -> Vec<Vec<u8>> {
        let Some((_, association)) = &mut self.association else {
            return Vec::new();
        };
        let mut messages = Vec::new();
        for id in association.stream_ids() {
            let mut stream = association.stream(id).unwrap();
            while let Some(chunks) = stream.read().unwrap() {
                messages.push(chunks.to_payload(65_536).unwrap().to_vec());
            }
        }
        messages
    }
}

// Parse only SCTP common/chunk headers. No ciphertext-size or global ordinal match.
fn data_fragments(packet: &[u8]) -> Vec<(u32, u8)> {
    assert!(packet.len() >= 12);
    let mut offset = 12;
    let mut fragments = Vec::new();
    while offset < packet.len() {
        assert!(offset + 4 <= packet.len());
        let size = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;
        assert!(size >= 4 && offset + size <= packet.len());
        if packet[offset] == 0 {
            assert!(size >= 16);
            fragments.push((
                u32::from_be_bytes(packet[offset + 4..offset + 8].try_into().unwrap()),
                packet[offset + 1],
            ));
        }
        offset += (size + 3) & !3;
    }
    assert_eq!(offset, packet.len());
    fragments
}

#[derive(Default)]
struct Plan {
    armed: bool,
    target: Option<u32>,
    drop_attempts: usize,
    attempts: Vec<u128>,
    forwarded_data: usize,
    negative_control: bool,
}

impl Plan {
    fn drop_packet(&mut self, packet: &[u8], now: Instant, start: Instant) -> bool {
        let fragments = data_fragments(packet);
        if !self.armed {
            return false;
        }
        if self.target.is_none()
            && let Some((tsn, flags)) = fragments.first()
        {
            assert_eq!(
                flags & 0x07,
                0x02,
                "target must begin a fragmented ordered message"
            );
            self.target = Some(*tsn);
        }
        if fragments.iter().any(|(tsn, _)| Some(*tsn) == self.target) {
            self.attempts.push(now.duration_since(start).as_millis());
            if self.attempts.len() <= self.drop_attempts || self.negative_control {
                return true;
            }
        }
        self.forwarded_data += fragments.len();
        false
    }
}

struct Pair {
    a: Node,
    b: Node,
    now: Instant,
    start: Instant,
    plan: Plan,
}

impl Pair {
    fn new() -> Self {
        let now = Instant::now();
        let mut a = Node::new(5000, false);
        let b = Node::new(5001, true);
        a.association = Some(
            a.endpoint
                .connect(
                    now,
                    ClientConfig::new(TransportConfig::default()),
                    b.address,
                )
                .unwrap(),
        );
        let mut pair = Self {
            a,
            b,
            now,
            start: now,
            plan: Plan::default(),
        };
        pair.pump();
        assert!(!pair.a.association.as_ref().unwrap().1.is_handshaking());
        assert!(!pair.b.association.as_ref().unwrap().1.is_handshaking());
        pair
    }

    fn pump(&mut self) {
        for _ in 0..128 {
            let a = self.a.outgoing(self.now);
            let b = self.b.outgoing(self.now);
            if a.is_empty() && b.is_empty() {
                return;
            }
            for packet in a {
                if !self.plan.drop_packet(&packet, self.now, self.start) {
                    self.b.receive(self.now, self.a.address, packet);
                }
            }
            for packet in b {
                self.a.receive(self.now, self.b.address, packet);
            }
        }
        panic!("non-quiescent SCTP pump");
    }

    fn advance(&mut self, milliseconds: u64) {
        let end = self.start + Duration::from_millis(milliseconds);
        for _ in 0..512 {
            let next = [self.a.timeout(), self.b.timeout()]
                .into_iter()
                .flatten()
                .min();
            match next {
                Some(next) if next <= end => {
                    assert!(next >= self.now);
                    self.now = next;
                    self.a.expire(next);
                    self.b.expire(next);
                    self.pump();
                }
                _ => {
                    self.now = end;
                    return;
                }
            }
        }
        panic!("timeout loop exceeded");
    }

    fn send(&mut self, drops: usize, negative: bool) -> Vec<Vec<u8>> {
        self.plan = Plan {
            armed: true,
            drop_attempts: drops,
            negative_control: negative,
            ..Default::default()
        };
        self.start = self.now;
        let expected = vec![
            (0..1600).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
            b"later ordered message".to_vec(),
        ];
        let association = &mut self.a.association.as_mut().unwrap().1;
        let mut stream = association
            .open_stream(0, PayloadProtocolIdentifier::Binary)
            .unwrap();
        for message in &expected {
            assert_eq!(stream.write(self.now, message).unwrap(), message.len());
        }
        self.pump();
        expected
    }
}

#[test]
fn recoverable_loss_requires_exact_ordered_delivery() {
    let mut pair = Pair::new();
    let expected = pair.send(2, std::env::var_os("CAIRN_SCTP_NEGATIVE_CONTROL").is_some());
    pair.advance(2999);
    assert!(pair.b.read().is_empty());
    pair.advance(3000);
    let recovered = pair.b.read();
    assert_eq!(
        recovered.len(),
        expected.len(),
        "recoverable DATA loss must complete by 3 seconds"
    );
    assert!(
        recovered == expected,
        "recoverable delivery must preserve bytes and order"
    );
    pair.advance(20_000);
    assert!(pair.b.read().is_empty(), "duplicate delivery");
    assert_eq!(pair.plan.attempts, [0, 1000, 3000]);
    assert!(pair.plan.forwarded_data >= 3);
    println!(
        "recoverable: selected DATA attempt times ms {:?}; exact once, ordered, integrity verified",
        pair.plan.attempts
    );
}

#[test]
fn four_losses_cross_health_boundary_before_ordered_recovery() {
    let mut pair = Pair::new();
    let expected = pair.send(4, false);
    pair.advance(14_999);
    assert!(
        pair.b.read().is_empty(),
        "delivery before necessary fragment"
    );
    assert_eq!(pair.plan.attempts, [0, 1000, 3000, 7000]);
    assert!(
        pair.plan.forwarded_data >= 1,
        "later DATA must actually be forwarded"
    );
    pair.advance(15_000);
    let recovered = pair.b.read();
    assert_eq!(
        recovered.len(),
        expected.len(),
        "delivery must resume at 15 seconds"
    );
    assert!(
        recovered == expected,
        "recovered delivery must preserve bytes and order"
    );
    pair.advance(20_000);
    assert!(pair.b.read().is_empty());
    assert_eq!(pair.plan.attempts, [0, 1000, 3000, 7000, 15000]);
    println!(
        "starved: selected DATA attempt times ms {:?}; no delivery before 15s, then exact ordered recovery",
        pair.plan.attempts
    );
}
