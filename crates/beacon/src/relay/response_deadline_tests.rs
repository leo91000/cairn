use super::{
    ApiResponse, IntoResponse, REQUEST_TIMEOUT, StatusCode, installation_response, oneshot,
};

fn response() -> ApiResponse {
    ApiResponse {
        id: "deadline-fixture".into(),
        status: 200,
        headers: Vec::new(),
        body: Vec::new(),
    }
}

#[tokio::test(start_paused = true)]
async fn an_expired_request_deadline_wins_over_a_closed_response_channel() {
    let (reply, receiver) = oneshot::channel();
    let waiting = installation_response(receiver);
    tokio::pin!(waiting);
    assert!(futures_util::poll!(&mut waiting).is_pending());

    tokio::time::advance(REQUEST_TIMEOUT).await;
    drop(reply);

    assert_eq!(
        waiting.await.err().unwrap().into_response().status(),
        StatusCode::GATEWAY_TIMEOUT
    );
}

#[tokio::test(start_paused = true)]
async fn an_expired_request_deadline_wins_over_a_ready_response() {
    let (reply, receiver) = oneshot::channel();
    let waiting = installation_response(receiver);
    tokio::pin!(waiting);
    assert!(futures_util::poll!(&mut waiting).is_pending());

    tokio::time::advance(REQUEST_TIMEOUT).await;
    assert!(reply.send(response()).is_ok());

    assert_eq!(
        waiting.await.err().unwrap().into_response().status(),
        StatusCode::GATEWAY_TIMEOUT
    );
}

#[tokio::test(start_paused = true)]
async fn a_pending_response_times_out_at_the_request_deadline() {
    let (_reply, receiver) = oneshot::channel();

    assert_eq!(
        installation_response(receiver)
            .await
            .err()
            .unwrap()
            .into_response()
            .status(),
        StatusCode::GATEWAY_TIMEOUT
    );
}

#[tokio::test(start_paused = true)]
async fn a_response_before_the_deadline_succeeds() {
    let (reply, receiver) = oneshot::channel();
    assert!(reply.send(response()).is_ok());

    assert_eq!(
        installation_response(receiver).await.ok().unwrap().status,
        200
    );
}

#[tokio::test(start_paused = true)]
async fn a_connection_lost_before_the_deadline_remains_a_bad_gateway() {
    let (reply, receiver) = oneshot::channel();
    drop(reply);

    assert_eq!(
        installation_response(receiver)
            .await
            .err()
            .unwrap()
            .into_response()
            .status(),
        StatusCode::BAD_GATEWAY
    );
}
