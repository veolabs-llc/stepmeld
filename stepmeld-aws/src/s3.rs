//! [`Objects`] in an S3 bucket: an ETag is an object's version, and a
//! conditional write is a PUT with `If-Match` or `If-None-Match: *`.

use crate::{block_on, refused};
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::primitives::ByteStream;
use stepmeld_core::objects::{Cond, Objects};
use stepmeld_core::Error;

pub struct S3Objects {
    client: aws_sdk_s3::Client,
    bucket: String,
}

impl S3Objects {
    pub fn new(client: aws_sdk_s3::Client, bucket: &str) -> S3Objects {
        S3Objects { client, bucket: bucket.into() }
    }
}

impl Objects for S3Objects {
    fn describe(&self) -> String {
        format!("s3://{}", self.bucket)
    }

    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, Error> {
        block_on(async {
            match self.client.get_object().bucket(&self.bucket).key(key).send().await {
                Ok(out) => {
                    let etag = out.e_tag().unwrap_or_default().to_string();
                    let body = out.body.collect().await.map_err(|e| refused("read", key, e))?.into_bytes().to_vec();
                    Ok(Some((body, etag)))
                }
                Err(e) if e.as_service_error().is_some_and(|s| s.is_no_such_key()) || e.raw_response().is_some_and(|r| r.status().as_u16() == 404) => Ok(None),
                Err(e) => Err(refused("read", key, e)),
            }
        })
    }

    fn put(&self, key: &str, body: &[u8], cond: Cond) -> Result<Option<String>, Error> {
        block_on(async {
            let mut put = self.client.put_object().bucket(&self.bucket).key(key).content_type("application/json").body(ByteStream::from(body.to_vec()));
            put = match cond {
                Cond::None => put,
                Cond::IfMatch(v) => put.if_match(v),
                Cond::IfNoneMatch => put.if_none_match("*"),
            };
            // a conditional write is tried once: an answer lost on the
            // way back could hide a write that landed, and trying again
            // would then lose to itself
            let sent = if cond == Cond::None {
                put.send().await
            } else {
                put.customize()
                    .config_override(aws_sdk_s3::config::Builder::default().retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled()))
                    .send()
                    .await
            };
            match sent {
                Ok(out) => Ok(Some(out.e_tag().unwrap_or_default().to_string())),
                Err(e) if matches!(e.code(), Some("PreconditionFailed" | "ConditionalRequestConflict")) || e.raw_response().is_some_and(|r| matches!(r.status().as_u16(), 412 | 409)) => Ok(None),
                Err(e) => Err(refused("write", key, e)),
            }
        })
    }

    fn delete(&self, key: &str) -> Result<(), Error> {
        block_on(async { self.client.delete_object().bucket(&self.bucket).key(key).send().await.map(|_| ()).map_err(|e| refused("delete", key, e)) })
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, Error> {
        block_on(async {
            let mut keys = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let out = self
                    .client
                    .list_objects_v2()
                    .bucket(&self.bucket)
                    .prefix(prefix)
                    .set_continuation_token(token.clone())
                    .send()
                    .await
                    .map_err(|e| refused("list", prefix, e))?;
                keys.extend(out.contents().iter().filter_map(|o| o.key().map(str::to_string)));
                token = out.next_continuation_token().map(str::to_string);
                if token.is_none() || !out.is_truncated().unwrap_or(false) {
                    break;
                }
            }
            keys.sort();
            Ok(keys)
        })
    }

    /// A listing carries each object's ETag, so versions come with it.
    fn listing(&self, prefix: &str) -> Result<Vec<(String, String)>, Error> {
        block_on(async {
            let mut out = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let page = self
                    .client
                    .list_objects_v2()
                    .bucket(&self.bucket)
                    .prefix(prefix)
                    .set_continuation_token(token.clone())
                    .send()
                    .await
                    .map_err(|e| refused("list", prefix, e))?;
                out.extend(page.contents().iter().filter_map(|o| Some((o.key()?.to_string(), o.e_tag()?.to_string()))));
                token = page.next_continuation_token().map(str::to_string);
                if token.is_none() || !page.is_truncated().unwrap_or(false) {
                    break;
                }
            }
            out.sort();
            Ok(out)
        })
    }
}
