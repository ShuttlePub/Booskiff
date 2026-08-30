//! S3-compatible object storage access (MinIO in dev).

use aws_smithy_types::body::SdkBody;
use aws_smithy_types::byte_stream::ByteStream;

use crate::config::Config;
use crate::error::AppError;
use crate::model::Owner;

/// Thin wrapper over the S3 client bound to one bucket.
#[derive(Clone)]
pub struct Storage {
    client: aws_sdk_s3::Client,
    bucket: String,
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

    /// Upload `body` to `key`. TODO(Wave2/T4): real implementation.
    pub async fn put_streaming(
        &self,
        _key: &str,
        _body: SdkBody,
        _content_length: i64,
        _content_type: &str,
    ) -> Result<(), AppError> {
        Err(AppError::Internal("storage not implemented yet".into()))
    }

    /// Return a presigned GET URL valid for `ttl`. TODO(Wave2/T4).
    pub async fn presign_get(
        &self,
        _key: &str,
        _ttl: std::time::Duration,
    ) -> Result<String, AppError> {
        Err(AppError::Internal("storage not implemented yet".into()))
    }

    /// Return `(mime_type, size_bytes, stream)` for `key`. TODO(Wave2/T4).
    pub async fn get_streaming(&self, _key: &str) -> Result<(String, i64, ByteStream), AppError> {
        Err(AppError::Internal("storage not implemented yet".into()))
    }

    /// Delete a single object, tolerating a missing key. TODO(Wave2/T4).
    pub async fn delete_object(&self, _key: &str) -> Result<(), AppError> {
        Err(AppError::Internal("storage not implemented yet".into()))
    }

    /// List and delete every object under `prefix`, tolerating missing
    /// objects. TODO(Wave2/T4).
    pub async fn delete_prefix(&self, _prefix: &str) -> Result<(), AppError> {
        Err(AppError::Internal("storage not implemented yet".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OBJECT_KIND_ORIGINAL;

    #[test]
    fn object_key_places_owner_then_file_then_kind() {
        let owner = Owner::new("account", "alice");
        let file_id = uuid::Uuid::parse_str("01890622-0d3a-7abc-8def-0123456789ab").unwrap();
        assert_eq!(
            Storage::object_key(&owner, &file_id, OBJECT_KIND_ORIGINAL),
            "account/alice/01890622-0d3a-7abc-8def-0123456789ab/original"
        );
    }
}
