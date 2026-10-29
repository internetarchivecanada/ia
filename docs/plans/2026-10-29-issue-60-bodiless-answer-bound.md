# Every IA-S3 request has an answer bound (#60)

**The gap.** `send_with_retry` in `ia-core/src/upload/retry.rs` runs each attempt under a `BodyWatch` (`ia-core/src/upload/stall_watch.rs`). The watch judges a request only through the body stream it wraps: a send that moves no bytes for 60 s is dead (#38, #42), and once the stream has handed over its last byte the answer must arrive within 120 s (#40). The multipart control requests wrap no stream: initiate (`POST ?uploads`, `Content-Length: 0`), complete (`POST ?uploadId=` with the XML manifest as a plain body), abort (`DELETE`), and the two listings (`GET ?uploads`, `GET ?uploadId=`). Their watch never starts a clock, and the upload transport has no read timeout, so a server that accepts the connection and sends nothing holds the request for as long as the connection stays open.

Since #21 this is reachable without `--multipart`: a single PUT of a file larger than 2 MiB that dies is followed by an initiate to the same host, and against a host that has gone silent the initiate hangs.

**Decision.** A request that wraps no body stream is done sending when it is handed to the transport, and its answer must arrive within the same 120 s (`crate::stall::response_wait()`). `send_with_retry` builds the attempt's request (calling the caller's closure, which wraps a body stream while it builds the request if it has one), and if nothing was wrapped it starts the watch's response wait at once. From there the existing rule applies unchanged: an answer that has not arrived when the wait passes is abandoned (the future is dropped, closing the connection) and the request is re-sent at once, spending one of `--retries`; past the budget the failure is `IaError::UploadStalled` with the request counted as unanswered.

The rule sits in the one place every multipart request goes through, so no call site changes and a future request without a body is bounded by default. The single PUT (`upload::single`) always wraps its body and is unchanged.

**What the clock covers.** The wait runs from the moment the request is handed to the transport, so it includes connecting (bounded on its own by the 30 s connect timeout) and, for the completion request, sending the manifest (at most tens of kilobytes). A zero-length wrapped body already behaves this way: it is done at `wrap`, before the connect. Reading the answer's body after its headers arrive stays unbounded, as for bodied requests; the listing of unfinished uploads at the start of a run keeps its own 120 s bound over all attempts.

**Message wording.** The stall messages say an answer did not come "after the body was sent". With bodiless requests judged by the same rule that is inaccurate for them, so the wording becomes "after the request was sent", which is accurate for both: in `IaError::UploadStalled`'s message (`error::stall_detail`), the warnings in `retry.rs` and `single.rs`, the `--retries` help text in `ia-cli`, and `docs/usage.md`. The JSON fields (`stalls`, `unanswered`, `window_secs`, `response_secs`) do not change.

**Docs.** `docs/usage.md`: the `--retries` row and "Stalled uploads" say every IA-S3 request's answer must come within 120 s after the request was sent, and the sentence that a request with no body has no clock is replaced by what the clock covers for one.

## Tasks
- [x] Plan committed first.
- [x] Red: in `retry.rs`, a bodiless request (`GET`, no body) to a silent loopback listener is abandoned after the (shortened) wait and re-sent once, then fails as `UploadStalled { stalls: 2, unanswered: 2 }` with two connections seen; in `multipart.rs`, the initiate request against the silent listener fails the same way, and `upload_file_multipart` with `retries = 1` fails as stalled at its first request (the resume listing); a `stall_watch.rs` unit test that a watch marked bodiless judges the wait from the mark.
- [x] Green: the bodiless rule in `BodyWatch` and `send_with_retry`; the wording change.
- [x] Docs: `docs/usage.md`, `--retries` help.
- [x] `cargo test --locked -p ia-core -p ia-cli`, `cargo clippy --locked -p ia-core -p ia-cli -- -D warnings`.
- [ ] Code-reviewer pass; PR.
