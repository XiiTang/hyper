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
and `--config 'patch.crates-io.h2.rev="ba6b4e6e89849c480c7b1c5312aff539d0f1b658"'`.

- `cargo <overrides> update -p h2 --precise ba6b4e6e89849c480c7b1c5312aff539d0f1b658`: exit 0.
- `cargo <overrides> metadata --features full --format-version 1 --locked`: exit 0.
  Metadata and Cargo.lock each contain one h2 package, version 0.4.19, git SHA
  ba6b4e6e89849c480c7b1c5312aff539d0f1b658. The manifest retains h2 ^0.4.14.
- `cargo <overrides> test --features full --test client http2_graceful_goaway_capture_survives_ok`: exit 0, 1 passed.
- `cargo <overrides> test --features full --test client`: exit 0, 69 passed.
- `cargo <overrides> test --features full --lib`: exit 0, 117 passed, 6 ignored.
- `cargo <overrides> fmt --all --check`: exit 1, baseline src/ext/mod.rs re-export ordering (see correction below).

The landing owner approved a mechanical departure from design revision 7:
`--precise 0.4.19` failed with exit 101 because Cargo interpreted it as a git
revision for the patched dependency. The corrected update uses the same full
h2 SHA for the rev override and --precise, then checks package version separately.
No registry-only or path-override run counts as evidence. The isolated test
Cargo.lock is not committed. Existing ignored tests remain the five
client::conn::http2 executor send/sync compile checks and
proto::h1::io::tests::iobuf_write_empty_slice.

Pinned-tree formatting correction: a later clean-tree audit found that the
previous fmt pass included an unstaged formatter-only change to src/ext/mod.rs.
That unrelated change has been restored to the pinned source. On code tip
31c22edacb4895e400bf8e45f23617f5f1576a52, the required fmt --all --check with both
h2 overrides exits 1: the current rustfmt reorders informational re-exports at
lines 57-65. The same lines are unchanged from base 60bb53e5, so this is a
baseline formatting mismatch, not W5d behavior. The earlier fmt pass is not a
pass for the pinned tree. No out-of-scope formatting repair is included.
The earlier GOAWAY/client/lib results remain historical code evidence. The
final rerun below uses the final h2 work tip. This evidence correction changes
no Rust.

Final h2 tip verification on macOS arm64, hyper source tip
fa17d21963fd744c640535d1f2229c3f33d0cb61 (no Rust changes): all six commands
listed above were rerun with h2 ba6b4e6e89849c480c7b1c5312aff539d0f1b658.
Update, metadata, successful-drain test (1 passed), client tests (69 passed),
and library tests (117 passed, six ignored) exited 0. Formatting exited 1 at
unchanged src/ext/mod.rs:57-65. Metadata and lock independently confirm exactly
one h2 package, version 0.4.19, at that full SHA.
`build-slot cargo clippy <overrides> --features full --lib --tests --locked -- -D warnings`
(with both config arguments after the external clippy subcommand) exited 101.
The exact baseline diagnostic files are inventoried below. The landing owner excludes these unchanged baseline
Clippy and formatting failures from W5d gates. No unrelated fixes, lint waiver
or issue is included. Every Cargo command used both h2 overrides and build-slot.

Baseline diagnostic file inventory: `src/body/incoming.rs`, `src/client/conn/http2.rs`, `src/client/dispatch.rs`, `src/common/date.rs`, `src/common/io/compat.rs`, `src/error.rs`, `src/ext/h1_reason_phrase.rs`, `src/ext/mod.rs`, `src/headers.rs`, `src/lib.rs`, `src/proto/h1/conn.rs`, `src/proto/h1/decode.rs`, `src/proto/h1/dispatch.rs`, `src/proto/h1/encode.rs`, `src/proto/h1/io.rs`, `src/proto/h1/role.rs`, `src/proto/h2/client.rs`, `src/upgrade.rs`.

Final audited h2 tip after its evidence-only commit: all seven commands above
(update, metadata, drain test, client tests, library tests, fmt and strict Clippy)
were rerun on macOS arm64 at hyper source tip
fa17d21963fd744c640535d1f2229c3f33d0cb61, with both h2 overrides set to
ba6b4e6e89849c480c7b1c5312aff539d0f1b658. Update/metadata/drain/client/lib exit 0;
drain 1 passed, client 69 passed, library 117 passed and six ignored. Formatting
exits 1 in unchanged src/ext/mod.rs; strict Clippy exits 101 in the baseline
files inventoried above. The owner excludes these unchanged-code failures from
the W5d gate; no repair or issue is included. Independent metadata and lock
inspection exits 0 and confirms exactly one h2 package, version 0.4.19, at that
full SHA. The approved mechanical departure uses this same full SHA for
`--precise` and `patch.crates-io.h2.rev`; Cargo interprets a git-patched
`--precise` as a revision, so `--precise 0.4.19` is not runnable. No h2 commit
followed this verification.


## W5d fix8 readiness retention — WIP, blocked before native regressions

The native readiness error must be retained before dispatcher destruction, at
ClientTask::poll's existing h2 readiness Err branch after the ping-timeout guard.
The reviewed CloseReason latch has one private first writer and the public
SendRequest closed-readiness path is its sole reader; cloned senders share the
fixed association. No request scheduling, counted-stream result handling,
HTTP/1 path, timeout precedence or public signatures change.

Source committed as da4b62d0 (descendant of 93775f0), macOS arm64. Exact fix8
representation Arc<Mutex<Option<h2::Error>>> requires cloning h2::Error in both
remember and error. Pinned h2 ba6b4e6e89849c480c7b1c5312aff539d0f1b658 does not
implement Clone (its private Kind includes io::Error). The required new native
regression http2_poll_ready_retains_received_goaway therefore does not run:
`build-slot cargo --config 'patch.crates-io.h2.git="https://github.com/XiiTang/h2"' --config 'patch.crates-io.h2.rev="ba6b4e6e89849c480c7b1c5312aff539d0f1b658"' test --features full --test client http2_poll_ready_retains_received_goaway --locked`
exits 101 with E0599 at src/proto/h2/client.rs:43 (Option<h2::Error>: Clone
unsatisfied) and E0308 at :49 (expected h2::Error, found &h2::Error).
The real stream1/accepted-frame fixture includes nonzero and NO_ERROR GOAWAY,
cloned-sender readiness and no-received-frame local-drop controls; its assertions
are unverified until this compile blocker is resolved. No drive-error substitute,
error reconstruction, h2 patch or alternative representation has been invented.
This WIP tip must not replace the last buildable Boundless pin before review.

The same two h2 overrides were used for `build-slot cargo ... fmt --all`, exit 0.
Its unrelated src/ext/mod.rs baseline formatting change was restored. Existing
baseline fmt/Clippy exclusions and previous native regression results above remain
historical evidence, not a passing gate for this WIP. Required counted-stream,
capture-survives-Ok, client/lib and strict checks remain blocked by compilation.
No h2, lapin, fork boundless branch or Boundless dependency pin changed in fix8.
