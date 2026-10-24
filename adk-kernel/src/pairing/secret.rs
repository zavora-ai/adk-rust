//! Pairing codes, device tokens and their hashes (spec 3.3 §10 K2, S-I7).
//!
//! A pairing code is eight characters of Crockford base32 (40 bits), shown as `ABCD-2345`;
//! the stores keep only its SHA-256. A device token is `adkd_<id>_<secret>`, the secret 32
//! random bytes in hex; the stores keep only the secret's SHA-256, compared in constant time.

use sha2::{Digest as _, Sha256};

use crate::error::{KernelError, Result};

/// Crockford's base32 alphabet: no `I`, `L`, `O` or `U`, so a code read aloud or typed from a
/// screen has no look-alike characters.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The prefix every device token starts with, so a token is told from other bearer tokens.
pub(crate) const TOKEN_PREFIX: &str = "adkd_";

/// `len` random bytes from the operating system's generator.
pub(crate) fn random_bytes(len: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|error| KernelError::Config {
        reason: format!("the system random number generator failed: {error}"),
    })?;
    Ok(bytes)
}

/// `bytes` as lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The SHA-256 of `text`, in hex: what the stores keep of a code or a token.
pub(crate) fn digest(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// Whether `a` and `b` are equal, in time independent of where they differ.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// A new pairing code, shown as `XXXX-XXXX`.
pub(crate) fn new_code() -> Result<String> {
    let bytes = random_bytes(5)?;
    let bits = bytes.iter().fold(0u64, |bits, byte| (bits << 8) | u64::from(*byte));
    let mut code = String::with_capacity(9);
    for index in 0..8 {
        if index == 4 {
            code.push('-');
        }
        let shift = 35 - index * 5;
        code.push(char::from(ALPHABET[((bits >> shift) & 0x1f) as usize]));
    }
    Ok(code)
}

/// `code` as it is hashed: upper case, without separators, with the look-alikes Crockford
/// folds (`O` → `0`, `I` and `L` → `1`).
pub(crate) fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| !matches!(c, '-' | ' ' | '_' | '.'))
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect()
}

/// The SHA-256 of a code as typed, after [`normalize_code`].
pub(crate) fn code_hash(code: &str) -> String {
    digest(&normalize_code(code))
}

/// A new device id: sixteen hex digits.
pub(crate) fn new_device_id() -> Result<String> {
    Ok(hex(&random_bytes(8)?))
}

/// A new device token for `id`, and the hash of its secret the store keeps.
pub(crate) fn new_token(id: &str) -> Result<(String, String)> {
    let secret = hex(&random_bytes(32)?);
    let hash = digest(&secret);
    Ok((format!("{TOKEN_PREFIX}{id}_{secret}"), hash))
}

/// The device id and secret of a device token, or `None` for a token of another kind.
pub(crate) fn parse_token(token: &str) -> Option<(&str, &str)> {
    let rest = token.trim().strip_prefix(TOKEN_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    (!id.is_empty() && !secret.is_empty()).then_some((id, secret))
}

/// `text` with control characters removed and at most `max` characters: a name a device or
/// a chat platform supplied, which is untrusted input (spec 3.3 S-I6, S-I9).
pub(crate) fn clean_name(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_eight_crockford_characters() {
        for _ in 0..200 {
            let code = new_code().unwrap();
            assert_eq!(code.len(), 9, "{code}");
            assert_eq!(&code[4..5], "-");
            assert!(code.chars().filter(|c| *c != '-').all(|c| ALPHABET.contains(&(c as u8))));
        }
    }

    #[test]
    fn typed_codes_hash_as_shown() {
        assert_eq!(code_hash("ab1d-o2l5"), code_hash("AB1D0215"));
        assert_eq!(code_hash("AB1D 0215"), code_hash("ab1d-0215"));
        assert_ne!(code_hash("AB1D-0215"), code_hash("AB1D-0216"));
    }

    #[test]
    fn tokens_parse_and_hash() {
        let id = new_device_id().unwrap();
        let (token, hash) = new_token(&id).unwrap();
        let (parsed, secret) = parse_token(&token).unwrap();
        assert_eq!(parsed, id);
        assert!(constant_time_eq(digest(secret).as_bytes(), hash.as_bytes()));
        assert_eq!(parse_token("Bearer-xyz"), None);
        assert_eq!(parse_token("adkd_"), None);
        assert_eq!(parse_token("adkd_abc_"), None);
    }

    #[test]
    fn names_lose_control_characters() {
        assert_eq!(clean_name("Ana's\u{1b}[31m phone\n", 64), "Ana's[31m phone");
        assert_eq!(clean_name("abcdef", 3), "abc");
    }
}
