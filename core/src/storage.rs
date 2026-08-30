// allow: SIZE_OK — the module owns one responsibility (the object-storage
// gateway over one S3 client), but cannot be split: the public API and
// module tree are frozen for this wave, and the mandated unit + ignored
// integration tests must live in this file.
//! S3-compatible object storage access (MinIO in dev).

use std::time::Duration;

use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::byte_stream::ByteStream;

use crate::config::Config;
use crate::error::AppError;
use crate::model::Owner;

/// MinIO enforces the S3 protocol limit of 1000 keys per
/// `DeleteObjects` request.
const DELETE_BATCH_LIMIT: usize = 1000;

/// Thin wrapper over the S3 client bound to one bucket.
#[derive(Clone)]
pub struct Storage {
    client: aws_sdk_s3::Client,
    bucket: String,
    /// Endpoint the client talks to; presigned URLs are rewritten from
    /// this prefix to `public_endpoint` when one is configured.
    endpoint: String,
    public_endpoint: Option<String>,
}

impl Storage {
    /// Build a client from config: static credentials, explicit region and
    /// endpoint, path-style addressing (required by MinIO).
    pub async fn build(config: &Config) -> Result<Self, AppError> {
        let shared = aws_config::from_env()
            .region(aws_sdk_s3::config::Region::new(config.s3_region.clone()))
            .endpoint_url(config.s3_endpoint.clone())
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                config.s3_access_key.clone(),
                config.s3_secret_key.clone(),
                None,
                None,
                "booskiff-config",
            ))
            .load()
            .await;
        let s3_config = aws_sdk_s3::Config::from(&shared)
            .to_builder()
            .force_path_style(true)
            .build();
        Ok(Self {
            client: aws_sdk_s3::Client::from_conf(s3_config),
            bucket: config.s3_bucket.clone(),
            endpoint: config.s3_endpoint.clone(),
            public_endpoint: config.s3_endpoint_public.clone(),
        })
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Endpoint used for presigned URLs handed to external clients
    /// (`None` = same endpoint the client talks to).
    pub fn public_endpoint(&self) -> Option<&str> {
        self.public_endpoint.as_deref()
    }

    /// Storage key for one object of a file:
    /// `{owner_type}/{owner_id}/{file_id}/{object_kind}`.
    pub fn object_key(owner: &Owner, file_id: &uuid::Uuid, kind: &str) -> String {
        format!(
            "{}/{}/{}/{}",
            owner.owner_type, owner.owner_id, file_id, kind
        )
    }

    /// Create the configured bucket if it does not exist yet.
    pub async fn ensure_bucket(&self) -> Result<(), AppError> {
        match self.client.head_bucket().bucket(&self.bucket).send().await {
            Ok(_) => Ok(()),
            Err(err) => {
                // HeadBucket carries no modeled errors in aws-sdk-s3, so a
                // 404 status is the bucket-missing signal.
                let missing = err
                    .raw_response()
                    .is_some_and(|response| response.status().as_u16() == 404);
                if !missing {
                    return Err(AppError::Internal(format!(
                        "head bucket {}: {err}",
                        self.bucket
                    )));
                }
                self.client
                    .create_bucket()
                    .bucket(&self.bucket)
                    .send()
                    .await
                    .map_err(|err| {
                        AppError::Internal(format!("create bucket {}: {err}", self.bucket))
                    })?;
                Ok(())
            }
        }
    }

    /// Upload `body` to `key`.
    ///
    /// `content_length` is forwarded explicitly: the SDK needs a known
    /// size or it falls back to aws-chunked encoding, which MinIO
    /// mishandles on some paths. The value is the caller's accounting
    /// (its counting wrapper); this method does not re-verify it.
    pub async fn put_streaming(
        &self,
        key: &str,
        body: SdkBody,
        content_length: i64,
        content_type: &str,
    ) -> Result<(), AppError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .content_length(content_length)
            .body(ByteStream::new(body))
            .send()
            .await
            .map(|_| ())
            .map_err(|err| {
                tracing::warn!(key, error = %err, "put object");
                AppError::Internal(format!("put object {key}: {err}"))
            })
    }

    /// Return a presigned GET URL valid for `ttl`.
    ///
    /// The presigned URI targets `endpoint` (path style), so when a
    /// `s3_endpoint_public` is configured the prefix is rewritten to it:
    /// external clients get reachable URLs while the client keeps
    /// talking to the internal endpoint.
    pub async fn presign_get(&self, key: &str, ttl: Duration) -> Result<String, AppError> {
        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(
                PresigningConfig::builder()
                    .expires_in(ttl)
                    .build()
                    .map_err(|err| AppError::Internal(format!("presign get {key}: {err}")))?,
            )
            .await
            .map_err(|err| {
                tracing::warn!(key, error = %err, "presign get");
                AppError::Internal(format!("presign get {key}: {err}"))
            })?;
        let uri = presigned.uri().to_string();
        Ok(match &self.public_endpoint {
            Some(public) => rewrite_endpoint(&uri, &self.endpoint, public),
            None => uri,
        })
    }

    /// Return `(mime_type, size_bytes, stream)` for `key`.
    pub async fn get_streaming(&self, key: &str) -> Result<(String, i64, ByteStream), AppError> {
        match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(output) => Ok((
                output
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_owned(),
                output.content_length().unwrap_or(0),
                output.body,
            )),
            Err(err) if is_missing_object(&err) => Err(AppError::NotFound(format!("object {key}"))),
            Err(err) => {
                tracing::warn!(key, error = %err, "get object");
                Err(AppError::Internal(format!("get object {key}: {err}")))
            }
        }
    }

    /// Delete a single object, tolerating a missing key: S3 deletes are
    /// idempotent, so a Not-Found response is success here.
    pub async fn delete_object(&self, key: &str) -> Result<(), AppError> {
        match self
            .client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(err) if is_missing_object(&err) => Ok(()),
            Err(err) => {
                tracing::warn!(key, error = %err, "delete object");
                Err(AppError::Internal(format!("delete object {key}: {err}")))
            }
        }
    }

    /// List and delete every object under `prefix`, tolerating missing
    /// objects.
    ///
    /// Listing completes before deletion starts so the continuation
    /// cursor never races the deletions. Individual deletion failures are
    /// warned and processing continues best-effort; the first failure is
    /// then surfaced as [`AppError::Internal`]. A failed listing aborts
    /// immediately (no further pages can be fetched).
    pub async fn delete_prefix(&self, prefix: &str) -> Result<(), AppError> {
        let mut keys: Vec<String> = Vec::new();
        let mut continuation_token: Option<String> = None;
        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);
            if let Some(token) = continuation_token {
                request = request.continuation_token(token);
            }
            let page = request.send().await.map_err(|err| {
                tracing::warn!(prefix, error = %err, "list objects");
                AppError::Internal(format!("list objects {prefix}: {err}"))
            })?;
            keys.extend(
                page.contents()
                    .iter()
                    .filter_map(|object| object.key())
                    .map(str::to_owned),
            );
            continuation_token = page.next_continuation_token().map(str::to_owned);
            if continuation_token.is_none() {
                break;
            }
        }

        let mut first_error: Option<String> = None;
        for batch in keys.chunks(DELETE_BATCH_LIMIT) {
            let objects = batch
                .iter()
                .map(|key| ObjectIdentifier::builder().key(key).build())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| {
                    AppError::Internal(format!("build delete request {prefix}: {err}"))
                })?;
            let delete = Delete::builder()
                .set_objects(Some(objects))
                .build()
                .map_err(|err| {
                    AppError::Internal(format!("build delete request {prefix}: {err}"))
                })?;
            match self
                .client
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete)
                .send()
                .await
            {
                Ok(output) => {
                    if let Some(object_error) = output.errors().first() {
                        tracing::warn!(
                            prefix,
                            key = object_error.key().unwrap_or_default(),
                            code = object_error.code().unwrap_or_default(),
                            "object not deleted"
                        );
                        if first_error.is_none() {
                            first_error = Some(format!(
                                "delete object {} under {prefix}: {}",
                                object_error.key().unwrap_or_default(),
                                object_error.code().unwrap_or("unknown error")
                            ));
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(prefix, error = %err, "batch delete objects");
                    if first_error.is_none() {
                        first_error = Some(format!("delete objects {prefix}: {err}"));
                    }
                }
            }
        }
        match first_error {
            Some(message) => Err(AppError::Internal(message)),
            None => Ok(()),
        }
    }
}

/// Whether an S3 call failed because the addressed object does not exist.
///
/// `DeleteObject` does not model `NoSuchKey` (aws-sdk-s3), so detect it
/// via the wire error code, with the raw 404 status as fallback for
/// unparsed error bodies.
fn is_missing_object<E>(err: &SdkError<E>) -> bool
where
    E: ProvideErrorMetadata,
{
    err.as_service_error()
        .is_some_and(|service_err| service_err.code() == Some("NoSuchKey"))
        || err
            .raw_response()
            .is_some_and(|response| response.status().as_u16() == 404)
}

/// Replace the `from` endpoint prefix of `url` (schemes and host) with
/// `to`, keeping the `<bucket>/<key>` path and query intact; returns
/// `url` unchanged when it does not start with the `from` prefix.
fn rewrite_endpoint(url: &str, from: &str, to: &str) -> String {
    let from = from.trim_end_matches('/');
    let to = to.trim_end_matches('/');
    match url.strip_prefix(from) {
        Some(rest) => format!("{to}{rest}"),
        None => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OBJECT_KIND_ORIGINAL;

    const PAYLOAD: &[u8] = b"hello booskiff";

    #[test]
    fn object_key_places_owner_then_file_then_kind() {
        let owner = Owner::new("account", "alice");
        let file_id = uuid::Uuid::parse_str("01890622-0d3a-7abc-8def-0123456789ab").unwrap();
        assert_eq!(
            Storage::object_key(&owner, &file_id, OBJECT_KIND_ORIGINAL),
            "account/alice/01890622-0d3a-7abc-8def-0123456789ab/original"
        );
    }

    #[test]
    fn rewrite_endpoint_replaces_configured_prefix() {
        assert_eq!(
            rewrite_endpoint(
                "http://127.0.0.1:9000/booskiff-default/a/b?X-Amz-Algorithm=AWS4-HMAC-SHA256",
                "http://127.0.0.1:9000",
                "https://cdn.example.com"
            ),
            "https://cdn.example.com/booskiff-default/a/b?X-Amz-Algorithm=AWS4-HMAC-SHA256"
        );
    }

    #[test]
    fn rewrite_endpoint_trims_trailing_slashes() {
        assert_eq!(
            rewrite_endpoint(
                "http://127.0.0.1:9000/booskiff-original/key?sig=1",
                "http://127.0.0.1:9000/",
                "https://cdn.example.com/"
            ),
            "https://cdn.example.com/booskiff-original/key?sig=1"
        );
    }

    #[test]
    fn rewrite_endpoint_leaves_foreign_urls_untouched() {
        let url = "https://other.example.net/booskiff-default/key?sig=1";
        assert_eq!(
            rewrite_endpoint(url, "http://127.0.0.1:9000", "https://cdn.example.com"),
            url
        );
    }

    /// Round-trip against the dev compose MinIO (`docker compose up -d`
    /// from the repo root; defaults of [`Config::default`] match it).
    /// Run manually via `cargo test minio_roundtrip -- --ignored`.
    #[tokio::test]
    #[ignore = "requires the dev compose MinIO at 127.0.0.1:9000"]
    async fn minio_roundtrip_ignored() {
        let storage = Storage::build(&Config::default()).await.unwrap();
        storage.ensure_bucket().await.unwrap();

        // Unique per-run prefix so leftover objects from a failed run
        // never leak into this one.
        let prefix = format!("test/minio-roundtrip/{}/", uuid::Uuid::now_v7());
        let key = format!("{prefix}{OBJECT_KIND_ORIGINAL}");

        storage
            .put_streaming(
                &key,
                ByteStream::from_static(PAYLOAD).into_inner(),
                PAYLOAD.len() as i64,
                "text/plain",
            )
            .await
            .unwrap();

        let (content_type, length, stream) = storage.get_streaming(&key).await.unwrap();
        assert_eq!(content_type, "text/plain");
        assert_eq!(length, PAYLOAD.len() as i64);
        assert_eq!(
            stream.collect().await.unwrap().into_bytes().as_ref(),
            PAYLOAD
        );

        let url = storage
            .presign_get(&key, Duration::from_secs(60))
            .await
            .unwrap();
        assert!(
            url.starts_with("http://127.0.0.1:9000/booskiff-default/"),
            "presigned URL should target the configured endpoint path-style: {url}"
        );
        let response = reqwest::get(&url).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap().as_ref(), PAYLOAD);

        storage.delete_prefix(&prefix).await.unwrap();
        let result = storage.get_streaming(&key).await;
        match result {
            Err(AppError::NotFound(message)) => assert_eq!(message, format!("object {key}")),
            Err(other) => panic!("expected NotFound, got: {other}"),
            Ok(_) => panic!("expected NotFound, object still present"),
        }
    }
}
