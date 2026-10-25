//! TSIG, RFC 8945: HMAC-SHA256 signatures over DNS messages (spec 3.5 §7.13; feature
//! `rfc2136`).
//!
//! The RFC 2136 adapter signs its UPDATE and AXFR requests and verifies every response,
//! so neither a change nor a zone read is accepted from anyone without the key. The
//! functions work on wire bytes, so a test server signs and verifies with them too.

use base64::Engine as _;
use hickory_proto::op::Message;
use hickory_proto::rr::rdata::tsig::{TSIG, TsigAlgorithm, make_tsig_record};
use hickory_proto::rr::{Name, RData};
use hickory_proto::serialize::binary::{BinEncodable, BinEncoder};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::{DriverError, Result};

/// How far a signature's time may be from the verifier's clock, in seconds (RFC 8945
/// recommends 300).
pub const FUDGE: u16 = 300;

const SERVICE: &str = "dns";

/// A TSIG key: its name and HMAC-SHA256 secret (RFC 8945).
///
/// The secret never appears in `Debug`, errors or logs.
///
/// # Example
///
/// ```rust
/// use adk_drivers::dns::tsig::TsigKey;
///
/// let key = TsigKey::hmac_sha256("update-key", "c2VjcmV0LXNlY3JldC1zZWNyZXQtc2VjcmV0IQ==").unwrap();
/// assert_eq!(key.name(), "update-key.");
/// assert!(!format!("{key:?}").contains("c2Vj"));
/// ```
#[derive(Clone)]
pub struct TsigKey {
    name: Name,
    secret: Vec<u8>,
}

impl std::fmt::Debug for TsigKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsigKey").field("name", &self.name.to_ascii()).finish_non_exhaustive()
    }
}

impl TsigKey {
    /// The HMAC-SHA256 key `name` with the base64 `secret`, as `tsig-keygen` writes it.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a name that is not a domain name or a secret
    /// that is not base64 of at least 16 bytes.
    pub fn hmac_sha256(name: &str, secret: &str) -> Result<Self> {
        let mut name = Name::from_ascii(name).map_err(|error| {
            DriverError::invalid(format!("the TSIG key name is not a domain name: {error}"))
        })?;
        name.set_fqdn(true);
        let secret = base64::engine::general_purpose::STANDARD
            .decode(secret.trim())
            .map_err(|_| DriverError::invalid("the TSIG secret is not base64"))?;
        if secret.len() < 16 {
            return Err(DriverError::invalid("the TSIG secret is shorter than 16 bytes"));
        }
        Ok(Self { name: name.to_lowercase(), secret })
    }

    /// The key's name, fully qualified.
    pub fn name(&self) -> String {
        self.name.to_ascii()
    }

    fn mac(&self, data: &[u8]) -> Vec<u8> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC takes a key of any length");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    /// Signs the unsigned message `wire` at `time` (seconds since the epoch): a request with
    /// `prior_mac` `None`, a response with the request's MAC. Returns the signed message and
    /// its MAC.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for bytes that are not a DNS message.
    pub fn sign(
        &self,
        wire: &[u8],
        prior_mac: Option<&[u8]>,
        time: u64,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        if wire.len() < 12 {
            return Err(DriverError::invalid("a DNS message is at least 12 bytes"));
        }
        let id = u16::from_be_bytes([wire[0], wire[1]]);
        let unsigned =
            TSIG::new(TsigAlgorithm::HmacSha256, time, FUDGE, Vec::new(), id, None, Vec::new());
        let mut tbs = Vec::with_capacity(wire.len() + 128);
        if let Some(prior) = prior_mac {
            tbs.extend_from_slice(&(prior.len() as u16).to_be_bytes());
            tbs.extend_from_slice(prior);
        }
        tbs.extend_from_slice(wire);
        emit_variables(&mut tbs, &unsigned, &self.name)?;
        let mac = self.mac(&tbs);
        let record = make_tsig_record(self.name.clone(), unsigned.set_mac(mac.clone()));
        let tail = record.to_bytes().map_err(|error| DriverError::invalid(error.to_string()))?;
        let mut signed = wire.to_vec();
        let additionals = u16::from_be_bytes([signed[10], signed[11]]).saturating_add(1);
        signed[10..12].copy_from_slice(&additionals.to_be_bytes());
        signed.extend_from_slice(&tail);
        Ok((signed, mac))
    }

    /// Verifies the signed message `wire` at `now`: `prior_mac` is the request's MAC for a
    /// response, or the previous message's for the later messages of a zone transfer, whose
    /// `first` is false. Returns the message's MAC.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Unauthorized`] for an unsigned message, another key, another
    /// algorithm, a signature error the peer reported, a time outside the fudge, or a MAC
    /// that does not match.
    pub fn verify(
        &self,
        wire: &[u8],
        prior_mac: Option<&[u8]>,
        first: bool,
        now: u64,
    ) -> Result<Vec<u8>> {
        let refused = |reason: &str| DriverError::Unauthorized {
            service: SERVICE.into(),
            reason: reason.into(),
        };
        let message = Message::from_vec(wire)
            .map_err(|error| DriverError::protocol(SERVICE, error.to_string()))?;
        // hickory reads the TSIG record into `signature` when its DNSSEC features are on,
        // and leaves it as the last additional record otherwise.
        let (signer, tsig) = match message.signature {
            Some(record) => (record.name.clone(), record.data.clone()),
            None => match message.additionals.last() {
                Some(record) => match &record.data {
                    RData::TSIG(tsig) => (record.name.clone(), tsig.clone()),
                    _ => return Err(refused("the message is not signed with TSIG")),
                },
                None => return Err(refused("the message is not signed with TSIG")),
            },
        };
        if signer.to_lowercase() != self.name {
            return Err(refused("the message is signed with another TSIG key"));
        }
        let tsig = &tsig;
        if tsig.algorithm != TsigAlgorithm::HmacSha256 {
            return Err(refused("the message is signed with an algorithm other than hmac-sha256"));
        }
        if let Some(error) = &tsig.error {
            return Err(refused(&format!("the peer reported TSIG error {error:?}")));
        }
        if now.abs_diff(tsig.time) > u64::from(tsig.fudge) {
            return Err(refused("the TSIG time is outside the allowed fudge; check both clocks"));
        }
        let start = last_record_start(wire)
            .ok_or_else(|| DriverError::protocol(SERVICE, "the signed message is truncated"))?;
        let mut stripped = wire[..start].to_vec();
        stripped[0..2].copy_from_slice(&tsig.oid.to_be_bytes());
        let additionals = u16::from_be_bytes([stripped[10], stripped[11]]).saturating_sub(1);
        stripped[10..12].copy_from_slice(&additionals.to_be_bytes());
        let mut tbs = Vec::with_capacity(wire.len() + 64);
        if let Some(prior) = prior_mac {
            tbs.extend_from_slice(&(prior.len() as u16).to_be_bytes());
            tbs.extend_from_slice(prior);
        }
        tbs.extend_from_slice(&stripped);
        if first {
            emit_variables(&mut tbs, tsig, &signer)?;
        } else {
            tbs.extend_from_slice(&((tsig.time >> 32) as u16).to_be_bytes());
            tbs.extend_from_slice(&(tsig.time as u32).to_be_bytes());
            tbs.extend_from_slice(&tsig.fudge.to_be_bytes());
        }
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC takes a key of any length");
        mac.update(&tbs);
        mac.verify_slice(&tsig.mac).map_err(|_| refused("the TSIG MAC does not match"))?;
        Ok(tsig.mac.clone())
    }
}

/// Appends the TSIG variables of RFC 8945 §4.3.3 for `tsig` signed by `key`.
fn emit_variables(out: &mut Vec<u8>, tsig: &TSIG, key: &Name) -> Result<()> {
    // A `BinEncoder` writes from the start of its buffer, so the variables are encoded
    // apart and appended.
    let mut variables = Vec::with_capacity(64);
    let mut encoder = BinEncoder::new(&mut variables);
    tsig.emit_tsig_for_mac(&mut encoder, key)
        .map_err(|error| DriverError::invalid(error.to_string()))?;
    out.extend_from_slice(&variables);
    Ok(())
}

/// The offset of the last resource record of the DNS message `wire`, the TSIG record of a
/// signed message.
pub(crate) fn last_record_start(wire: &[u8]) -> Option<usize> {
    let count = |at: usize| -> Option<usize> {
        Some(usize::from(u16::from_be_bytes([*wire.get(at)?, *wire.get(at + 1)?])))
    };
    let questions = count(4)?;
    let records = count(6)? + count(8)? + count(10)?;
    if records == 0 {
        return None;
    }
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(wire, at)? + 4;
    }
    let mut last = at;
    for _ in 0..records {
        last = at;
        at = skip_name(wire, at)?;
        let length = usize::from(u16::from_be_bytes([*wire.get(at + 8)?, *wire.get(at + 9)?]));
        at += 10 + length;
        if at > wire.len() {
            return None;
        }
    }
    Some(last)
}

/// The offset after the name at `at`, compressed or not.
fn skip_name(wire: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let length = *wire.get(at)?;
        match length {
            0 => return Some(at + 1),
            length if length & 0xC0 == 0xC0 => return Some(at + 2),
            length => at += 1 + usize::from(length),
        }
    }
}

/// The time now, in seconds since the epoch.
pub(crate) fn now() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp()).unwrap_or_default()
}
