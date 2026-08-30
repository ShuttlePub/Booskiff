// allow: SIZE_OK — the requested single-server S1-S4 executable specification is intentionally
// kept in one integration-test target so its sequential state and lifecycle remain explicit.

use std::fs::OpenOptions;
use std::net::TcpListener as StdTcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::routing::get;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

const DATABASE_URL: &str = "postgres://booskiff:booskiff@127.0.0.1:5432/booskiff";
const JWKS: &str = include_str!("fixtures/test_only_jwks.json");
const PRIVATE_KEY: &[u8] = include_bytes!("fixtures/test_only_rsa_private.pem");
const SERVER_LOG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../e2e/core-server.log");

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    sub: &'a str,
    exp: u64,
    owner_type: &'a str,
}

#[derive(Deserialize)]
struct FileResponse {
    id: Uuid,
}

#[derive(Deserialize)]
struct FileListResponse {
    items: Vec<FileResponse>,
}

#[derive(Deserialize)]
struct UrlResponse {
    url: String,
}

#[derive(Deserialize)]
struct BillingStatus {
    used_bytes: i64,
    storage_quota_bytes: i64,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    code: String,
}

#[derive(Deserialize)]
struct CreatedAdminToken {
    id: Uuid,
    token: String,
}

struct JwksServer {
    issuer: String,
    task: JoinHandle<()>,
}

impl Drop for JwksServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct CoreServer {
    base_url: String,
    issuer: String,
    bootstrap_token: String,
    child: Child,
    _jwks: JwksServer,
}

impl Drop for CoreServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl CoreServer {
    async fn start(client: &Client) -> Self {
        let jwks = start_jwks_server().await;
        let port = reserve_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let bootstrap_token = format!("e2e-bootstrap-{}", Uuid::now_v7());
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(SERVER_LOG)
            .expect("open E2E server log");
        let stderr = log.try_clone().expect("clone E2E server log");
        let child = Command::new(env!("CARGO_BIN_EXE_core"))
            .env("DATABASE_URL", DATABASE_URL)
            .env("BOOSKIFF_S3_ENDPOINT", "http://127.0.0.1:9000")
            .env("BOOSKIFF_S3_REGION", "us-east-1")
            .env("BOOSKIFF_S3_BUCKET", "booskiff-default")
            .env("BOOSKIFF_S3_ACCESS_KEY", "booskiff")
            .env("BOOSKIFF_S3_SECRET_KEY", "booskiff-secret")
            .env("BOOSKIFF_JWT_TRUSTED_ISSUERS", &jwks.issuer)
            .env("BOOSKIFF_ADMIN_BOOTSTRAP_TOKEN", &bootstrap_token)
            .env("BOOSKIFF_LISTEN_ADDR", format!("127.0.0.1:{port}"))
            .env("BOOSKIFF_PUBLIC_BASE_URL", &base_url)
            .env("BOOSKIFF_PREMIUM_MODE", "mirror")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("spawn core binary");

        let server = Self {
            base_url,
            issuer: jwks.issuer.clone(),
            bootstrap_token,
            child,
            _jwks: jwks,
        };
        server.wait_ready(client).await;
        server
    }

    async fn wait_ready(&self, client: &Client) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(response) = client.get(format!("{}/readyz", self.base_url)).send().await
                && response.status() == StatusCode::OK
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "core did not become ready within 30s"
            );
            sleep(Duration::from_millis(100)).await;
        }
    }

    fn jwt(&self, owner_type: &str, owner_id: &str) -> String {
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_secs()
            + 300;
        let header = Header {
            alg: Algorithm::RS256,
            kid: Some("test-only-key-1".to_owned()),
            ..Header::default()
        };
        jsonwebtoken::encode(
            &header,
            &Claims {
                iss: &self.issuer,
                sub: owner_id,
                exp,
                owner_type,
            },
            &EncodingKey::from_rsa_pem(PRIVATE_KEY).expect("parse test RSA key"),
        )
        .expect("sign test JWT")
    }
}

async fn start_jwks_server() -> JwksServer {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind JWKS listener");
    let addr = listener.local_addr().expect("JWKS listener address");
    let app = Router::new().route(
        "/.well-known/jwks.json",
        get(|| async { ([("content-type", "application/json")], JWKS) }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve JWKS");
    });
    JwksServer {
        issuer: format!("http://{addr}"),
        task,
    }
}

fn reserve_port() -> u16 {
    StdTcpListener::bind("127.0.0.1:0")
        .expect("reserve core port")
        .local_addr()
        .expect("reserved core address")
        .port()
}

async fn expect_status(response: Response, expected: StatusCode, scenario: &str) -> Response {
    let actual = response.status();
    assert_eq!(actual, expected, "{scenario}: unexpected HTTP status");
    response
}

async fn expect_error_code(response: Response, expected: StatusCode, code: &str, scenario: &str) {
    let response = expect_status(response, expected, scenario).await;
    let body = response
        .json::<ErrorBody>()
        .await
        .expect("parse error body");
    assert_eq!(body.error.code, code, "{scenario}: unexpected error code");
}

async fn billing_status(client: &Client, server: &CoreServer, jwt: &str) -> BillingStatus {
    expect_status(
        client
            .get(format!("{}/v1/billing/status", server.base_url))
            .bearer_auth(jwt)
            .send()
            .await
            .expect("request billing status"),
        StatusCode::OK,
        "billing status",
    )
    .await
    .json()
    .await
    .expect("parse billing status")
}

async fn create_owner_rule(
    client: &Client,
    server: &CoreServer,
    owner_type: &str,
    owner_id: &str,
    key: &str,
    value: i64,
) {
    expect_status(
        client
            .post(format!("{}/v1/admin/billing/rules", server.base_url))
            .header("X-Admin-Token", &server.bootstrap_token)
            .json(&json!({
                "owner_type": owner_type,
                "owner_id": owner_id,
                "key": key,
                "value": value,
                "enabled": true
            }))
            .send()
            .await
            .expect("create owner billing rule"),
        StatusCode::OK,
        "admin create owner rule",
    )
    .await;
}

async fn upload(
    client: &Client,
    server: &CoreServer,
    jwt: &str,
    name: &str,
    bytes: &[u8],
) -> Response {
    client
        .post(format!(
            "{}/v1/files?name={name}&mime=application/octet-stream",
            server.base_url
        ))
        .bearer_auth(jwt)
        .header(reqwest::header::CONTENT_LENGTH, bytes.len())
        .body(bytes.to_vec())
        .send()
        .await
        .expect("upload file")
}

async fn scenario_s1(client: &Client, server: &CoreServer) {
    let owner_id = Uuid::now_v7().to_string();
    let jwt = server.jwt("e2e-s1", &owner_id);
    let mut uploaded = vec![0_u8; 1024 * 1024];
    rand::fill(uploaded.as_mut_slice());

    let file = expect_status(
        upload(client, server, &jwt, "s1.bin", &uploaded).await,
        StatusCode::CREATED,
        "S1 upload",
    )
    .await
    .json::<FileResponse>()
    .await
    .expect("S1 parse upload response");
    let status = billing_status(client, server, &jwt).await;
    assert_eq!(status.used_bytes, 1_048_576, "S1 metered bytes");

    let download = expect_status(
        client
            .get(format!(
                "{}/v1/files/{}/download-url",
                server.base_url, file.id
            ))
            .bearer_auth(&jwt)
            .send()
            .await
            .expect("S1 request download URL"),
        StatusCode::OK,
        "S1 download URL",
    )
    .await
    .json::<UrlResponse>()
    .await
    .expect("S1 parse download URL");
    let downloaded = expect_status(
        client
            .get(download.url)
            .send()
            .await
            .expect("S1 download object"),
        StatusCode::OK,
        "S1 presigned download",
    )
    .await
    .bytes()
    .await
    .expect("S1 read downloaded bytes");
    assert_eq!(
        Sha256::digest(&downloaded),
        Sha256::digest(&uploaded),
        "S1 SHA-256"
    );
}

async fn scenario_s2(client: &Client, server: &CoreServer) {
    let owner_type = "e2e-s2";
    let owner_id = Uuid::now_v7().to_string();
    let jwt = server.jwt(owner_type, &owner_id);
    create_owner_rule(
        client,
        server,
        owner_type,
        &owner_id,
        "storage_quota_bytes",
        2_048,
    )
    .await;
    create_owner_rule(
        client,
        server,
        owner_type,
        &owner_id,
        "max_file_bytes",
        4_096,
    )
    .await;
    let initial = billing_status(client, server, &jwt).await.used_bytes;

    let oversized = vec![1_u8; 4_097];
    expect_error_code(
        upload(client, server, &jwt, "oversized.bin", &oversized).await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
        "S2 maximum file size",
    )
    .await;
    assert_eq!(
        billing_status(client, server, &jwt).await.used_bytes,
        initial,
        "S2 oversize usage unchanged"
    );

    let over_quota = vec![2_u8; 3_072];
    expect_error_code(
        upload(client, server, &jwt, "over-quota.bin", &over_quota).await,
        StatusCode::INSUFFICIENT_STORAGE,
        "insufficient_storage",
        "S2 storage quota",
    )
    .await;
    assert_eq!(
        billing_status(client, server, &jwt).await.used_bytes,
        initial,
        "S2 quota usage unchanged"
    );
    let files = expect_status(
        client
            .get(format!("{}/v1/files", server.base_url))
            .bearer_auth(&jwt)
            .send()
            .await
            .expect("S2 list files"),
        StatusCode::OK,
        "S2 list after rejected uploads",
    )
    .await
    .json::<FileListResponse>()
    .await
    .expect("S2 parse file list");
    assert!(
        files.items.is_empty(),
        "S2 rejected uploads must not create file rows"
    );

    create_owner_rule(client, server, owner_type, &owner_id, "rate_limit_rpm", 3).await;
    let mut rate_limited = false;
    for _ in 0..4 {
        let response = client
            .get(format!("{}/v1/files", server.base_url))
            .bearer_auth(&jwt)
            .send()
            .await
            .expect("S2 rate-limit request");
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            let body = response
                .json::<ErrorBody>()
                .await
                .expect("S2 parse rate-limit body");
            assert_eq!(body.error.code, "rate_limited", "S2 rate-limit code");
            rate_limited = true;
        }
    }
    assert!(rate_limited, "S2 burst must produce at least one 429");
}

async fn scenario_s3(client: &Client, server: &CoreServer) {
    let owner_id = Uuid::now_v7().to_string();
    let jwt = server.jwt("e2e-s3", &owner_id);
    let bytes = b"S3 publish lifecycle bytes".to_vec();
    let before = billing_status(client, server, &jwt).await.used_bytes;
    let file = expect_status(
        upload(client, server, &jwt, "published.bin", &bytes).await,
        StatusCode::CREATED,
        "S3 upload",
    )
    .await
    .json::<FileResponse>()
    .await
    .expect("S3 parse upload response");
    let published = expect_status(
        client
            .post(format!("{}/v1/files/{}/publish", server.base_url, file.id))
            .bearer_auth(&jwt)
            .send()
            .await
            .expect("S3 publish"),
        StatusCode::OK,
        "S3 publish",
    )
    .await
    .json::<UrlResponse>()
    .await
    .expect("S3 parse public URL");
    let public = expect_status(
        client
            .get(&published.url)
            .send()
            .await
            .expect("S3 public GET"),
        StatusCode::OK,
        "S3 public GET",
    )
    .await;
    assert_eq!(
        public
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .expect("S3 cache header"),
        "public, max-age=31536000, immutable",
        "S3 immutable cache control"
    );
    assert_eq!(
        public.bytes().await.expect("S3 public bytes").as_ref(),
        bytes,
        "S3 public content"
    );

    expect_status(
        client
            .delete(format!("{}/v1/files/{}/publish", server.base_url, file.id))
            .bearer_auth(&jwt)
            .send()
            .await
            .expect("S3 unpublish"),
        StatusCode::NO_CONTENT,
        "S3 unpublish",
    )
    .await;
    expect_error_code(
        client
            .get(&published.url)
            .send()
            .await
            .expect("S3 unpublished GET"),
        StatusCode::NOT_FOUND,
        "not_found",
        "S3 unpublished public URL",
    )
    .await;
    expect_status(
        client
            .delete(format!("{}/v1/files/{}", server.base_url, file.id))
            .bearer_auth(&jwt)
            .send()
            .await
            .expect("S3 delete file"),
        StatusCode::NO_CONTENT,
        "S3 delete file",
    )
    .await;
    let after = billing_status(client, server, &jwt).await.used_bytes;
    assert_eq!(
        after, before,
        "S3 deletion must subtract exactly the file size"
    );
}

fn contains_matching_secret_field(value: &Value, raw_token: &str) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(name, value)| {
            ((name == "token" || name == "token_hash") && value.as_str() == Some(raw_token))
                || contains_matching_secret_field(value, raw_token)
        }),
        Value::Array(items) => items
            .iter()
            .any(|item| contains_matching_secret_field(item, raw_token)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

async fn scenario_s4(client: &Client, server: &CoreServer) {
    let token_name = format!("e2e-{}", Uuid::now_v7());
    let created = expect_status(
        client
            .post(format!("{}/v1/admin/tokens", server.base_url))
            .header("X-Admin-Token", &server.bootstrap_token)
            .json(&json!({"name": token_name}))
            .send()
            .await
            .expect("S4 create admin token"),
        StatusCode::CREATED,
        "S4 create admin token",
    )
    .await
    .json::<CreatedAdminToken>()
    .await
    .expect("S4 parse created admin token");
    let listed = expect_status(
        client
            .get(format!("{}/v1/admin/tokens", server.base_url))
            .header("X-Admin-Token", &server.bootstrap_token)
            .send()
            .await
            .expect("S4 list admin tokens"),
        StatusCode::OK,
        "S4 list admin tokens",
    )
    .await
    .json::<Value>()
    .await
    .expect("S4 parse admin token list");
    assert!(
        !contains_matching_secret_field(&listed, &created.token),
        "S4 token list leaked raw secret material"
    );

    expect_status(
        client
            .get(format!("{}/v1/admin/tokens", server.base_url))
            .header("X-Admin-Token", &created.token)
            .send()
            .await
            .expect("S4 use new admin token"),
        StatusCode::OK,
        "S4 new admin token auth",
    )
    .await;
    expect_status(
        client
            .delete(format!(
                "{}/v1/admin/tokens/{}",
                server.base_url, created.id
            ))
            .header("X-Admin-Token", &server.bootstrap_token)
            .send()
            .await
            .expect("S4 revoke admin token"),
        StatusCode::NO_CONTENT,
        "S4 revoke admin token",
    )
    .await;
    expect_error_code(
        client
            .get(format!("{}/v1/admin/tokens", server.base_url))
            .header("X-Admin-Token", &created.token)
            .send()
            .await
            .expect("S4 use revoked admin token"),
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "S4 revoked admin token",
    )
    .await;

    let owner_type = "e2e-s4";
    let owner_id = Uuid::now_v7().to_string();
    let jwt = server.jwt(owner_type, &owner_id);
    expect_status(
        client
            .put(format!(
                "{}/v1/admin/owners/{owner_type}/{owner_id}/plan",
                server.base_url
            ))
            .header("X-Admin-Token", &server.bootstrap_token)
            .json(&json!({"plan": "premium"}))
            .send()
            .await
            .expect("S4 assign premium plan"),
        StatusCode::NO_CONTENT,
        "S4 assign premium plan",
    )
    .await;
    assert_eq!(
        billing_status(client, server, &jwt)
            .await
            .storage_quota_bytes,
        10_737_418_240,
        "S4 premium quota"
    );

    let override_quota = 12_345_678_i64;
    create_owner_rule(
        client,
        server,
        owner_type,
        &owner_id,
        "storage_quota_bytes",
        override_quota,
    )
    .await;
    assert_eq!(
        billing_status(client, server, &jwt)
            .await
            .storage_quota_bytes,
        override_quota,
        "S4 owner rule overrides plan"
    );
}

#[ignore]
#[test]
fn e2e_suite() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build E2E Tokio runtime");
    runtime.block_on(async {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("build E2E HTTP client");
        let server = CoreServer::start(&client).await;

        scenario_s1(&client, &server).await;
        scenario_s2(&client, &server).await;
        scenario_s3(&client, &server).await;
        scenario_s4(&client, &server).await;
    });
}
