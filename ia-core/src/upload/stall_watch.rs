//! Stall detection for an upload body send (#38).
//!
//! The upload transport has no read timeout, on purpose: the read-timeout
//! clock is not reset by request-body writes, so a server that is
//! legitimately silent while a large body uploads would abort the send. The
//! cost is that a server that stops *reading* the body hangs the request for
//! as long as the connection stays open, and no retry ever fires.
//!
//! [`BodyWatch`] closes that gap with one fixed rule: a send that moves no
//! bytes for [`stall::WINDOW`] (60 s) is dead and is abandoned. The body
//! stream reports every chunk it hands to the transport into a
//! [`StallDetector`] with a floor of one byte per second and both window
//! and grace at 60 s, and [`watch_send`] runs the request under a
//! once-a-second ticker that asks it; a dead send's future is dropped,
//! which closes the connection, and the caller retries. There is no rate
//! floor to tune, unlike download's `--min-speed`: a re-send goes to the
//! same endpoint, so "slow" is not a signal of anything fixable, only
//! "dead" is. The clock starts at the first body poll, so a slow connect
//! is not counted. Only the body send is judged: once the last chunk has
//! been handed over, the wait for the server's response is not a stall (a
//! part PUT's answer legitimately arrives seconds after the body, while IA
//! hashes it), and nothing bounds that wait (#40).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::Stream;
use tokio::time::Instant;

use crate::stall::{self, StallDetector};

/// The detector for one attempt's body send, shared between the body
/// stream that feeds it and the ticker that asks it.
#[derive(Debug, Clone)]
pub(crate) struct BodyWatch {
    /// Built at the first body poll, so the clock starts there.
    detector: Arc<Mutex<Option<StallDetector>>>,
    /// Set when the body has been handed over in full; from then on the
    /// send is not judged. With a `Content-Length` set, the transport stops
    /// polling the stream once that many bytes are out and never asks for
    /// its end, so the flag is set by the byte count reaching the length
    /// given to [`wrap`](Self::wrap), and also on the stream's end.
    done: Arc<AtomicBool>,
    /// The body length given to `wrap`.
    expected: Arc<AtomicU64>,
    sent: Arc<AtomicU64>,
    window: std::time::Duration,
}

impl BodyWatch {
    /// A watch for one attempt's body send. The clock starts at the first
    /// body poll, not now: connecting is not judged.
    pub(crate) fn new() -> Self {
        let (window, _grace) = stall::policy();
        Self {
            detector: Arc::new(Mutex::new(None)),
            done: Arc::new(AtomicBool::new(false)),
            expected: Arc::new(AtomicU64::new(u64::MAX)),
            sent: Arc::new(AtomicU64::new(0)),
            window,
        }
    }

    /// Wrap a body stream of `body_len` bytes so every chunk it yields is
    /// credited to the detector, and the judging stops once `body_len`
    /// bytes have been handed over (or the stream ends).
    pub(crate) fn wrap<S>(&self, inner: S, body_len: u64) -> WatchedBody<S> {
        self.expected.store(body_len, Ordering::SeqCst);
        if body_len == 0 {
            self.done.store(true, Ordering::SeqCst);
        }
        WatchedBody {
            inner,
            watch: self.clone(),
        }
    }

    fn record(&self, bytes: u64) {
        if let Ok(mut guard) = self.detector.lock() {
            let now = Instant::now();
            // Floor of one byte per second, grace equal to the window: a
            // send is judged dead when the last window holds no bytes.
            let d =
                guard.get_or_insert_with(|| StallDetector::new(1, self.window, self.window, now));
            d.record(now, bytes);
        }
        let sent = self.sent.fetch_add(bytes, Ordering::SeqCst) + bytes;
        if sent >= self.expected.load(Ordering::SeqCst) {
            self.done.store(true, Ordering::SeqCst);
        }
    }

    /// `Some(window_secs)` when the send is dead: nothing moved over the
    /// window. `None` while nothing has been polled yet, within the first
    /// window, while bytes keep moving, and once the body is done.
    fn check(&self) -> Option<u64> {
        if self.done.load(Ordering::SeqCst) {
            return None;
        }
        let mut guard = self.detector.lock().ok()?;
        let d = guard.as_mut()?;
        d.check(Instant::now()).map(|_observed| d.window_secs())
    }
}

/// A body stream that reports each chunk to its [`BodyWatch`].
pub(crate) struct WatchedBody<S> {
    inner: S,
    watch: BodyWatch,
}

impl<S, E> Stream for WatchedBody<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Bytes, E>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.watch.record(chunk.len() as u64);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(None) => {
                this.watch.done.store(true, Ordering::SeqCst);
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

/// How a watched send ended.
#[derive(Debug)]
pub(crate) enum SendEnd<T> {
    /// The request future finished with this result.
    Done(T),
    /// The body send moved no bytes for `window_secs`; the request was
    /// dropped, closing the connection.
    Stalled { window_secs: u64 },
}

/// Run `send` under `watch`: the future races a once-a-second check of the
/// detector, and a dead send drops the future.
pub(crate) async fn watch_send<F, T>(watch: &BodyWatch, send: F) -> SendEnd<T>
where
    F: Future<Output = T>,
{
    tokio::pin!(send);
    let mut ticker = tokio::time::interval(stall::CHECK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            result = &mut send => return SendEnd::Done(result),
            _ = ticker.tick() => {
                if let Some(window_secs) = watch.check() {
                    return SendEnd::Stalled { window_secs };
                }
            }
        }
    }
}

/// Chunk size for streaming an in-memory body — 64 KiB, as `ProgressBody`
/// uses for files.
const CHUNK_SIZE: usize = 64 * 1024;

/// An in-memory body as a stream of 64 KiB slices, so a part PUT can be
/// watched like a file. Slicing `Bytes` bumps a refcount; nothing is
/// copied.
pub(crate) fn bytes_chunks(
    body: Bytes,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + Unpin + 'static {
    let len = body.len();
    futures::stream::iter((0..len).step_by(CHUNK_SIZE).map(move |start| {
        let end = (start + CHUNK_SIZE).min(len);
        Ok(body.slice(start..end))
    }))
}

/// Test fixtures shared by the stall tests here, in `retry.rs` and in
/// `single.rs`.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    /// A server that accepts, reads the first kilobyte of each request and
    /// then stops reading without closing. Returns its address and a
    /// counter of accepted connections.
    pub(crate) async fn stalling_listener() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        use tokio::io::AsyncReadExt;
        // A small receive buffer, so the client's send blocks after a few
        // hundred kilobytes whatever the host's socket buffer defaults are;
        // the 16 MiB body then cannot be handed over in full.
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(64 * 1024).unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener = socket.listen(16).unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = connections.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = stream.read_exact(&mut buf).await;
                    // Stop reading; hold the connection open.
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    drop(stream);
                });
            }
        });
        (addr, connections)
    }

    /// 16 MiB: larger than the loopback socket buffers, so the send really
    /// stops when the server stops reading.
    pub(crate) fn big_body() -> bytes::Bytes {
        bytes::Bytes::from(vec![0x5au8; 16 * 1024 * 1024])
    }

    /// Window 2 s, grace 1 s: a stalled send is judged in about three
    /// seconds instead of ninety.
    pub(crate) fn shrink_policy() -> crate::stall::PolicyOverride {
        crate::stall::PolicyOverride::new(
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn plain_client() -> reqwest::Client {
        crate::client::configure_transport(
            reqwest::Client::builder(),
            std::time::Duration::from_secs(5),
            None,
        )
        .build()
        .unwrap()
    }

    /// A send whose peer stops reading is judged a stall once the grace has
    /// passed and the window average has fallen below the floor.
    #[tokio::test]
    async fn stalled_send_is_abandoned() {
        let _policy = shrink_policy();
        let (addr, connections) = stalling_listener().await;
        let client = plain_client();
        let watch = BodyWatch::new();
        let body = watch.wrap(bytes_chunks(big_body()), 16 * 1024 * 1024);
        let started = std::time::Instant::now();
        let end = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            watch_send(
                &watch,
                client
                    .put(format!("http://{addr}/item/part"))
                    .header("Content-Length", (16 * 1024 * 1024).to_string())
                    .body(reqwest::Body::wrap_stream(body))
                    .send(),
            ),
        )
        .await
        .expect("the stalled send must be judged within a minute");
        match end {
            SendEnd::Stalled { window_secs } => assert_eq!(window_secs, 2),
            SendEnd::Done(r) => panic!("expected a stall, got {r:?}"),
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(15),
            "judged in {:?}",
            started.elapsed()
        );
        assert_eq!(connections.load(Ordering::SeqCst), 1);
    }

    /// A send the server reads completes; the watch stays quiet.
    #[tokio::test]
    async fn a_send_the_server_reads_completes() {
        let _policy = shrink_policy();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = plain_client();
        let watch = BodyWatch::new();
        let payload = Bytes::from(vec![1u8; 200 * 1024]);
        let body = watch.wrap(bytes_chunks(payload), 200 * 1024);
        let end = watch_send(
            &watch,
            client
                .put(format!("{}/item/part", server.uri()))
                .header("Content-Length", (200 * 1024).to_string())
                .body(reqwest::Body::wrap_stream(body))
                .send(),
        )
        .await;
        match end {
            SendEnd::Done(Ok(resp)) => assert_eq!(resp.status(), 200),
            other => panic!("expected a completed send, got {other:?}"),
        }
        assert!(
            watch.done.load(Ordering::SeqCst),
            "the body end was recorded"
        );
        server.verify().await;
    }

    /// Once the body has been handed over, waiting for the response is not
    /// judged: a slow server that has read everything is not a stall.
    #[tokio::test]
    async fn a_finished_body_is_not_judged_while_waiting_for_the_response() {
        let _policy = shrink_policy();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(3500)),
            )
            .mount(&server)
            .await;
        let client = plain_client();
        let watch = BodyWatch::new();
        let body = watch.wrap(bytes_chunks(Bytes::from_static(b"tiny")), 4);
        let end = watch_send(
            &watch,
            client
                .put(format!("{}/x", server.uri()))
                .header("Content-Length", "4")
                .body(reqwest::Body::wrap_stream(body))
                .send(),
        )
        .await;
        assert!(
            matches!(end, SendEnd::Done(Ok(_))),
            "a slow response after a finished body is not a stall: {end:?}"
        );
    }

    /// The clock starts at the first body poll, not when the watch is
    /// built: a slow connect and TLS handshake are not a stall. With nothing
    /// polled yet, there is nothing to judge.
    #[tokio::test]
    async fn the_clock_starts_at_the_first_body_poll() {
        let _policy = shrink_policy();
        let watch = BodyWatch::new();
        let _body = watch.wrap(bytes_chunks(Bytes::from_static(b"x")), 1);
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert!(
            watch.check().is_none(),
            "nothing was polled yet, so nothing is judged"
        );
        // The first poll starts the clock; the grace runs from here.
        watch.record(0);
        assert!(watch.check().is_none(), "within the first window");
        tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
        assert!(
            watch.check().is_some(),
            "a whole window with no bytes: a dead send"
        );
    }

    #[test]
    fn bytes_chunks_slices_without_losing_bytes() {
        let data: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let chunks: Vec<Bytes> =
            futures::executor::block_on(futures::StreamExt::collect::<Vec<_>>(
                futures::StreamExt::map(bytes_chunks(Bytes::from(data.clone())), |r| r.unwrap()),
            ));
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].len(), CHUNK_SIZE);
        let joined: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
        assert_eq!(joined, data);
        let none: Vec<_> = futures::executor::block_on(futures::StreamExt::collect::<Vec<_>>(
            bytes_chunks(Bytes::new()),
        ));
        assert!(none.is_empty());
    }
}
