use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::Stream;
use tokio::io::{AsyncRead, ReadBuf};

/// Chunk size for reading the file body — 64 KiB balances progress
/// granularity against throughput overhead.
const CHUNK_SIZE: usize = 64 * 1024;

/// A stream wrapper that reads from an `AsyncRead` source and invokes a
/// callback with cumulative bytes sent after each chunk.
///
/// Designed for use with `reqwest::Body::wrap_stream()` to provide
/// byte-level upload progress without buffering the entire file.
///
/// `Content-Length` must be set explicitly on the request — IA S3 does not
/// support chunked transfer encoding, and `wrap_stream` would otherwise
/// default to chunked. Setting Content-Length overrides that behavior.
pub(crate) struct ProgressBody<R, F> {
    reader: R,
    callback: F,
    bytes_sent: u64,
    buf: Vec<u8>,
}

impl<R, F> ProgressBody<R, F>
where
    R: AsyncRead + Unpin,
    F: Fn(u64),
{
    pub fn new(reader: R, callback: F) -> Self {
        Self {
            reader,
            callback,
            bytes_sent: 0,
            buf: vec![0u8; CHUNK_SIZE],
        }
    }
}

impl<R, F> Stream for ProgressBody<R, F>
where
    R: AsyncRead + Unpin,
    F: Fn(u64) + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let mut read_buf = ReadBuf::new(&mut this.buf);
        match Pin::new(&mut this.reader).poll_read(cx, &mut read_buf) {
            Poll::Ready(Ok(())) => {
                let n = read_buf.filled().len();
                if n == 0 {
                    return Poll::Ready(None); // EOF
                }
                this.bytes_sent += n as u64;
                (this.callback)(this.bytes_sent);
                Poll::Ready(Some(Ok(Bytes::copy_from_slice(&this.buf[..n]))))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    /// Verify that the callback fires with accurate cumulative byte counts
    /// and that the total bytes match the input data after full consumption.
    #[tokio::test]
    async fn reports_cumulative_bytes_accurately() {
        // 100 KiB — larger than CHUNK_SIZE (64 KiB), so we get multiple chunks.
        let data = vec![42u8; 100_000];

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&data).unwrap();
        tmp.flush().unwrap();

        let file = tokio::fs::File::open(tmp.path()).await.unwrap();

        let reported: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let reported_clone = Arc::clone(&reported);

        let mut stream = ProgressBody::new(file, move |bytes_sent| {
            reported_clone.lock().unwrap().push(bytes_sent);
        });

        // Consume the stream fully.
        let mut total_bytes = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            total_bytes += chunk.len() as u64;
        }

        let calls = reported.lock().unwrap();

        // Total bytes consumed must equal the data length.
        assert_eq!(total_bytes, data.len() as u64);

        // At least two callbacks (100 KiB > one 64 KiB chunk).
        assert!(calls.len() >= 2, "expected multiple callbacks, got {}", calls.len());

        // Each callback must report a strictly increasing cumulative count.
        for window in calls.windows(2) {
            assert!(
                window[1] > window[0],
                "cumulative bytes must be strictly increasing: {} -> {}",
                window[0],
                window[1],
            );
        }

        // The last reported value must equal the total data length.
        assert_eq!(*calls.last().unwrap(), data.len() as u64);
    }

    /// Verify correct behavior with data smaller than one chunk.
    #[tokio::test]
    async fn single_chunk_small_file() {
        let data = vec![7u8; 1000]; // 1 KiB, well under CHUNK_SIZE

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&data).unwrap();
        tmp.flush().unwrap();

        let file = tokio::fs::File::open(tmp.path()).await.unwrap();

        let reported: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let reported_clone = Arc::clone(&reported);

        let mut stream = ProgressBody::new(file, move |bytes_sent| {
            reported_clone.lock().unwrap().push(bytes_sent);
        });

        let mut total_bytes = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            total_bytes += chunk.len() as u64;
        }

        let calls = reported.lock().unwrap();
        assert_eq!(total_bytes, 1000);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], 1000);
    }

    /// Verify empty file produces no callbacks and zero bytes.
    #[tokio::test]
    async fn empty_file_no_callbacks() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Empty file — no writes.

        let file = tokio::fs::File::open(tmp.path()).await.unwrap();

        let reported: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let reported_clone = Arc::clone(&reported);

        let mut stream = ProgressBody::new(file, move |bytes_sent| {
            reported_clone.lock().unwrap().push(bytes_sent);
        });

        let mut total_bytes = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            total_bytes += chunk.len() as u64;
        }

        let calls = reported.lock().unwrap();
        assert_eq!(total_bytes, 0);
        assert!(calls.is_empty());
    }

    /// Verify data integrity — bytes read from the stream match the original.
    #[tokio::test]
    async fn data_integrity_preserved() {
        let data: Vec<u8> = (0..=255).cycle().take(200_000).collect();

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&data).unwrap();
        tmp.flush().unwrap();

        let file = tokio::fs::File::open(tmp.path()).await.unwrap();

        let mut stream = ProgressBody::new(file, |_| {});

        let mut collected = Vec::new();
        while let Some(chunk) = stream.next().await {
            collected.extend_from_slice(&chunk.unwrap());
        }

        assert_eq!(collected, data);
    }
}
