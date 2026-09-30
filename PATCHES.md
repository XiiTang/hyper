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
