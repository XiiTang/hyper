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

## A request handed to h2 takes its own stream's result (Boundless #354, driver plan v23 Rule 8)

At e158e802, as on upstream master c954d80c, the HTTP/2 client dispatcher
checks `SendRequest::poll_ready` again right after `send_request` has created
a request's stream (`src/proto/h2/client.rs:768-786`). On an error it answers
the request with that connection error, `message: None`, and drops the
stream. A request kept while its stream is pending open is dropped unpiped
when the loop's top readiness check returns (`:682-700`), so it ends
`DispatchGone`. h2 reports the connection's error first (`streams.rs:1075`
at h2 7b547e04), and a graceful GOAWAY sets that error for every stream
although it fails only the streams above its last stream id
(`streams.rs:800-816`). A stream the GOAWAY counts, whose HEADERS may have
been sent and processed, thus took a remote NO_ERROR GOAWAY, the fact that
marks a request as unprocessed and lets a caller resend it, or lost its
result. grpc-node attributes in the same way as this patch: a created
stream's status comes from its own close (`subchannel-call.ts:240-300`),
never from the session's `goaway` (`transport.ts:205-227`).

The dispatcher now checks readiness only at its loop's top. A request handed
to h2 is kept in `fut_ctx` until that check is ready or failed, and is then
piped whatever the result, so it takes its own stream's result: a counted
stream its response, a stream above the last stream id h2's GOAWAY error.
`FutCtx` is created only into `fut_ctx`, which is taken only there.

Regression evidence: the `proto::h2::client::tests` unit tests
`counted_stream_keeps_its_result_when_goaway_precedes_the_readiness_check`
and `counted_stream_keeps_its_result_when_goaway_arrives_while_it_waits_to_open`.
An h2 server sends a graceful GOAWAY that counts the stream and acknowledges
when the client has read it. The first holds the dispatcher, on its own
thread, between `send_request` and the readiness check until then, through a
`#[cfg(test)]` thread-local hook: nothing else runs in that window. The
second polls the dispatcher itself, with `initial_max_send_streams(1)`.
Without the change they fail (exit 101) with `GoAway(NO_ERROR, Remote)` and
`DispatchGone`; with it they pass (exit 0).

## Successful-drain GOAWAY capture (Boundless W5d / #287)

A received GOAWAY can finish the native connection successfully and never enter
an error conversion. The HTTP/2 builder forwards an immutable h2 GoAwayCapture
before handshake through proto::h2::client::new_builder. The public builder
asserts configuration once; only h2's accepted receive entry fills the capture.
No response extension, retry policy or change to the Ok drive result is added.

macOS arm64 regression evidence, code tip
2f5d27ae5916ff419b79e6c8c62cfd4d2c85f0cb, features `full`:
all Cargo commands below use both overrides:
`--config 'patch.crates-io.h2.git="https://github.com/XiiTang/h2"'`
and `--config 'patch.crates-io.h2.rev="db341210890d8e95381c13b49b8356c43da552a4"'`.

- `cargo <overrides> update -p h2 --precise db341210890d8e95381c13b49b8356c43da552a4`: exit 0.
- `cargo <overrides> metadata --features full --format-version 1 --locked`: exit 0.
  Metadata and Cargo.lock each contain one h2 package, version 0.4.19, git SHA
  db341210890d8e95381c13b49b8356c43da552a4. The manifest retains h2 ^0.4.14.
- `cargo <overrides> test --features full --test client http2_graceful_goaway_capture_survives_ok`: exit 0, 1 passed.
- `cargo <overrides> test --features full --test client`: exit 0, 69 passed.
- `cargo <overrides> test --features full --lib`: exit 0, 117 passed, 6 ignored.
- `cargo <overrides> fmt --all --check`: exit 0.

The landing owner approved a mechanical departure from design revision 7:
`--precise 0.4.19` failed with exit 101 because Cargo interpreted it as a git
revision for the patched dependency. The corrected update uses the same full
h2 SHA for the rev override and --precise, then checks package version separately.
No registry-only or path-override run counts as evidence. The isolated test
Cargo.lock is not committed. Existing ignored tests remain the five
client::conn::http2 executor send/sync compile checks and
proto::h1::io::tests::iobuf_write_empty_slice.
