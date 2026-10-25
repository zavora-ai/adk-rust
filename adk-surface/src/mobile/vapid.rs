//! The kernel's own push key and the VAPID token it signs (spec 3.3 §5.4; RFC 8292).
//!
//! The key is a P-256 key kept in the kernel home, readable by its owner only, so pushes come
//! from the person's own kernel and no relay holds a key. A VAPID token is an ES256 JWT for one
//! push service origin, valid 12 hours, re-signed at most once an hour per origin.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use adk_core::Clock;
use base64ct::{Base64UrlUnpadded, Encoding as _};
use chrono::{DateTime, Duration, Utc};
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use sha2::{Digest as _, Sha256};

use super::error::MobileError;

/// How long a VAPID token is valid.
const TOKEN_LIFETIME: Duration = Duration::hours(12);
/// How long a signed token is reused for its origin.
const RESIGN_AFTER: Duration = Duration::hours(1);

/// The kernel's push key (spec 3.3 §5.4): a P-256 key, kept at `<home>/push.key`, that signs
/// the VAPID token of every push and that phones subscribe with. Its `Debug` shows only its
/// id.
///
/// Replicas share one key with `adk-rust up --push-key <file>`; each subscription remembers the
/// id of the key it was made with, and a replica sends only with that key.
///
/// # Example
///
/// ```rust
/// use adk_surface::mobile::PushKey;
///
/// let home = tempfile::tempdir().unwrap();
/// let path = home.path().join(PushKey::FILE);
/// let key = PushKey::load_or_create(&path).unwrap();
/// assert_eq!(key.id(), PushKey::load_or_create(&path).unwrap().id());
/// assert_eq!(format!("{key:?}"), format!("PushKey({})", key.id()));
/// ```
#[derive(Clone)]
pub struct PushKey {
    signing: SigningKey,
    public: String,
    id: String,
}

impl fmt::Debug for PushKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PushKey({})", self.id)
    }
}

impl PushKey {
    /// The key's file name in the kernel home.
    pub const FILE: &'static str = "push.key";

    /// A new random key from the operating system's generator.
    ///
    /// # Errors
    ///
    /// Returns [`MobileError::PushKey`] when the generator is unavailable.
    pub fn generate() -> Result<Self, MobileError> {
        loop {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|error| MobileError::PushKey {
                reason: format!("the operating system's random source failed: {error}"),
            })?;
            // A value outside the curve's order is astronomically rare; draw again.
            if let Ok(key) = Self::from_bytes(bytes) {
                return Ok(key);
            }
        }
    }

    /// The key with this secret scalar.
    ///
    /// # Errors
    ///
    /// Returns [`MobileError::PushKey`] when the bytes are not a P-256 secret key.
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, MobileError> {
        let signing = SigningKey::from_bytes(&bytes.into()).map_err(|_| MobileError::PushKey {
            reason: "the bytes are not a P-256 secret key".into(),
        })?;
        let point = signing.verifying_key().to_encoded_point(false);
        let public = Base64UrlUnpadded::encode_string(point.as_bytes());
        let digest = Sha256::digest(point.as_bytes());
        let id = Base64UrlUnpadded::encode_string(&digest[..12]);
        Ok(Self { signing, public, id })
    }

    /// The key in the file at `path`, or a new one written there, readable only by its owner
    /// on Unix (mode `0600`). The file holds 64 hex digits. An empty file gets a new key and
    /// keeps its permissions, so a caller can create it owner-only first.
    ///
    /// # Errors
    ///
    /// Returns [`MobileError::PushKey`] when the file cannot be read or created, or does not
    /// hold a key.
    pub fn load_or_create(path: &Path) -> Result<Self, MobileError> {
        let unusable = |error: std::io::Error| MobileError::PushKey {
            reason: format!("{}: {error}", path.display()),
        };
        match std::fs::read_to_string(path) {
            Ok(text) if text.trim().is_empty() => {
                let key = Self::generate()?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(path)
                    .map_err(unusable)?;
                key.write_to(&mut file).map_err(unusable)?;
                return Ok(key);
            }
            Ok(text) => return Self::parse(path, &text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(unusable(error)),
        }
        let key = Self::generate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(unusable)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(path) {
            Ok(mut file) => {
                key.write_to(&mut file).map_err(unusable)?;
                Ok(key)
            }
            // Another kernel created it first: use that one.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::parse(path, &std::fs::read_to_string(path).map_err(unusable)?)
            }
            Err(error) => Err(unusable(error)),
        }
    }

    fn write_to(&self, file: &mut std::fs::File) -> std::io::Result<()> {
        use std::io::Write as _;
        let bytes = self.signing.to_bytes();
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        writeln!(file, "{hex}")?;
        file.sync_all()
    }

    fn parse(path: &Path, text: &str) -> Result<Self, MobileError> {
        let text = text.trim();
        let invalid = || MobileError::PushKey {
            reason: format!("{} does not hold 64 hex digits", path.display()),
        };
        if text.len() != 64 || !text.is_ascii() {
            return Err(invalid());
        }
        let mut bytes = [0u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte =
                u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| invalid())?;
        }
        Self::from_bytes(bytes)
    }

    /// The key's id: what a subscription records, so a replica sends only with its key.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The public key a phone subscribes with (`applicationServerKey`): the uncompressed point,
    /// base64url without padding.
    pub fn public_key(&self) -> &str {
        &self.public
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature: Signature = self.signing.sign(message);
        signature.to_bytes().to_vec()
    }
}

/// Signs and caches the VAPID token of each push service origin.
#[derive(Clone)]
pub(crate) struct Vapid {
    key: PushKey,
    subject: String,
    clock: Arc<dyn Clock>,
    signed: Arc<Mutex<Signed>>,
}

/// Each origin's token and when it was signed.
type Signed = HashMap<String, (String, DateTime<Utc>)>;

impl fmt::Debug for Vapid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vapid").field("key", &self.key).field("subject", &self.subject).finish()
    }
}

impl Vapid {
    /// A signer with `key`, naming `subject` (a `mailto:` or https URL) as the contact.
    pub(crate) fn new(key: PushKey, subject: String, clock: Arc<dyn Clock>) -> Self {
        Self { key, subject, clock, signed: Arc::default() }
    }

    pub(crate) fn key(&self) -> &PushKey {
        &self.key
    }

    /// The `Authorization` header of a push to `origin` (`https://host[:port]`).
    pub(crate) fn authorization(&self, origin: &str) -> String {
        format!("vapid t={}, k={}", self.token(origin), self.key.public_key())
    }

    /// The token for `origin`, signed again once the cached one is an hour old.
    fn token(&self, origin: &str) -> String {
        let now = self.clock.now();
        let mut signed = self.signed.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((token, at)) = signed.get(origin)
            && now - *at < RESIGN_AFTER
            && now >= *at
        {
            return token.clone();
        }
        let header = Base64UrlUnpadded::encode_string(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({
            "aud": origin,
            "exp": (now + TOKEN_LIFETIME).timestamp(),
            "sub": self.subject,
        });
        let claims = Base64UrlUnpadded::encode_string(claims.to_string().as_bytes());
        let input = format!("{header}.{claims}");
        let signature = Base64UrlUnpadded::encode_string(&self.key.sign(input.as_bytes()));
        let token = format!("{input}.{signature}");
        signed.insert(origin.to_string(), (token.clone(), now));
        token
    }
}

#[cfg(test)]
mod tests {
    use adk_core::ManualClock;
    use chrono::TimeZone as _;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier as _;

    use super::*;

    fn clock() -> Arc<ManualClock> {
        Arc::new(ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 24, 18, 0, 0).unwrap()))
    }

    /// The signed input, the claims, the signature and the public key of a header.
    fn parts(header: &str) -> (String, serde_json::Value, Vec<u8>, String) {
        let (token, key) = header
            .strip_prefix("vapid t=")
            .and_then(|rest| rest.split_once(", k="))
            .expect("vapid t=…, k=…");
        let (input, signature) = token.rsplit_once('.').unwrap();
        let claims = input.split_once('.').unwrap().1;
        let claims =
            serde_json::from_slice(&Base64UrlUnpadded::decode_vec(claims).unwrap()).unwrap();
        (
            input.to_string(),
            claims,
            Base64UrlUnpadded::decode_vec(signature).unwrap(),
            key.to_string(),
        )
    }

    /// The token is ES256 over its header and claims, for the push service's origin, valid
    /// 12 hours, naming the contact (RFC 8292 §2).
    #[test]
    fn a_token_is_es256_for_the_origin() {
        let clock = clock();
        let key = PushKey::generate().unwrap();
        let vapid = Vapid::new(key.clone(), "https://home.example.ts.net".into(), clock.clone());
        let (input, claims, signature, public) =
            parts(&vapid.authorization("https://web.push.apple.com"));
        assert_eq!(public, key.public_key());
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert_eq!(claims["sub"], "https://home.example.ts.net");
        assert_eq!(claims["exp"], (clock.now() + Duration::hours(12)).timestamp());
        let verifying = VerifyingKey::from_sec1_bytes(
            &Base64UrlUnpadded::decode_vec(key.public_key()).unwrap(),
        )
        .unwrap();
        let signature = Signature::from_slice(&signature).unwrap();
        verifying.verify(input.as_bytes(), &signature).unwrap();
    }

    /// One signature per origin per hour.
    #[test]
    fn a_token_is_reused_for_an_hour_per_origin() {
        let clock = clock();
        let vapid = Vapid::new(
            PushKey::generate().unwrap(),
            "mailto:ana@example.com".into(),
            clock.clone(),
        );
        let apple = vapid.authorization("https://web.push.apple.com");
        let google = vapid.authorization("https://fcm.googleapis.com");
        assert_ne!(apple, google);
        clock.advance(std::time::Duration::from_secs(59 * 60));
        assert_eq!(vapid.authorization("https://web.push.apple.com"), apple);
        clock.advance(std::time::Duration::from_secs(60));
        assert_ne!(vapid.authorization("https://web.push.apple.com"), apple);
    }

    #[test]
    fn a_key_file_that_is_not_a_key_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(PushKey::FILE);
        std::fs::write(&path, "not a key\n").unwrap();
        let error = PushKey::load_or_create(&path).unwrap_err();
        assert!(matches!(error, MobileError::PushKey { .. }), "{error:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_key_file_is_its_owner_s_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(PushKey::FILE);
        PushKey::load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
