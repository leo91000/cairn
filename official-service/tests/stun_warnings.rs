// Isolate the scoped warning subscriber from concurrent STUN listeners that
// intentionally run without a subscriber and share tracing's callsite cache.
use std::time::Duration;
use tokio::net::UdpSocket;

struct WarningCounter(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for WarningCounter {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if *event.metadata().level() == tracing::Level::WARN {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[tokio::test]
async fn repeated_receive_errors_are_counted_without_flooding_logs_and_binding_recovers() {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::prelude::*;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let source = client.local_addr().unwrap();
    drop(client);
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    sender.connect(source).unwrap();
    sender.set_nonblocking(true).unwrap();
    let destination = sender.local_addr().unwrap();
    let socket = UdpSocket::from_std(sender.try_clone().unwrap()).unwrap();
    let warnings = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(WarningCounter(warnings.clone()));
    let status = leo_official_service::stun::Status::default();
    let task = tokio::spawn(
        leo_official_service::stun::serve_with_status(socket, status.clone())
            .with_subscriber(subscriber),
    );
    for count in 1..=3 {
        sender.send(&[0]).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while status.snapshot().receive_errors < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(
        warnings.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "repeated socket errors must not produce a warning every retry"
    );
    let client = UdpSocket::bind(source).await.unwrap();
    let request = [
        0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    ];
    client.send_to(&request, destination).await.unwrap();
    let mut reply = [0; 512];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut reply))
            .await
            .unwrap()
            .unwrap()
            .0,
        32
    );
    assert_eq!(status.snapshot().status, "running");
    task.abort();
    let _ = task.await;
}
