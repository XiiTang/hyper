//! The executor owns errors that were erased before task completion.
use bytes::Bytes;
use http_body::{Body, Frame};
use http_body_util::Empty;
use hyper::{rt::Executor, Request};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use support::TokioIo;
use tokio::sync::{mpsc, oneshot};
mod support;

trait Output {
    fn result(self) -> Option<hyper::Result<()>>;
}
impl Output for () {
    fn result(self) -> Option<hyper::Result<()>> {
        None
    }
}
impl Output for hyper::Result<()> {
    fn result(self) -> Option<hyper::Result<()>> {
        Some(self)
    }
}
#[derive(Clone)]
struct Recording {
    results: mpsc::UnboundedSender<hyper::Result<()>>,
    tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}
impl<F> Executor<F> for Recording
where
    F: Future + Send + 'static,
    F::Output: Output + Send + 'static,
{
    fn execute(&self, future: F) {
        let results = self.results.clone();
        let task = tokio::spawn(async move {
            if let Some(result) = future.await.result() {
                results.send(result).unwrap();
            }
        });
        self.tasks.lock().unwrap().push(task);
    }
}
impl Recording {
    fn new() -> (Self, mpsc::UnboundedReceiver<hyper::Result<()>>) {
        let (results, receiver) = mpsc::unbounded_channel();
        (
            Self {
                results,
                tasks: Default::default(),
            },
            receiver,
        )
    }
    async fn join(&self) {
        loop {
            let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                task.await.unwrap();
            }
        }
    }
}
async fn next(results: &mut mpsc::UnboundedReceiver<hyper::Result<()>>) -> hyper::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), results.recv())
        .await
        .expect("task did not publish its result")
        .expect("executor gone")
}
struct BrokenIo;
impl hyper::rt::Read for BrokenIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::new(
            io::ErrorKind::Other,
            "h2-connection-marker",
        )))
    }
}
impl hyper::rt::Write for BrokenIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
#[tokio::test]
async fn executor_reads_h2_connection_error() {
    let (executor, mut results) = Recording::new();
    let (sender, connection) =
        hyper::client::conn::http2::handshake::<_, _, Empty<Bytes>>(executor.clone(), BrokenIo)
            .await
            .unwrap();
    let error = next(&mut results).await.unwrap_err();
    assert!(format!("{error:?}").contains("h2-connection-marker"));
    drop(sender);
    let _ = connection.await;
    executor.join().await;
}

struct FailingBody(Option<oneshot::Receiver<()>>);
impl Body for FailingBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if let Some(gate) = self.0.as_mut() {
            if Pin::new(gate).poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.0 = None;
        }
        Poll::Ready(Some(Err(io::Error::new(
            io::ErrorKind::Other,
            "body-pipe-marker",
        ))))
    }
}
async fn pipe_error(asynchronous: bool) {
    let (executor, mut results) = Recording::new();
    let (io, peer) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        let mut connection = h2::server::handshake(peer).await.unwrap();
        let mut requests = Vec::new();
        while let Some(request) = connection.accept().await {
            if let Ok((request, mut response)) = request {
                let _ = response.send_response(http::Response::new(()), true);
                requests.push(request);
            }
        }
    });
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(executor.clone(), TokioIo::new(io))
            .await
            .unwrap();
    let worker = tokio::spawn(connection);
    let (release, gate) = oneshot::channel();
    let body = FailingBody(asynchronous.then_some(gate));
    let response = sender.send_request(Request::post("https://fixture.test/").body(body).unwrap());
    let successful_response = if asynchronous {
        let response = response.await.unwrap();
        assert_eq!(response.status(), 200);
        release.send(()).unwrap();
        Some(response)
    } else {
        assert!(response.await.is_err());
        None
    };
    let mut errors = Vec::new();
    loop {
        if let Err(error) = next(&mut results).await {
            let text = format!("{error:?}");
            let found = text.contains("body-pipe-marker");
            errors.push(text);
            if found {
                break;
            }
        }
    }
    // A successful response remains successful after the diagnostic arrives.
    if let Some(response) = successful_response {
        assert_eq!(response.status(), 200);
    }
    drop(sender);
    let _ = worker.await.unwrap();
    executor.join().await;
    server.await.unwrap();
    while let Ok(result) = results.try_recv() {
        if let Err(error) = result {
            errors.push(format!("{error:?}"));
        }
    }
    assert_eq!(
        errors
            .iter()
            .filter(|e| e.contains("body-pipe-marker"))
            .count(),
        1
    );
    // The response/reset error delivered by SendWhen is not reported again.
    assert!(
        !errors.iter().any(|e| e.contains("stream error")),
        "{errors:?}"
    );
}
#[tokio::test]
async fn executor_reads_synchronous_body_pipe_error() {
    pipe_error(false).await;
}
#[tokio::test]
async fn executor_reads_asynchronous_body_pipe_error_without_rewriting_response() {
    pipe_error(true).await;
}

#[tokio::test]
async fn executor_does_not_repeat_an_error_delivered_to_the_request() {
    let (executor, mut results) = Recording::new();
    let (io, peer) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        let mut connection = h2::server::handshake(peer).await.unwrap();
        while let Some(request) = connection.accept().await {
            let (_request, mut response) = request.unwrap();
            response.send_reset(h2::Reason::REFUSED_STREAM);
        }
    });
    let (mut sender, connection) = hyper::client::conn::http2::handshake::<_, _, Empty<Bytes>>(
        executor.clone(),
        TokioIo::new(io),
    )
    .await
    .unwrap();
    let worker = tokio::spawn(connection);
    let error = sender
        .send_request(
            Request::get("https://fixture.test/")
                .body(Empty::<Bytes>::new())
                .unwrap(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("REFUSED_STREAM"));
    assert!(next(&mut results).await.is_ok());
    drop(sender);
    worker.await.unwrap().unwrap();
    executor.join().await;
    server.await.unwrap();
    while let Ok(result) = results.try_recv() {
        assert!(result.is_ok(), "duplicate: {result:?}");
    }
}
