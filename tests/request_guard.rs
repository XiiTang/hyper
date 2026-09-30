//! A queued request must recheck its guard in the dispatcher, and H2 rejection
//! must leave the connection usable for an independently authorized request.
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response};
use std::convert::Infallible;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use support::{TokioExecutor, TokioIo};
mod support;

#[tokio::test]
async fn h1_dispatch_refuses_a_guard_revoked_after_enqueue() {
    let (io, mut peer) = tokio::io::duplex(4096);
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .unwrap();
    let authorized = Arc::new(AtomicBool::new(true));
    let state = authorized.clone();
    let mut request = Request::new(Full::new(Bytes::new()));
    request
        .extensions_mut()
        .insert(hyper::ext::RequestGuard::new(move || {
            if state.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(std::io::Error::other("authorization expired"))
            }
        }));
    let response = sender.send_request(request);
    authorized.store(false, Ordering::SeqCst);
    let worker = tokio::spawn(connection);
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), response)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.is_user());
    drop(sender);
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).await.unwrap();
    assert!(bytes.is_empty());
    let _ = worker.await;
}

#[tokio::test]
async fn h2_rejected_request_has_no_peer_effect_and_does_not_close_the_connection() {
    let (io, peer) = tokio::io::duplex(4096);
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let server = tokio::spawn(async move {
        hyper::server::conn::http2::Builder::new(TokioExecutor)
            .serve_connection(
                TokioIo::new(peer),
                hyper::service::service_fn(move |_| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    async { Ok::<_, Infallible>(Response::new(Full::new(Bytes::new()))) }
                }),
            )
            .await
    });
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor, TokioIo::new(io))
            .await
            .unwrap();
    let mut request = Request::builder()
        .uri("https://fixture.test/")
        .body(Full::new(Bytes::new()))
        .unwrap();
    request
        .extensions_mut()
        .insert(hyper::ext::RequestGuard::new(|| {
            Err::<(), _>(std::io::Error::other("authorization expired"))
        }));
    let response = sender.send_request(request);
    let worker = tokio::spawn(connection);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), response)
            .await
            .unwrap()
            .unwrap_err()
            .is_user()
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    let request = Request::builder()
        .uri("https://fixture.test/")
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(sender.send_request(request).await.unwrap().status(), 200);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    worker.abort();
    server.abort();
}
