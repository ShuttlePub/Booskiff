use super::*;
use crate::config::Config;
use axum::http::Request;
use rstest::rstest;
use std::net::IpAddr;
use std::sync::Arc;
use tower::ServiceExt;

#[rstest]
#[case(true, Some("198.51.100.1, 203.0.113.1"), Some("198.51.100.1"))]
#[case(true, Some("garbage, 2001:db8::1, 203.0.113.1"), Some("2001:db8::1"))]
#[case(true, Some("garbage, unknown"), Some("192.0.2.1"))]
#[case(true, Some(""), Some("192.0.2.1"))]
#[case(true, None, Some("192.0.2.1"))]
#[case(false, Some("198.51.100.1"), Some("192.0.2.1"))]
fn client_ip_resolves_when_proxy_trust_is_configured(
    #[case] trusted: bool,
    #[case] forwarded: Option<&str>,
    #[case] expected: Option<&str>,
) {
    // Given a peer and an optional forwarded chain.
    let peer = Some("192.0.2.1".parse::<IpAddr>().unwrap());
    let header = forwarded.map(|value| HeaderValue::from_str(value).unwrap());
    // When resolving the client at the trust boundary.
    let resolved = resolve_client_ip(trusted, header.as_ref(), peer);
    // Then the first valid forwarded IP is used only when trusted.
    assert_eq!(resolved, expected.map(|value| value.parse().unwrap()));
}

#[rstest]
#[case(false)]
#[case(true)]
fn client_ip_is_absent_when_no_sources_exist(#[case] trusted: bool) {
    // Given no header or peer; when resolving; then preserve fail-open.
    assert_eq!(resolve_client_ip(trusted, None, None), None);
}

#[test]
fn client_ip_falls_back_when_header_is_not_text() {
    // Given an opaque header and a valid peer.
    let header = HeaderValue::from_bytes(b"\xff").unwrap();
    let peer = Some("192.0.2.1".parse().unwrap());
    // When resolving; then use the peer rather than the invalid header.
    assert_eq!(resolve_client_ip(true, Some(&header), peer), peer);
}

#[test]
fn proxy_headers_are_untrusted_when_config_is_default() {
    // Given default configuration; when inspecting trust; then it is opt-in.
    assert!(!Config::default().trust_proxy_headers);
}

#[rstest]
#[case(true)]
#[case(false)]
#[tokio::test]
async fn public_router_separates_forwarded_budgets_only_when_trusted(#[case] trusted: bool) {
    // Given two forwarded clients behind the same peer with a two-request budget.
    let config = Config {
        trust_proxy_headers: trusted,
        ..Config::default()
    };
    let pool = sqlx::PgPool::connect_lazy("postgres://127.0.0.1:9/never").unwrap();
    pool.close().await;
    let state = AppState {
        pool,
        s3: crate::storage::Storage::build(&config).await.unwrap(),
        jwks_cache: crate::auth::jwks::JwksCache::new(Vec::new()),
        rate_limiters: Arc::new(crate::state::RateLimiters::default()),
        billing_cache: Arc::new(crate::billing::cache::BillingCache::new(60)),
        public_rate_limiter: Arc::new(crate::auth::rate_limit::PublicRateLimiter::new(2)),
        config,
    };
    let app = public_router().with_state(state);
    let peer = SocketAddr::from(([192, 0, 2, 1], 1234));
    // When both clients exhaust their budgets through that peer.
    for (forwarded, limited) in [
        ("198.51.100.1", false),
        ("198.51.100.1", false),
        ("198.51.100.1", true),
        ("198.51.100.2", !trusted),
        ("198.51.100.2", !trusted),
        ("198.51.100.2", true),
    ] {
        let request = Request::builder()
            .uri("/public/test-key")
            .header("x-forwarded-for", forwarded)
            .extension(ConnectInfo(peer))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        // Then trusted clients are independent; spoofing cannot bypass peer limiting.
        assert_eq!(
            response.status(),
            if limited {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        );
    }
}
