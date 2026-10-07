use std::time::Duration;
use tokio::net::UdpSocket;

#[tokio::test]
async fn binding_reports_the_actual_udp_source_and_never_relays_turn_or_invalid_packets() {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let destination = server.local_addr().unwrap();
    let task = tokio::spawn(leo_official_service::stun::serve(server));
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let source = client.local_addr().unwrap();
    let request = [
        0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    ];
    client.send_to(&request, destination).await.unwrap();
    let mut reply = [0; 512];
    let (length, sender) =
        tokio::time::timeout(Duration::from_secs(1), client.recv_from(&mut reply))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(sender, destination);
    assert_eq!(length, 32);
    assert_eq!(&reply[..8], &[1, 1, 0, 12, 0x21, 0x12, 0xa4, 0x42]);
    assert_eq!(&reply[8..20], &request[8..20]);
    assert_eq!(&reply[20..26], &[0, 0x20, 0, 8, 0, 1]);
    assert_eq!(
        u16::from_be_bytes(reply[26..28].try_into().unwrap()) ^ 0x2112,
        source.port()
    );
    assert_eq!(&reply[28..32], &[0x7f ^ 0x21, 0x12, 0xa4, 1 ^ 0x42]);
    for mut invalid in [request, request, request] {
        invalid[1] = 3; // TURN Allocate is deliberately unsupported.
        client.send_to(&invalid, destination).await.unwrap();
    }
    let mut invalid = request;
    invalid[3] = 4; // Attribute length exceeds the datagram.
    client.send_to(&invalid, destination).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), client.recv_from(&mut reply))
            .await
            .is_err()
    );
    task.abort();
    let _ = task.await;
}
