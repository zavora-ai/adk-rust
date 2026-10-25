//! A credential broker over fixed test secrets, and in-process fake provider servers.

use std::sync::Arc;

use adk_core::{
    ActingContext, CredentialBroker, Result, RevocationScope, ScopedToken, SubjectKind, Url,
    async_trait,
};
use adk_site::host::HostCredential;

/// Hands out one fixed secret, for the resource it is asked about.
#[derive(Debug)]
pub struct FixedBroker(pub String);

#[async_trait]
impl CredentialBroker for FixedBroker {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        scopes: &[String],
    ) -> Result<ScopedToken> {
        Ok(ScopedToken::new(
            acting.agent.id.clone(),
            self.0.clone(),
            resource.clone(),
            scopes.to_vec(),
        ))
    }

    async fn revoke(&self, _scope: &RevocationScope) -> Result<u64> {
        Ok(0)
    }
}

/// A host credential holding `secret` for `resource`.
pub fn credential(resource: &str, secret: &str) -> HostCredential {
    let acting = ActingContext::derived("site", "publisher", "owner", SubjectKind::Human);
    HostCredential::new(
        Arc::new(FixedBroker(secret.to_string())),
        acting,
        Url::parse(resource).expect("resource URL"),
    )
}

/// A fake provider server on a loopback port, stopped on drop.
#[cfg(any(
    feature = "host-s3",
    feature = "host-gcs",
    feature = "host-azure",
    feature = "host-netlify"
))]
pub struct FakeServer {
    pub base: String,
    task: tokio::task::JoinHandle<()>,
}

#[cfg(any(
    feature = "host-s3",
    feature = "host-gcs",
    feature = "host-azure",
    feature = "host-netlify"
))]
impl FakeServer {
    pub async fn start(router: axum::Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        Self { base, task }
    }
}

#[cfg(any(
    feature = "host-s3",
    feature = "host-gcs",
    feature = "host-azure",
    feature = "host-netlify"
))]
impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Percent-decodes a URL path.
#[cfg(any(
    feature = "host-s3",
    feature = "host-gcs",
    feature = "host-azure",
    feature = "host-netlify"
))]
pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("UTF-8 path")
}
