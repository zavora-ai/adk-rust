//! Private key files: an RFC 8037 Ed25519 JWK carrying its AWP key id, created owner-only
//! (spec 3.5 §7.2, D-11, A-I5).
//!
//! The root key file stays on the owner's machine; delegated keys — the release key a site
//! signs its log with — live in the site's `data/keys/`. A key never reaches a log, an error
//! or `Debug` output: only its key id does.

use std::path::Path;

use adk_auth::web_bot_auth::Ed25519SigningKey;
use adk_awp::signing::jws::Ed25519DocumentSigner;
use anyhow::{Context, Result, bail};
use awp_types::PublicKeyJwk;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use zeroize::Zeroizing;

#[derive(Deserialize)]
struct PrivateJwk {
    kty: String,
    crv: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    d: Option<String>,
}

/// An Ed25519 private key with its AWP key id.
pub(crate) struct KeyFile {
    kid: String,
    seed: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for KeyFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyFile").field("kid", &self.kid).finish_non_exhaustive()
    }
}

impl KeyFile {
    /// A fresh key named `kid`, from the system's secure random source.
    pub(crate) fn generate(kid: &str) -> Result<Self> {
        let key = Ed25519SigningKey::generate().context("no secure randomness for a new key")?;
        let mut file = Self::parse(&key.to_private_jwk(), "the generated key")?;
        file.kid = check_kid(kid)?;
        Ok(file)
    }

    /// Reads a private JWK file.
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let text = Zeroizing::new(
            std::fs::read_to_string(path)
                .with_context(|| format!("cannot read the key file {}", path.display()))?,
        );
        Self::parse(&text, &path.display().to_string())
    }

    fn parse(text: &str, origin: &str) -> Result<Self> {
        let jwk: PrivateJwk = serde_json::from_str(text)
            .map_err(|_| anyhow::anyhow!("{origin} is not a JSON Web Key"))?;
        if jwk.kty != "OKP" || jwk.crv != "Ed25519" {
            bail!("{origin} is not an Ed25519 key (kty OKP, crv Ed25519)");
        }
        let d = Zeroizing::new(jwk.d.unwrap_or_default());
        if d.is_empty() {
            bail!("{origin} holds a public key only; a private key file carries `d`");
        }
        let bytes = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(d.as_bytes())
                .map_err(|_| anyhow::anyhow!("{origin}: `d` is not base64url"))?,
        );
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("{origin}: `d` is not a 32-byte Ed25519 seed"))?;
        let file = Self {
            kid: check_kid(jwk.kid.as_deref().unwrap_or_default())
                .with_context(|| format!("{origin}: the key file names no valid `kid`"))?,
            seed: Zeroizing::new(seed),
        };
        if jwk.x.is_some_and(|x| x != file.public_key().x) {
            bail!("{origin}: `x` does not match `d`");
        }
        Ok(file)
    }

    /// The key id.
    pub(crate) fn kid(&self) -> &str {
        &self.kid
    }

    /// The public key.
    pub(crate) fn public_key(&self) -> PublicKeyJwk {
        PublicKeyJwk::ed25519(URL_SAFE_NO_PAD.encode(self.web_bot_auth_public()))
    }

    fn web_bot_auth_public(&self) -> [u8; 32] {
        Ed25519SigningKey::from_seed(*self.seed).map(|key| *key.public_key()).unwrap_or_default()
    }

    /// The key as an AWP document signer.
    pub(crate) fn document_signer(&self) -> Result<Ed25519DocumentSigner> {
        Ed25519DocumentSigner::from_seed(self.kid.clone(), *self.seed)
            .map_err(|error| anyhow::anyhow!("key `{}`: {error}", self.kid))
    }

    /// The key as the release key of `domain`'s site log.
    pub(crate) fn release_signer(
        &self,
        domain: &str,
    ) -> Result<adk_site::signer::Ed25519ReleaseSigner> {
        adk_site::signer::Ed25519ReleaseSigner::from_seed(domain, *self.seed)
            .map_err(|error| anyhow::anyhow!("release key `{}`: {error}", self.kid))
    }

    /// The key as a Web Bot Auth signing key.
    pub(crate) fn web_bot_auth(&self) -> Result<Ed25519SigningKey> {
        Ed25519SigningKey::from_seed(*self.seed)
            .map_err(|error| anyhow::anyhow!("key `{}`: {error}", self.kid))
    }

    /// Whether this key is `public`.
    pub(crate) fn is(&self, public: &PublicKeyJwk) -> bool {
        self.public_key() == *public
    }

    /// Writes the key to `path`, which must not exist, readable by its owner only.
    pub(crate) fn write_new(&self, path: &Path) -> Result<()> {
        use std::io::Write as _;
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let x = self.public_key().x;
        let d = Zeroizing::new(URL_SAFE_NO_PAD.encode(self.seed.as_ref()));
        let text = Zeroizing::new(format!(
            "{{\"kty\":\"OKP\",\"crv\":\"Ed25519\",\"kid\":{},\"x\":\"{x}\",\"d\":\"{}\"}}\n",
            serde_json::Value::String(self.kid.clone()),
            d.as_str()
        ));
        let mut file = create_owner_only(path)?;
        file.write_all(text.as_bytes()).with_context(|| format!("cannot write {}", path.display()))
    }
}

/// 32 bytes from the system's secure random source, as an Ed25519 seed is drawn.
pub(crate) fn random_secret() -> Result<Zeroizing<[u8; 32]>> {
    Ok(KeyFile::generate("secret")?.seed)
}

/// A key id is printable ASCII without whitespace or `+`, as signed-note key names are.
fn check_kid(kid: &str) -> Result<String> {
    let valid = !kid.is_empty()
        && kid.len() <= 128
        && kid.bytes().all(|b| b.is_ascii_graphic() && b != b'+');
    if !valid {
        bail!("`{kid}` is not a key id; use letters, digits and `-`, such as site-root-2026");
    }
    Ok(kid.to_owned())
}

/// Creates `path` for writing, failing when it exists, with mode `0600` on Unix and an
/// owner-only DACL on Windows (spec 3.2 §10 W4).
pub(crate) fn create_owner_only(path: &Path) -> Result<std::fs::File> {
    #[cfg(not(windows))]
    let created = {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    };
    #[cfg(windows)]
    let created = crate::owner_only::create_new(path);
    created.with_context(|| {
        format!("cannot create {}; a key file is never overwritten", path.display())
    })
}

#[cfg(test)]
mod tests {
    use adk_awp::signing::jws::DocumentSigner as _;

    use super::*;

    #[test]
    fn a_key_file_round_trips_and_never_shows_its_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys/root.jwk");
        let key = KeyFile::generate("site-root-2026").unwrap();
        key.write_new(&path).unwrap();
        let read = KeyFile::read(&path).unwrap();
        assert_eq!((read.kid(), read.public_key()), (key.kid(), key.public_key()));
        assert_eq!(
            read.document_signer().unwrap().public_key(),
            key.document_signer().unwrap().public_key()
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let d = serde_json::from_str::<serde_json::Value>(&text).unwrap()["d"].clone();
        assert!(!format!("{read:?}").contains(d.as_str().unwrap()));
        assert!(key.write_new(&path).is_err(), "a key file is never overwritten");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn malformed_key_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k.jwk");
        let key = KeyFile::generate("k1").unwrap();
        for text in [
            "not json".to_string(),
            r#"{"kty":"RSA","crv":"Ed25519","kid":"k","d":"AA"}"#.to_string(),
            format!(r#"{{"kty":"OKP","crv":"Ed25519","kid":"k","x":"{}"}}"#, key.public_key().x),
            r#"{"kty":"OKP","crv":"Ed25519","kid":"bad kid","d":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#
                .to_string(),
            r#"{"kty":"OKP","crv":"Ed25519","kid":"k","x":"AAAA","d":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#
                .to_string(),
        ] {
            std::fs::write(&path, &text).unwrap();
            assert!(KeyFile::read(&path).is_err(), "{text}");
        }
        assert!(KeyFile::generate("has+plus").is_err());
    }
}
