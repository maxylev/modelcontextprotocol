use tokio::io::{AsyncRead, AsyncReadExt};

/// Read `reader` to EOF, retaining at most `limit` bytes.
///
/// After the limit is reached the pipe keeps being drained (so the child
/// never blocks on a full pipe) but additional bytes are discarded. Returns
/// the retained bytes and whether the stream exceeded the limit.
pub async fn drain_limited<R>(mut reader: R, limit: usize) -> (Vec<u8>, bool)
where
    R: AsyncRead + Unpin,
{
    let mut retained: Vec<u8> = Vec::with_capacity(limit.min(8192));
    let mut truncated = false;
    let mut buffer = [0u8; 8192];

    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let room = limit.saturating_sub(retained.len());
        if room > 0 {
            let take = read.min(room);
            retained.extend_from_slice(&buffer[..take]);
            if take < read {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }

    (retained, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retains_output_up_to_limit() {
        let (bytes, truncated) = drain_limited(&b"hello"[..], 1024).await;
        assert_eq!(bytes, b"hello");
        assert!(!truncated);
    }

    #[tokio::test]
    async fn truncates_at_limit_and_keeps_draining() {
        let (bytes, truncated) = drain_limited(&b"abcdef"[..], 4).await;
        assert_eq!(bytes, b"abcd");
        assert!(truncated);
    }

    #[tokio::test]
    async fn drains_past_the_limit() {
        // 100 KiB of data with a 4 KiB limit: must return quickly with
        // exactly the first 4 KiB retained.
        let data = vec![b'x'; 100 * 1024];
        let (bytes, truncated) = drain_limited(data.as_slice(), 4 * 1024).await;
        assert_eq!(bytes.len(), 4 * 1024);
        assert!(truncated);
    }

    #[tokio::test]
    async fn exact_limit_is_not_truncated() {
        let (bytes, truncated) = drain_limited(&b"abcd"[..], 4).await;
        assert_eq!(bytes, b"abcd");
        assert!(!truncated);
    }

    #[tokio::test]
    async fn empty_stream_is_not_truncated() {
        let (bytes, truncated) = drain_limited(&b""[..], 1024).await;
        assert!(bytes.is_empty());
        assert!(!truncated);
    }
}
