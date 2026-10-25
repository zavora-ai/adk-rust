//! What `awp verify` and `awp conformance` read the network through: `GET` only, no
//! redirects, signed with Web Bot Auth when a key is given, the `_awp-root` record from DNS
//! (spec 3.5 §7.3 "Outbound", §7.7, A-I9).

use std::sync::Arc;

use adk_auth::web_bot_auth::HttpSignatureSigner;
use adk_awp::signing::anchor::{
    AnchorError, DnsTxtResolver, FixedTxtResolver, TxtAnswer, root_record_name,
};
use adk_awp::verify::fetch::{FetchError, Fetched, Fetcher, HttpFetcher, RequestSigner};
use adk_site::migrate::claim::{DnsResolver as _, RecordKind, SystemDnsResolver};
use anyhow::{Context, Result};
use async_trait::async_trait;
use axum::http::{HeaderMap, Method, Uri};

use super::keyfile::KeyFile;

/// TXT records from the system's resolver, through hickory (adk-site's resolver). Answers are
/// not DNSSEC-validated.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NetworkTxt;

#[async_trait]
impl DnsTxtResolver for NetworkTxt {
    async fn txt(&self, name: &str) -> std::result::Result<TxtAnswer, AnchorError> {
        let records = SystemDnsResolver
            .lookup(name, RecordKind::Txt)
            .await
            .map_err(|error| AnchorError::Lookup(error.to_string()))?;
        Ok(TxtAnswer { records, authenticated: false })
    }
}

/// The root anchor: DNS, or the record the person gave with `--anchor` for a deployment
/// whose DNS is not live yet.
pub(crate) fn resolver(domain: &str, anchor: Option<&str>) -> Arc<dyn DnsTxtResolver> {
    match anchor {
        Some(record) => {
            Arc::new(FixedTxtResolver::default().with(&root_record_name(domain), record))
        }
        None => Arc::new(NetworkTxt),
    }
}

/// Sends the requests for `https://<domain>` to another origin, such as a local or staging
/// deployment; every other URL is fetched as it is.
pub(crate) struct OriginFetcher {
    inner: HttpFetcher,
    from: String,
    to: Option<String>,
}

#[async_trait]
impl Fetcher for OriginFetcher {
    async fn get(
        &self,
        url: &str,
        headers: &HeaderMap,
    ) -> std::result::Result<Fetched, FetchError> {
        let mapped = match (&self.to, url.strip_prefix(&self.from)) {
            (Some(to), Some(rest))
                if rest.is_empty() || rest.starts_with('/') || rest.starts_with('?') =>
            {
                format!("{to}{rest}")
            }
            _ => url.to_owned(),
        };
        self.inner.get(&mapped, headers).await
    }
}

/// Signs each `GET` as a Web Bot Auth agent (spec 3.5 §7.3 "Outbound").
struct WebBotAuth(HttpSignatureSigner);

#[async_trait]
impl RequestSigner for WebBotAuth {
    async fn sign(&self, url: &str, headers: &mut HeaderMap) -> std::result::Result<(), String> {
        let uri: Uri = url.parse().map_err(|error| format!("{url} is not a URI: {error}"))?;
        self.0.sign_head(&Method::GET, &uri, headers).map_err(|error| error.to_string())
    }
}

/// The identity requests carry: a key file and the origin whose key directory publishes it.
pub(crate) struct Identity<'a> {
    pub(crate) key: Option<&'a std::path::Path>,
    pub(crate) agent: Option<&'a str>,
}

/// A fetcher for `domain`, optionally reading it from `origin` and signing with `identity`.
pub(crate) fn fetcher(
    domain: &str,
    origin: Option<&str>,
    identity: &Identity<'_>,
) -> Result<Arc<dyn Fetcher>> {
    let mut inner = HttpFetcher::new().context("cannot build the HTTP client")?;
    match (identity.key, identity.agent) {
        (Some(path), Some(agent)) => {
            let key = Arc::new(KeyFile::read(path)?.web_bot_auth()?);
            let signer = HttpSignatureSigner::builder(key, agent)
                .build()
                .map_err(|error| anyhow::anyhow!("--signature-agent {agent}: {error}"))?;
            inner = inner.with_signer(Arc::new(WebBotAuth(signer)));
        }
        (None, None) => {}
        _ => anyhow::bail!(
            "--sign-key and --signature-agent go together: the key, and the origin whose \
             /.well-known/http-message-signatures-directory publishes it"
        ),
    }
    let to = origin.map(|origin| origin.trim_end_matches('/').to_owned());
    Ok(Arc::new(OriginFetcher { inner, from: format!("https://{domain}"), to }))
}

/// Whether `url` names a loopback host: a deployment the person runs on this machine.
pub(crate) fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name == "localhost" || name.ends_with(".localhost"),
        None => false,
    }
}
