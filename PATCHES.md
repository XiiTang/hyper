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

## Executor-owned HTTP2 results (issue #63, batch 5)

The h2 connection task and request body pipes now return `crate::Result<()>`
to the existing generic executor instead of erasing errors into `()`. An
eagerly completed pipe is submitted as a completed result through the same
`H2ClientFuture` handoff. The executor traits keep their methods and generic
shape; generic Tokio executors continue to work. Request response tasks return
Ok once their callback accepts an error, and retain an undelivered error for
the executor if that callback is already cancelled. Responses are unchanged.

`cargo test --features full --test task_results` passes four regressions
(exit 0) on macOS arm64: the underlying connection error, synchronous and
asynchronous body errors, and no duplicate for a stream error already delivered
to its request. The async test keeps its successful response through the
later diagnostic. Baseline comparison and full-suite results are recorded
on macOS arm64: all four regressions fail against the original result-erasing
implementation (exit 101), and pass with the patch (exit 0).
`cargo test --features full --locked` passes (exit 0). Strict Clippy remains
blocked by exactly the baseline's diagnostic messages: two in the library
lane and the existing all-target test diagnostics (exit 101). No suppression
or unrelated repair is included.
