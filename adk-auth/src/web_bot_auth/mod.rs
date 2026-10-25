//! Web Bot Auth: verified identity for automated HTTP clients. Spec 3.5 §7.3, §4.4, A-I6.
//!
//! An agent signs each request with [HTTP Message Signatures] (RFC 9421) and names, in the
//! `Signature-Agent` field, the origin whose key directory publishes its key. A site verifies
//! the signature against that directory and learns a requester identity it can rely on, where
//! a `User-Agent` string is only a claim. This module implements the IETF Web Bot Auth working
//! group draft [draft-ietf-webbotauth-httpsig-protocol-00] and the key directory format of
//! [draft-meunier-http-message-signatures-directory-05], which the working-group draft absorbed,
//! for Ed25519 keys.
//!
//! | Part | Type |
//! |------|------|
//! | Verify a request; get `wba:{directory origin}#{key thumbprint}` | [`HttpSignatureVerifier`], [`VerifiedRequester`] |
//! | Sign an outgoing `http::Request` or `reqwest::Request` | [`HttpSignatureSigner`] |
//! | Resolve key directories, cache them, refuse private addresses | [`KeyDirectorySource`], [`HttpKeyDirectorySource`], [`InMemoryKeyDirectorySource`], [`DirectoryCachePolicy`] |
//! | Publish a site's own directory, with its possession proof | [`KeyDirectory`], [`DirectoryResponse`] |
//! | Keys and thumbprints | [`Ed25519SigningKey`], [`jwk_thumbprint`] |
//! | RFC 9421 signature base, RFC 8941 structured fields | [`RequestHead`], [`SignatureInput`], [`ComponentId`], [`structured`] |
//!
//! The verifier assigns no trust tier: a verified requester is evidence the AWP edge maps to a
//! tier (spec 3.5 §7.3). Private keys never appear in logs or `Debug` output.
//!
//! [HTTP Message Signatures]: https://www.rfc-editor.org/rfc/rfc9421
//! [draft-ietf-webbotauth-httpsig-protocol-00]: https://datatracker.ietf.org/doc/draft-ietf-webbotauth-httpsig-protocol/
//! [draft-meunier-http-message-signatures-directory-05]: https://datatracker.ietf.org/doc/draft-meunier-http-message-signatures-directory/
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_auth::web_bot_auth::{
//!     Ed25519SigningKey, HttpSignatureSigner, HttpSignatureVerifier, InMemoryKeyDirectorySource,
//!     KeyDirectory,
//! };
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! // The agent's operator publishes its key at https://crawler.example.
//! let key = Arc::new(Ed25519SigningKey::generate().unwrap());
//! let published = KeyDirectory::from_keys([key.as_ref()]).to_json();
//!
//! // The site resolves directories; here from memory instead of HTTPS.
//! let directories = Arc::new(InMemoryKeyDirectorySource::new());
//! let directory = KeyDirectory::parse(published.as_bytes(), 64).unwrap();
//! directories.insert("https://crawler.example", directory, None).await;
//! let verifier = HttpSignatureVerifier::builder(directories).build();
//!
//! let signer = HttpSignatureSigner::builder(Arc::clone(&key), "https://crawler.example").build().unwrap();
//! let mut request = http::Request::get("https://site.example/products?page=2").body(()).unwrap();
//! signer.sign_request(&mut request).unwrap();
//!
//! let requester = verifier.verify_request(&request).await.unwrap().expect("signed");
//! assert_eq!(requester.directory_origin, "https://crawler.example");
//! assert_eq!(requester.key_thumbprint, key.thumbprint());
//! # }
//! ```

mod directory;
mod error;
mod keys;
mod message;
mod signer;
pub mod structured;
mod verifier;

pub use directory::{
    DirectoryCachePolicy, FetchedDirectory, HttpKeyDirectorySource, HttpKeyDirectorySourceBuilder,
    InMemoryKeyDirectorySource, KeyDirectorySource,
};
pub use error::WebBotAuthError;
pub use keys::{DirectoryKey, DirectoryResponse, Ed25519SigningKey, KeyDirectory, jwk_thumbprint};
pub use message::{ComponentId, RequestHead, SignatureInput};
pub use signer::{HttpSignatureSigner, HttpSignatureSignerBuilder, SigningParams};
pub use verifier::{HttpSignatureVerifier, HttpSignatureVerifierBuilder, VerifiedRequester};

/// The `tag` of a Web Bot Auth request signature (draft §5.2).
pub const WEB_BOT_AUTH_TAG: &str = "web-bot-auth";

/// The `tag` of a directory response's possession proof (draft Appendix B.1).
pub const DIRECTORY_TAG: &str = "http-message-signatures-directory";

/// The key directory's media type (draft §8.2.1).
pub const DIRECTORY_MEDIA_TYPE: &str = "application/http-message-signatures-directory+json";

/// The key directory's well-known path (draft §8.1).
pub const DIRECTORY_WELL_KNOWN_PATH: &str = "/.well-known/http-message-signatures-directory";

/// The `Signature-Agent` field name, lowercase (draft §5.2.1).
pub const SIGNATURE_AGENT: &str = "signature-agent";

/// The `Signature-Input` field name, lowercase (RFC 9421 §4.1).
pub const SIGNATURE_INPUT: &str = "signature-input";

/// The `Signature` field name, lowercase (RFC 9421 §4.2).
pub const SIGNATURE: &str = "signature";

/// The values of every `name` field line, combined with `", "` (RFC 9110 §5.3), or `None`
/// when the request has none.
fn combined_field(
    headers: &http::HeaderMap,
    name: &str,
) -> Result<Option<String>, WebBotAuthError> {
    let values = headers
        .get_all(name)
        .iter()
        .map(|value| {
            value.to_str().map_err(|_| WebBotAuthError::MalformedField {
                field: name.to_string(),
                reason: "the value is not ASCII".into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((!values.is_empty()).then(|| values.join(", ")))
}
