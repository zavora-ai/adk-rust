//! [`Digest`]: the `sha256:<hex>` identity that travels instead of pixels.

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

use crate::error::{Result, VisionError};

/// A SHA-256 digest in the form `sha256:<64 lowercase hex digits>`.
///
/// Images are identified by the digest of their encoded bytes; approvals, caches, ledgers,
/// audit records and wake payloads carry the digest, never the pixels (V-I15).
///
/// # Example
///
/// ```
/// use adk_vision::Digest;
///
/// let digest = Digest::of(b"abc");
/// assert_eq!(
///     digest.to_string(),
///     "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
/// );
/// assert_eq!(digest.to_string().parse::<Digest>().unwrap(), digest);
/// assert!("md5:abc".parse::<Digest>().is_err());
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

impl Digest {
    /// The digest of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// The digest of a value's canonical JSON form: object keys sorted, no whitespace.
    ///
    /// Used to identify operations (`Provenance::Derived::operation`), so the same edit of the
    /// same image always has the same digest.
    pub fn of_json<T: Serialize>(value: &T) -> Self {
        let value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        Self::of(canonical_json(&value).as_bytes())
    }

    /// Streams a file through SHA-256 without holding it in memory.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::Io`] when the file cannot be read.
    pub fn of_file(path: &Path) -> Result<(Self, u64)> {
        let mut file =
            std::fs::File::open(path).map_err(|e| VisionError::io("could not open", path, e))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let read =
                file.read(&mut buffer).map_err(|e| VisionError::io("could not read", path, e))?;
            if read == 0 {
                break;
            }
            size += read as u64;
            hasher.update(&buffer[..read]);
        }
        Ok((Self(hasher.finalize().into()), size))
    }

    /// Parses 64 hex digits, with or without the `sha256:` prefix.
    ///
    /// # Errors
    ///
    /// Returns [`VisionError::InvalidInput`] for anything else.
    pub fn from_hex(hex_digits: &str) -> Result<Self> {
        let digits = hex_digits.strip_prefix("sha256:").unwrap_or(hex_digits);
        if digits.len() != 64 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(VisionError::InvalidInput(format!(
                "`{hex_digits}` is not a SHA-256 digest; expected `sha256:` and 64 hex digits"
            )));
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(digits.to_ascii_lowercase(), &mut bytes)
            .map_err(|e| VisionError::InvalidInput(format!("`{hex_digits}`: {e}")))?;
        Ok(Self(bytes))
    }

    /// The 64 lowercase hex digits, without the prefix.
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }

    /// The raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

impl FromStr for Digest {
    type Err = VisionError;

    fn from_str(s: &str) -> Result<Self> {
        if !s.starts_with("sha256:") {
            return Err(VisionError::InvalidInput(format!(
                "`{s}` is not a digest; expected `sha256:` and 64 hex digits"
            )));
        }
        Self::from_hex(s)
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Serializes `value` with object keys sorted and no insignificant whitespace.
pub(crate) fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|key| {
                    let name = serde_json::Value::String(key.clone()).to_string();
                    format!("{name}:{}", canonical_json(&map[key]))
                })
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        serde_json::Value::Array(items) => {
            let items: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_digests_ignore_key_order() {
        let a = serde_json::json!({"b": 1, "a": [1, {"y": 2, "x": 1}]});
        let b = serde_json::json!({"a": [1, {"x": 1, "y": 2}], "b": 1});
        assert_eq!(Digest::of_json(&a), Digest::of_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":[1,{"x":1,"y":2}],"b":1}"#);
    }

    #[test]
    fn upper_case_hex_parses_to_the_same_digest() {
        let digest = Digest::of(b"x");
        let upper = format!("sha256:{}", digest.hex().to_uppercase());
        assert_eq!(upper.parse::<Digest>().unwrap(), digest);
        assert!(Digest::from_hex("abc").is_err());
    }

    #[test]
    fn files_hash_like_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"hello").unwrap();
        assert_eq!(Digest::of_file(&path).unwrap(), (Digest::of(b"hello"), 5));
    }
}
