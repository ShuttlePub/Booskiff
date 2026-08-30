//! Content-Length-checked streaming upload body handling.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use axum::body::Body;
use http_body::{Frame, SizeHint};

#[derive(Debug, Clone)]
pub struct ByteCount(Arc<AtomicU64>);

impl ByteCount {
    pub fn load(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CountingBodyError {
    #[error("upload body exceeds its byte limit")]
    LimitExceeded,
    #[error("upload body stream failed: {0}")]
    Inner(#[from] axum::Error),
    #[error("upload body lock was poisoned")]
    Poisoned,
}

impl CountingBodyError {
    #[cfg(test)]
    fn is_limit_exceeded(&self) -> bool {
        matches!(self, Self::LimitExceeded)
    }
}

pub struct CountingBody {
    inner: Mutex<Body>,
    count: ByteCount,
    cap: u64,
    declared: u64,
    failed: bool,
}

impl std::fmt::Debug for CountingBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CountingBody")
            .field("count", &self.count.load())
            .field("cap", &self.cap)
            .field("declared", &self.declared)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl CountingBody {
    pub fn new(inner: Body, cap: u64, declared: u64) -> (Self, ByteCount) {
        let count = ByteCount(Arc::new(AtomicU64::new(0)));
        (
            Self {
                inner: Mutex::new(inner),
                count: count.clone(),
                cap,
                declared,
                failed: false,
            },
            count,
        )
    }
}

impl http_body::Body for CountingBody {
    type Data = axum::body::Bytes;
    type Error = CountingBodyError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.failed {
            return Poll::Ready(None);
        }
        let poll = match self.inner.lock() {
            Ok(mut inner) => Pin::new(&mut *inner).poll_frame(cx),
            Err(_) => return Poll::Ready(Some(Err(CountingBodyError::Poisoned))),
        };
        match poll {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let frame_bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
                    let total =
                        self.count.0.fetch_add(frame_bytes, Ordering::Relaxed) + frame_bytes;
                    if total > self.cap {
                        self.failed = true;
                        return Poll::Ready(Some(Err(CountingBodyError::LimitExceeded)));
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                Poll::Ready(Some(Err(CountingBodyError::Inner(error))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.declared)
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use http_body::Body as _;
    use http_body_util::BodyExt;

    use super::CountingBody;

    #[tokio::test]
    async fn counts_exact_bytes_when_body_fits_cap() {
        // Given
        let (body, count) = CountingBody::new(Body::from("booskiff"), 8, 8);

        // When
        let bytes = body.collect().await.unwrap().to_bytes();

        // Then
        assert_eq!(bytes.as_ref(), b"booskiff");
        assert_eq!(count.load(), 8);
    }

    #[tokio::test]
    async fn returns_error_as_soon_as_count_exceeds_cap() {
        // Given
        let (body, count) = CountingBody::new(Body::from("too large"), 3, 9);

        // When
        let error = body.collect().await.unwrap_err();

        // Then
        assert!(error.is_limit_exceeded());
        assert_eq!(count.load(), 9);
    }

    #[test]
    fn reports_declared_length_as_exact_size_hint() {
        // Given
        let (body, _count) = CountingBody::new(Body::empty(), 7, 42);

        // When
        let hint = body.size_hint();

        // Then
        assert_eq!(hint.lower(), 42);
        assert_eq!(hint.upper(), Some(42));
    }
}
