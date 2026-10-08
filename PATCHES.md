# Maintained transport patches

- Retain upstream history and the existing half-close and buffered-flush fixes.
- `ext::RequestGuard` carries a caller-owned, per-request authorization check.
  The HTTP/1 and HTTP/2 client dispatchers run it after connection/readiness
  waits and before handing request headers to the protocol encoder. Rejection
  reports the original error through its source without sending the request.
  It does not authorize retry, inspect responses, or govern existing tunnels.
- Regression evidence: `tests/request_guard.rs` covers revocation after enqueue,
  absence of an HTTP/1 request on the wire, and HTTP/2 rejection without a peer
  operation or loss of the connection's next independently authorized request.

## Connection header preservation (2026-10-02)

Backport hyper #4196, 74a39081ff1496970fb75da75c5282b23a9b69e5, preserving
existing Connection tokens when adding close/keep-alive. Retain the dispatch
guard, half-close and bounded flush changes. `cargo test --features full`
passes on macOS arm64, including upstream exact-wire tests.

## Cancel a request body when its response body is dropped (2026-10-08)

hyper #4042 resets an HTTP/2 stream when the caller drops the response future
before the headers arrive. Once they have arrived, a request body still
waiting for flow-control capacity kept its stream open: the body pipe polls
neither the body nor any local signal while a chunk waits for capacity, so a
peer that never reads (a gRPC client-streaming call the caller cancelled)
never learned of the cancellation and the stream held its capacity until the
connection closed. The response body now takes over #4042's cancel sender
and, dropped before its end, tells the pipe to reset the stream with CANCEL.
`Incoming` grows by one word, so `test_size_of` allows six.
Regression evidence: `tests/client.rs`
`h2_pipe_task_cancelled_on_response_body_drop` fails without the change and
passes with it; `cargo test --features full` passes on macOS arm64.
