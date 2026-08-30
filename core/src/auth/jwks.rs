//! JWKS fetching and key caching per trusted issuer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::DecodingKey;
use jsonwebtoken::jwk::{AlgorithmParameters, JwkSet};
use tokio::sync::Mutex;

use crate::config::TrustedIssuer;
use crate::error::AppError;

/// How long a fetched JwkSet is served from cache before the next
/// on-demand refresh (Emumet-style minimum refresh interval: issuers rotate
/// keys rarely, so this bounds both staleness and upstream fetch load).
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Minimum spacing between kid-miss ("forced") refreshes for one issuer.
/// Without this cooldown an attacker sending unknown kids could turn every
/// request into an upstream JWKS fetch (refresh storm).
const FORCED_REFRESH_COOLDOWN: Duration = Duration::from_secs(60);

/// Upper bound for one outgoing JWKS fetch (a hung issuer must not hang
/// request handlers indefinitely).
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Cached state for one trusted issuer.
///
/// Interior mutability choice: the `std::sync::RwLock` serves the read
/// path (cache reads never await, so a plain lock avoids task-thread
/// parking), while the `tokio::sync::Mutex` serializes refreshes so
/// concurrent misses for one issuer collapse into a single JWKS fetch
/// (single-flight).
struct IssuerSlot {
    cache: std::sync::RwLock<CacheEntry>,
    refresh: Mutex<()>,
}

/// All fields are `None` until the first successful fetch.
#[derive(Default)]
struct CacheEntry {
    jwks: Option<JwkSet>,
    fetched_at: Option<Instant>,
    forced_refresh_at: Option<Instant>,
}

impl IssuerSlot {
    fn read<T>(&self, with: impl FnOnce(&CacheEntry) -> T) -> T {
        let entry = self
            .cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        with(&entry)
    }

    /// The cached set regardless of freshness (stale kids still resolve;
    /// the TTL only gates `jwks_for` reads and background re-fetches).
    fn jwks(&self) -> Option<JwkSet> {
        self.read(|entry| entry.jwks.clone())
    }

    /// The cached set when it was fetched within [`CACHE_TTL`].
    fn fresh_jwks(&self) -> Option<JwkSet> {
        self.read(|entry| match (&entry.jwks, entry.fetched_at) {
            (Some(jwks), Some(fetched_at)) if fetched_at.elapsed() < CACHE_TTL => {
                Some(jwks.clone())
            }
            _ => None,
        })
    }

    /// Whether a forced refresh ran within [`FORCED_REFRESH_COOLDOWN`].
    fn forced_refresh_recent(&self) -> bool {
        self.read(|entry| {
            entry
                .forced_refresh_at
                .is_some_and(|at| at.elapsed() < FORCED_REFRESH_COOLDOWN)
        })
    }

    fn note_forced_refresh(&self) {
        let mut entry = self
            .cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entry.forced_refresh_at = Some(Instant::now());
    }

    fn store(&self, jwks: JwkSet) {
        let mut entry = self
            .cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entry.jwks = Some(jwks);
        entry.fetched_at = Some(Instant::now());
    }
}

/// Cache of JWKS keys per trusted issuer. Cheap to clone: every clone
/// shares the same per-issuer slots and HTTP client.
#[derive(Clone)]
pub struct JwksCache {
    issuers: Arc<[TrustedIssuer]>,
    /// Parallel to `issuers` (same index = same issuer).
    slots: Arc<[Arc<IssuerSlot>]>,
    client: reqwest::Client,
}

impl JwksCache {
    pub fn new(issuers: Vec<TrustedIssuer>) -> Self {
        let slots = issuers
            .iter()
            .map(|_| {
                Arc::new(IssuerSlot {
                    cache: std::sync::RwLock::new(CacheEntry::default()),
                    refresh: Mutex::new(()),
                })
            })
            .collect::<Vec<_>>();
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .unwrap_or_else(|err| panic!("failed to build JWKS http client: {err}"));
        Self {
            issuers: Arc::from(issuers),
            slots: Arc::from(slots),
            client,
        }
    }

    pub fn issuers(&self) -> &[TrustedIssuer] {
        &self.issuers
    }

    fn find(&self, issuer: &str) -> Result<(&TrustedIssuer, &IssuerSlot), AppError> {
        self.issuers
            .iter()
            .zip(self.slots.iter())
            .find(|(trusted, _)| trusted.issuer == issuer)
            .map(|(trusted, slot)| (trusted, slot.as_ref()))
            .ok_or_else(|| AppError::Unauthorized("untrusted issuer".into()))
    }

    /// The (cached) JW document for a known issuer. Unknown issuers are
    /// rejected without any outgoing request.
    pub async fn jwks_for(&self, issuer: &str) -> Result<JwkSet, AppError> {
        let (trusted, slot) = self.find(issuer)?;
        if let Some(jwks) = slot.fresh_jwks() {
            return Ok(jwks);
        }
        // Single-flight: a concurrent task may refresh while we wait on
        // the mutex, so re-check freshness after acquiring the guard.
        let _guard = slot.refresh.lock().await;
        if let Some(jwks) = slot.fresh_jwks() {
            return Ok(jwks);
        }
        let jwks = self.fetch_jwks(&trusted.jwks_url).await?;
        slot.store(jwks.clone());
        Ok(jwks)
    }

    /// The decoding key for `kid` as published by `issuer`. A `kid` miss
    /// on a live cache triggers at most one forced refresh per
    /// [`FORCED_REFRESH_COOLDOWN`] window (per issuer) to prevent refresh
    /// storms; a cold or expired cache first gets a regular refresh.
    pub async fn key_for(&self, issuer: &str, kid: &str) -> Result<DecodingKey, AppError> {
        let (trusted, slot) = self.find(issuer)?;
        if let Some(key) = slot.jwks().and_then(|jwks| decoding_key(&jwks, kid)) {
            return Ok(key);
        }
        // Single-flight: a concurrent task may refresh while we wait on
        // the mutex, so re-check the kid after acquiring the guard.
        let _guard = slot.refresh.lock().await;
        if let Some(key) = slot.jwks().and_then(|jwks| decoding_key(&jwks, kid)) {
            return Ok(key);
        }
        // Cold or expired cache: a regular refresh. It does not consume
        // the forced-refresh cooldown — a cold start must not suppress
        // the kid-miss probes below (e.g. key rotation checks).
        if slot.fresh_jwks().is_none() {
            let jwks = self.fetch_jwks(&trusted.jwks_url).await?;
            slot.store(jwks.clone());
            if let Some(key) = decoding_key(&jwks, kid) {
                return Ok(key);
            }
            // The set was fetched moments ago: a forced re-fetch would
            // return the same keys, so this miss is final — but it still
            // stamps the cooldown so repeated misses do not re-fetch.
            slot.note_forced_refresh();
            return Err(AppError::Unauthorized(format!(
                "unknown kid {kid:?} for issuer {issuer:?}"
            )));
        }
        // Kid miss on a live cache: at most one forced refresh per
        // cooldown window (per issuer) to prevent refresh storms.
        if slot.forced_refresh_recent() {
            return Err(AppError::Unauthorized(format!(
                "unknown kid {kid:?} for issuer {issuer:?}"
            )));
        }
        // Stamp the cooldown before (not after) fetching so a failing or
        // slow JWKS endpoint is shielded from refresh storms too.
        slot.note_forced_refresh();
        let jwks = self.fetch_jwks(&trusted.jwks_url).await?;
        slot.store(jwks.clone());
        decoding_key(&jwks, kid).ok_or_else(|| {
            AppError::Unauthorized(format!("unknown kid {kid:?} for issuer {issuer:?}"))
        })
    }

    async fn fetch_jwks(&self, jwks_url: &str) -> Result<JwkSet, AppError> {
        self.client
            .get(jwks_url)
            .send()
            .await
            .and_then(|response| response.error_for_status())
            .map_err(|err| AppError::Internal(format!("jwks fetch {jwks_url} failed: {err}")))?
            .json::<JwkSet>()
            .await
            .map_err(|err| AppError::Internal(format!("jwks parse {jwks_url} failed: {err}")))
    }
}

/// Build a decoding key for `kid` from the set. A matching non-RSA key
/// counts as not found (only RS256 tokens are accepted downstream).
fn decoding_key(jwks: &JwkSet, kid: &str) -> Option<DecodingKey> {
    let jwk = jwks
        .keys
        .iter()
        .find(|jwk| jwk.common.key_id.as_deref() == Some(kid))?;
    match &jwk.algorithm {
        AlgorithmParameters::RSA(params) => {
            DecodingKey::from_rsa_components(&params.n, &params.e).ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as SharedArc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::routing::get;

    const TEST_ISSUER: &str = "https://issuer.test";
    const JWKS_FIXTURE: &str = include_str!("../../tests/fixtures/test_only_jwks.json");

    /// Serve the fixture JWKS, counting requests, on a random local port.
    async fn jwks_server() -> (String, SharedArc<AtomicUsize>) {
        let hits = SharedArc::new(AtomicUsize::new(0));
        let counter = SharedArc::clone(&hits);
        let app = axum::Router::new().route(
            "/jwks.json",
            get(move || {
                let counter = SharedArc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    JWKS_FIXTURE
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/jwks.json"), hits)
    }

    fn cache_with(jwks_url: &str) -> JwksCache {
        JwksCache::new(vec![TrustedIssuer {
            issuer: TEST_ISSUER.into(),
            jwks_url: jwks_url.into(),
        }])
    }

    #[tokio::test]
    async fn unknown_issuer_is_rejected_without_fetching() {
        let (url, hits) = jwks_server().await;
        let cache = cache_with(&url);
        let err = cache.jwks_for("https://evil.test").await.unwrap_err();
        assert!(
            matches!(err, AppError::Unauthorized(message) if message.contains("untrusted issuer"))
        );
        let err = cache
            .key_for("https://evil.test", "test-only-key-1")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn jwks_for_fetches_once_then_serves_from_cache() {
        let (url, hits) = jwks_server().await;
        let cache = cache_with(&url);
        let first = cache.jwks_for(TEST_ISSUER).await.unwrap();
        let second = cache.jwks_for(TEST_ISSUER).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn key_for_builds_decoding_key_for_known_kid() {
        let (url, hits) = jwks_server().await;
        let cache = cache_with(&url);
        cache.key_for(TEST_ISSUER, "test-only-key-1").await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn key_for_unknown_kid_forces_one_refresh_per_cooldown() {
        let (url, hits) = jwks_server().await;
        let cache = cache_with(&url);
        let err = cache.key_for(TEST_ISSUER, "missing-kid").await.unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
        // A repeat miss inside the 60s cooldown must not re-fetch.
        let err = cache.key_for(TEST_ISSUER, "missing-kid").await.unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
