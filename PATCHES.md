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
the executor if that callback is already cancelled, except for ordinary local
cancellation. Responses are unchanged.

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

## Ordinary background ends (issue #63 regression)

A background result means failed work only when the task's purpose failed.
The client upload adapter's `upload_result` is shared by eager and pending
pipes: a peer's NO_ERROR stop is ordinary early-response upload termination,
as the upstream Incoming body already treats that reason. Other resets and
user-body errors retain their original error. The test distinguishes a
NO_ERROR coming from a user body from the upload pipe's own stop.
`Callback::send_result` retains genuine undelivered errors but maps local
cancellation to Ok; the h2 check uses the error kind and reset initiator,
never a reset string or every CANCEL.

Regression evidence: `tests/task_results.rs` checks a complete response
followed by NO_ERROR, INTERNAL_ERROR retained once, and local cancellation
before headers and after headers. The callback unit regression covers both
retry and non-retry callbacks, cancellation and a genuine undelivered body
failure. Existing connection, eager/pending user-body and once-only request
delivery regressions remain. `cargo test -p hyper --features full --locked`
passes on macOS arm64 with the matching hyper-util test workspace.

The h2 connection's internal task and public dispatcher also share one error
slot. `ClientTask::finish_connection` takes the original failure for the public
connection, or releases it to the internal task when the dispatcher is dropped;
the internal task completes Ok after that delivery. This preserves the public
connection's original error without reporting it twice to one executor.
`task_results` checks a driven public connection, direct error delivery without
an executor duplicate, and an observer leaving after the original I/O failure
was published. It never treats dropping an unfailed connection as failure.

An upload can also inherit the connection's h2 I/O/GOAWAY error before the
public dispatcher has consumed it. The client-only upload checkpoint leaves
that connection end to the separately owned connection handoff. It matches
BodyWrite plus a direct h2 connection-error variant, not EOF text, every I/O
cause, or stream resets; user-body I/O and independent reset failures remain.
The pending-upload/failed-connection regression reproduces two diagnostics
before this ownership correction and one afterward, retaining the real
connection error and the already complete response.

The cancelled-observer response handoff applies the same ownership distinction:
`response_task_result` leaves an h2 connection I/O/GOAWAY error to the connection
handoff, while keeping a genuine undelivered stream/request error. The private
ResponseFutMap context and Error kind distinguish that error from a user body's
own I/O error; the generic executor has no EOF/reset filter. Its unit regression
covers connection, REFUSED_STREAM and user-body I/O outcomes. Boundless's gRPC
peer-loss regression requires the one original-code/cause warning after close.
