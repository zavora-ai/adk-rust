//! S3-compatible object storage: Amazon S3, MinIO, Cloudflare R2, Backblaze B2, Wasabi and any
//! store that speaks the S3 REST API with Signature Version 4 (spec 3.5 §7.8 "Hosts",
//! feature `host-s3`).
//!
//! [`S3Store`] is an [`ObjectStore`]; put it under an
//! [`ObjectStoreHost`](super::object::ObjectStoreHost) to publish releases:
//!
//! | Layout | Serve with |
//! |--------|-----------|
//! | [`ObjectStoreHost::prefix_pointer`](super::object::ObjectStoreHost::prefix_pointer) | An edge that reads `current.pointer` and rewrites `/path` to `<pointer>/path`: Lambda@Edge, a Cloudflare Worker, an nginx `proxy_pass` with a short-cached pointer lookup |
//! | [`ObjectStoreHost::in_place`](super::object::ObjectStoreHost::in_place) | The bucket's website endpoint; a second, private bucket keeps the releases |
//!
//! [`CloudFrontInvalidation`] is a [`CachePurge`] for a CloudFront distribution.
//!
//! Requests are signed with AWS Signature Version 4 over the SHA-256 of the body. The
//! credential is JSON `{"accessKeyId", "secretAccessKey", "sessionToken"?}` from a
//! [`HostCredential`]. Grant `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject` and
//! `s3:ListBucket` (without it a missing object answers 403 instead of 404).

use std::fmt;

use adk_core::Url;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::Method;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::HostCredential;
use super::http::{ATTEMPTS, backoff, failure, retryable, uri_encode};
use super::object::{CachePurge, ObjectMeta, ObjectStore};
use crate::error::{Result, SiteError};

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// AWS access keys, parsed from a [`HostCredential`]'s secret.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AwsKeys {
    pub(crate) access_key_id: String,
    pub(crate) secret_access_key: String,
    #[serde(default)]
    pub(crate) session_token: Option<String>,
}

impl fmt::Debug for AwsKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AwsKeys")
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

impl AwsKeys {
    pub(crate) fn parse(host: &str, secret: &str) -> Result<Self> {
        serde_json::from_str(secret).map_err(|_| {
            SiteError::host(
                host,
                "the credential is not JSON {\"accessKeyId\", \"secretAccessKey\", \"sessionToken\"?}; \
                 store the keys in that form in the secret the broker reads",
            )
        })
    }
}

/// Who signs a request with AWS Signature Version 4, for which region and service.
pub(crate) struct SigV4<'a> {
    pub(crate) keys: &'a AwsKeys,
    pub(crate) region: &'a str,
    pub(crate) service: &'a str,
}

impl SigV4<'_> {
    /// Signs a request: adds `x-amz-date`, `x-amz-content-sha256`, `x-amz-security-token`
    /// when the keys carry one, and `authorization` to `headers`. Every header in `headers`
    /// is signed.
    pub(crate) fn sign(
        &self,
        method: &Method,
        url: &Url,
        headers: &mut Vec<(String, String)>,
        payload_sha256: &str,
        now: DateTime<Utc>,
    ) {
        let (keys, region, service) = (self.keys, self.region, self.service);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };
        headers.push(("x-amz-date".into(), amz_date.clone()));
        headers.push(("x-amz-content-sha256".into(), payload_sha256.into()));
        if let Some(token) = &keys.session_token {
            headers.push(("x-amz-security-token".into(), token.clone()));
        }
        let mut canonical: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| {
                (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" "))
            })
            .chain(std::iter::once(("host".to_string(), host)))
            .collect();
        canonical.sort();
        let signed_headers =
            canonical.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        let canonical_headers: String =
            canonical.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let mut query: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (uri_encode(&k, false), uri_encode(&v, false)))
            .collect();
        query.sort();
        let canonical_query =
            query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
        let path = if url.path().is_empty() { "/" } else { url.path() };
        let canonical_request = format!(
            "{method}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_sha256}"
        );
        let scope = format!("{date}/{region}/{service}/aws4_request");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex::encode(Sha256::digest(canonical_request.as_bytes()))
        );
        let k_date = hmac(format!("AWS4{}", keys.secret_access_key).as_bytes(), &date);
        let k_region = hmac(&k_date, region);
        let k_service = hmac(&k_region, service);
        let k_signing = hmac(&k_service, "aws4_request");
        let signature = hex::encode(hmac(&k_signing, &string_to_sign));
        headers.push((
            "authorization".into(),
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                keys.access_key_id
            ),
        ));
    }
}

/// An S3-compatible bucket as an [`ObjectStore`] (spec 3.5 §7.8 "Hosts").
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::host::object::ObjectStoreHost;
/// use adk_site::host::s3::S3Store;
/// use adk_site::host::HostCredential;
///
/// # fn demo(credential: HostCredential) -> adk_site::Result<()> {
/// // Amazon S3, virtual-hosted style.
/// let store = S3Store::aws("eu-west-1", "example-site", credential.clone());
/// let host = ObjectStoreHost::prefix_pointer(Arc::new(store));
///
/// // MinIO or any other S3-compatible endpoint, path style.
/// let minio = S3Store::new("http://127.0.0.1:9000".parse().unwrap(), "us-east-1", "site", credential)
///     .path_style(true)
///     .with_prefix("example.com");
/// # let _ = (host, minio);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct S3Store {
    name: String,
    client: reqwest::Client,
    endpoint: Url,
    region: String,
    bucket: String,
    path_style: bool,
    prefix: String,
    credential: HostCredential,
}

impl S3Store {
    /// The bucket `bucket` at `endpoint` in `region`, virtual-hosted style.
    pub fn new(
        endpoint: Url,
        region: impl Into<String>,
        bucket: impl Into<String>,
        credential: HostCredential,
    ) -> Self {
        Self {
            name: "s3".into(),
            client: reqwest::Client::new(),
            endpoint,
            region: region.into(),
            bucket: bucket.into(),
            path_style: false,
            prefix: String::new(),
            credential,
        }
    }

    /// The Amazon S3 bucket `bucket` in `region`.
    pub fn aws(region: &str, bucket: impl Into<String>, credential: HostCredential) -> Self {
        let endpoint = format!("https://s3.{region}.amazonaws.com")
            .parse()
            .unwrap_or_else(|_| Url::parse("https://s3.amazonaws.com").expect("static URL"));
        Self::new(endpoint, region, bucket, credential)
    }

    /// Addresses the bucket in the path (`endpoint/bucket/key`) rather than the host name;
    /// MinIO and most emulators need this.
    #[must_use]
    pub fn path_style(mut self, path_style: bool) -> Self {
        self.path_style = path_style;
        self
    }

    /// Keeps every key under `prefix/` in the bucket.
    #[must_use]
    pub fn with_prefix(mut self, prefix: &str) -> Self {
        let prefix = prefix.trim_matches('/');
        self.prefix = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
        self
    }

    /// Names the store in reports.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sends requests through `client`.
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// The URL of the bucket: what the credential's resource must cover.
    pub fn bucket_url(&self) -> Url {
        let mut url = self.endpoint.clone();
        if self.path_style {
            url.set_path(&format!(
                "{}/{}",
                url.path().trim_end_matches('/'),
                uri_encode(&self.bucket, false)
            ));
        } else if let Some(host) = self.endpoint.host_str() {
            let _ = url.set_host(Some(&format!("{}.{host}", self.bucket)));
        }
        url
    }

    fn object_path(&self, key: &str) -> String {
        uri_encode(&format!("{}{key}", self.prefix), true)
    }

    fn object_url(&self, key: &str) -> Result<Url> {
        let bucket = self.bucket_url();
        let url = format!("{}/{}", bucket.as_str().trim_end_matches('/'), self.object_path(key));
        Url::parse(&url).map_err(|e| SiteError::host(&self.name, format!("bad object URL: {e}")))
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Bytes,
        extra: &[(&str, String)],
    ) -> Result<reqwest::Response> {
        let payload = hex::encode(Sha256::digest(&body));
        let mut attempt = 0;
        loop {
            attempt += 1;
            let secret = self.credential.secret_for(&self.name, &url).await?;
            let keys = AwsKeys::parse(&self.name, secret.expose())?;
            let mut headers: Vec<(String, String)> =
                extra.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
            SigV4 { keys: &keys, region: &self.region, service: "s3" }.sign(
                &method,
                &url,
                &mut headers,
                &payload,
                Utc::now(),
            );
            let mut request = self.client.request(method.clone(), url.clone()).body(body.clone());
            for (name, value) in &headers {
                request = request.header(name.as_str(), value.as_str());
            }
            let result = request.send().await;
            if attempt < ATTEMPTS && retryable(&result) {
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }
            return result.map_err(|e| {
                SiteError::host(&self.name, format!("{method} {}: {}", url.path(), e.without_url()))
            });
        }
    }
}

#[async_trait]
impl ObjectStore for S3Store {
    fn name(&self) -> &str {
        &self.name
    }

    async fn put(&self, key: &str, body: Bytes, meta: &ObjectMeta) -> Result<()> {
        let mut headers = vec![("content-type", meta.content_type.clone())];
        if let Some(cache) = &meta.cache_control {
            headers.push(("cache-control", cache.clone()));
        }
        let response = self.send(Method::PUT, self.object_url(key)?, body, &headers).await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(SiteError::host(&self.name, format!("PUT {key}: {}", failure(response).await)))
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let response = self.send(Method::GET, self.object_url(key)?, Bytes::new(), &[]).await?;
        match response.status() {
            status if status.is_success() => response.bytes().await.map(Some).map_err(|e| {
                SiteError::host(&self.name, format!("GET {key}: {}", e.without_url()))
            }),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            _ => {
                Err(SiteError::host(&self.name, format!("GET {key}: {}", failure(response).await)))
            }
        }
    }

    async fn copy(&self, from: &str, to: &str) -> Result<()> {
        let source = format!("/{}/{}", uri_encode(&self.bucket, false), self.object_path(from));
        let response = self
            .send(Method::PUT, self.object_url(to)?, Bytes::new(), &[("x-amz-copy-source", source)])
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        // A copy can answer 200 with an error document.
        if status.is_success() && !body.contains("<Error>") {
            Ok(())
        } else {
            let body: String = body.chars().take(300).collect();
            Err(SiteError::host(
                &self.name,
                format!("copy {from} to {to}: {status}: {}", body.trim()),
            ))
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let response = self.send(Method::DELETE, self.object_url(key)?, Bytes::new(), &[]).await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(SiteError::host(&self.name, format!("DELETE {key}: {}", failure(response).await)))
        }
    }
}

/// Invalidates a CloudFront distribution's cached copies of published paths (spec 3.5
/// §7.8.2 step 6), signed with Signature Version 4 for `cloudfront` in `us-east-1`.
///
/// A file `/a/index.html` is invalidated with its directory URL `/a/`. Above
/// [`max_paths`](Self::with_max_paths) paths (1,000 by default) one `/*` invalidation
/// replaces them.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::host::object::ObjectStoreHost;
/// use adk_site::host::s3::{CloudFrontInvalidation, S3Store};
/// use adk_site::host::HostCredential;
///
/// # fn demo(s3: HostCredential, cloudfront: HostCredential) {
/// let host = ObjectStoreHost::prefix_pointer(Arc::new(S3Store::aws("eu-west-1", "site", s3)))
///     .with_cache_purge(Arc::new(CloudFrontInvalidation::new("E2EXAMPLE", cloudfront)));
/// # let _ = host;
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct CloudFrontInvalidation {
    client: reqwest::Client,
    endpoint: Url,
    distribution: String,
    credential: HostCredential,
    max_paths: usize,
}

impl CloudFrontInvalidation {
    /// Invalidations for distribution `distribution`.
    pub fn new(distribution: impl Into<String>, credential: HostCredential) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: Url::parse("https://cloudfront.amazonaws.com").expect("static URL"),
            distribution: distribution.into(),
            credential,
            max_paths: 1_000,
        }
    }

    /// Sends requests to `endpoint` instead of `https://cloudfront.amazonaws.com`.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: Url) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Invalidates everything (`/*`) above `paths` paths.
    #[must_use]
    pub fn with_max_paths(mut self, paths: usize) -> Self {
        self.max_paths = paths.max(1);
        self
    }

    /// The invalidation batch for `paths`, as CloudFront's XML.
    fn batch(&self, paths: &[String], reference: &str) -> String {
        let mut items: Vec<String> = Vec::new();
        for path in paths {
            let path = if path.starts_with('/') { path.clone() } else { format!("/{path}") };
            if let Some(dir) = path.strip_suffix("index.html") {
                items.push(dir.to_string());
            }
            items.push(path);
        }
        items.sort();
        items.dedup();
        if items.len() > self.max_paths {
            items = vec!["/*".into()];
        }
        let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let list: String = items
            .iter()
            .map(|p| format!("<Path>{}</Path>", escape(&uri_encode(p, true).replace("%2A", "*"))))
            .collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <InvalidationBatch xmlns=\"http://cloudfront.amazonaws.com/doc/2020-05-31/\">\
             <Paths><Quantity>{}</Quantity><Items>{list}</Items></Paths>\
             <CallerReference>{reference}</CallerReference></InvalidationBatch>",
            items.len()
        )
    }
}

#[async_trait]
impl CachePurge for CloudFrontInvalidation {
    async fn purge(&self, paths: &[String]) -> Result<()> {
        const NAME: &str = "cloudfront";
        let reference =
            format!("adk-site-{}", Utc::now().timestamp_nanos_opt().unwrap_or_default());
        let body = Bytes::from(self.batch(paths, &reference));
        let url = Url::parse(&format!(
            "{}/2020-05-31/distribution/{}/invalidation",
            self.endpoint.as_str().trim_end_matches('/'),
            uri_encode(&self.distribution, false)
        ))
        .map_err(|e| SiteError::host(NAME, format!("bad invalidation URL: {e}")))?;
        let secret = self.credential.secret_for(NAME, &url).await?;
        let keys = AwsKeys::parse(NAME, secret.expose())?;
        let mut headers = vec![("content-type".to_string(), "text/xml".to_string())];
        let payload = hex::encode(Sha256::digest(&body));
        SigV4 { keys: &keys, region: "us-east-1", service: "cloudfront" }.sign(
            &Method::POST,
            &url,
            &mut headers,
            &payload,
            Utc::now(),
        );
        let mut request = self.client.post(url).body(body);
        for (name, value) in &headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response =
            request.send().await.map_err(|e| SiteError::host(NAME, e.without_url().to_string()))?;
        if response.status().is_success() {
            tracing::info!(cloudfront.distribution = %self.distribution, paths = paths.len(), "invalidation created");
            Ok(())
        } else {
            Err(SiteError::host(NAME, format!("invalidation: {}", failure(response).await)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn example_keys() -> AwsKeys {
        AwsKeys {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        }
    }

    fn signature(headers: &[(String, String)]) -> String {
        let auth = &headers.iter().find(|(k, _)| k == "authorization").unwrap().1;
        auth.rsplit("Signature=").next().unwrap().to_string()
    }

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    // The worked examples in the Amazon S3 API reference, "Signature Calculations for the
    // Authorization Header: Transferring Payload in a Single Chunk".
    #[test]
    fn signatures_match_the_s3_reference_examples() {
        let now = Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap();
        let keys = example_keys();

        let url = Url::parse("https://examplebucket.s3.amazonaws.com/test.txt").unwrap();
        let mut headers = vec![("range".to_string(), "bytes=0-9".to_string())];
        SigV4 { keys: &keys, region: "us-east-1", service: "s3" }.sign(
            &Method::GET,
            &url,
            &mut headers,
            EMPTY,
            now,
        );
        assert_eq!(
            signature(&headers),
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );

        let url = Url::parse("https://examplebucket.s3.amazonaws.com/?lifecycle").unwrap();
        let mut headers = Vec::new();
        SigV4 { keys: &keys, region: "us-east-1", service: "s3" }.sign(
            &Method::GET,
            &url,
            &mut headers,
            EMPTY,
            now,
        );
        assert_eq!(
            signature(&headers),
            "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        );

        let url =
            Url::parse("https://examplebucket.s3.amazonaws.com/?max-keys=2&prefix=J").unwrap();
        let mut headers = Vec::new();
        SigV4 { keys: &keys, region: "us-east-1", service: "s3" }.sign(
            &Method::GET,
            &url,
            &mut headers,
            EMPTY,
            now,
        );
        assert_eq!(
            signature(&headers),
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }

    #[test]
    fn the_put_example_signs_its_payload_and_extra_headers() {
        let now = Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap();
        let body = b"Welcome to Amazon S3.";
        let payload = hex::encode(Sha256::digest(body));
        assert_eq!(payload, "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072");
        let url = Url::parse("https://examplebucket.s3.amazonaws.com/test%24file.text").unwrap();
        let mut headers = vec![
            ("date".to_string(), "Fri, 24 May 2013 00:00:00 GMT".to_string()),
            ("x-amz-storage-class".to_string(), "REDUCED_REDUNDANCY".to_string()),
        ];
        let keys = example_keys();
        SigV4 { keys: &keys, region: "us-east-1", service: "s3" }.sign(
            &Method::PUT,
            &url,
            &mut headers,
            &payload,
            now,
        );
        assert_eq!(
            signature(&headers),
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    #[test]
    fn keys_are_encoded_once_and_slashes_kept() {
        assert_eq!(uri_encode("a b/c$d~e", true), "a%20b/c%24d~e");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(uri_encode("é", true), "%C3%A9");
    }

    #[test]
    fn credentials_must_be_the_documented_json() {
        assert!(AwsKeys::parse("s3", "AKIA:secret").is_err());
        let keys = AwsKeys::parse("s3", r#"{"accessKeyId":"a","secretAccessKey":"b"}"#).unwrap();
        assert_eq!(keys.session_token, None);
        assert!(!format!("{keys:?}").contains('b'), "the secret is not in Debug");
    }
}
