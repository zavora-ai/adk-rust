//! A software passkey authenticator for the phone's tests (spec 3.3 §8.2): an ES256 key with
//! attestation `none`, answering WebAuthn creation and request options as a phone's platform
//! authenticator does, with the knobs a refusal test turns.

// Several test binaries include this file, each using part of it.
#![allow(dead_code)]

use base64ct::{Base64UrlUnpadded, Encoding as _};
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

/// A platform authenticator holding one passkey.
#[derive(Clone)]
pub struct Authenticator {
    key: SigningKey,
    credential_id: Vec<u8>,
    /// The origin its browser reports.
    pub origin: String,
    /// The RP id whose hash its authenticator data carries.
    pub rp_id: String,
    /// Whether it verified the person (face, fingerprint or passcode).
    pub user_verified: bool,
    /// Its signature counter.
    pub counter: u32,
    /// Whether it moves its counter on each use; Apple's passkeys report 0 every time.
    pub counting: bool,
}

impl Authenticator {
    /// A new passkey for `origin` and `rp_id`, its counter at 0 and not counting.
    pub fn new(origin: &str, rp_id: &str) -> Self {
        let mut seed = [0u8; 32];
        let mut id = [0u8; 16];
        getrandom::fill(&mut seed).unwrap();
        getrandom::fill(&mut id).unwrap();
        Self {
            key: SigningKey::from_bytes(&seed.into()).unwrap(),
            credential_id: id.to_vec(),
            origin: origin.to_string(),
            rp_id: rp_id.to_string(),
            user_verified: true,
            counter: 0,
            counting: false,
        }
    }

    /// Moves its counter from `start` on each use.
    #[must_use]
    pub fn counting_from(mut self, start: u32) -> Self {
        self.counter = start;
        self.counting = true;
        self
    }

    /// The same passkey in a browser that reports `origin`.
    pub fn with_origin(&self, origin: &str) -> Self {
        Self { origin: origin.to_string(), ..self.clone() }
    }

    /// The same passkey signing for `rp_id`.
    pub fn with_rp_id(&self, rp_id: &str) -> Self {
        Self { rp_id: rp_id.to_string(), ..self.clone() }
    }

    /// The same passkey, the person not verified.
    pub fn unverified(&self) -> Self {
        Self { user_verified: false, ..self.clone() }
    }

    /// The same passkey reporting `counter` on every use.
    pub fn stuck_at(&self, counter: u32) -> Self {
        Self { counter, counting: false, ..self.clone() }
    }

    /// Its credential id, base64url.
    pub fn credential_id(&self) -> String {
        Base64UrlUnpadded::encode_string(&self.credential_id)
    }

    fn client_data(&self, kind: &str, options: &Value) -> String {
        json!({
            "type": kind,
            "challenge": options["challenge"],
            "origin": self.origin,
            "crossOrigin": false,
        })
        .to_string()
    }

    fn flags(&self) -> u8 {
        0x01 | if self.user_verified { 0x04 } else { 0 }
    }

    /// The answer to `navigator.credentials.create({ publicKey: options })`, as
    /// `credential.toJSON()` gives it.
    pub fn register(&mut self, options: &Value) -> Value {
        let client_data = self.client_data("webauthn.create", options);
        let mut auth_data = Sha256::digest(self.rp_id.as_bytes()).to_vec();
        auth_data.push(self.flags() | 0x40);
        auth_data.extend(self.counter.to_be_bytes());
        auth_data.extend([0u8; 16]);
        auth_data.extend(u16::try_from(self.credential_id.len()).unwrap().to_be_bytes());
        auth_data.extend(&self.credential_id);
        auth_data.extend(self.cose_key());
        let mut attestation = Vec::new();
        map(&mut attestation, 3);
        text(&mut attestation, "fmt");
        text(&mut attestation, "none");
        text(&mut attestation, "attStmt");
        map(&mut attestation, 0);
        text(&mut attestation, "authData");
        bytes(&mut attestation, &auth_data);
        json!({
            "id": self.credential_id(),
            "rawId": self.credential_id(),
            "type": "public-key",
            "authenticatorAttachment": "platform",
            "response": {
                "attestationObject": Base64UrlUnpadded::encode_string(&attestation),
                "clientDataJSON": Base64UrlUnpadded::encode_string(client_data.as_bytes()),
                "transports": ["internal"],
            },
            "clientExtensionResults": {},
        })
    }

    /// The answer to `navigator.credentials.get({ publicKey: options })`, as
    /// `credential.toJSON()` gives it.
    pub fn assert(&mut self, options: &Value) -> Value {
        if self.counting {
            self.counter += 1;
        }
        let client_data = self.client_data("webauthn.get", options);
        let mut auth_data = Sha256::digest(self.rp_id.as_bytes()).to_vec();
        auth_data.push(self.flags());
        auth_data.extend(self.counter.to_be_bytes());
        let mut signed = auth_data.clone();
        signed.extend(Sha256::digest(client_data.as_bytes()));
        let signature: Signature = self.key.sign(&signed);
        json!({
            "id": self.credential_id(),
            "rawId": self.credential_id(),
            "type": "public-key",
            "authenticatorAttachment": "platform",
            "response": {
                "authenticatorData": Base64UrlUnpadded::encode_string(&auth_data),
                "clientDataJSON": Base64UrlUnpadded::encode_string(client_data.as_bytes()),
                "signature": Base64UrlUnpadded::encode_string(&der(&signature.to_bytes())),
                "userHandle": null,
            },
            "clientExtensionResults": {},
        })
    }

    /// The credential's public key as a COSE key: EC2, ES256, P-256.
    fn cose_key(&self) -> Vec<u8> {
        let point = self.key.verifying_key().to_encoded_point(false);
        let mut out = Vec::new();
        map(&mut out, 5);
        int(&mut out, 1);
        int(&mut out, 2);
        int(&mut out, 3);
        int(&mut out, -7);
        int(&mut out, -1);
        int(&mut out, 1);
        int(&mut out, -2);
        bytes(&mut out, point.x().unwrap());
        int(&mut out, -3);
        bytes(&mut out, point.y().unwrap());
        out
    }
}

/// An ECDSA signature's `r || s` as the DER `SEQUENCE { INTEGER r, INTEGER s }` WebAuthn sends.
fn der(raw: &[u8]) -> Vec<u8> {
    let integer = |half: &[u8]| {
        let start = half.iter().position(|byte| *byte != 0).unwrap_or(half.len() - 1);
        let mut value = half[start..].to_vec();
        if value[0] & 0x80 != 0 {
            value.insert(0, 0);
        }
        let mut out = vec![0x02, u8::try_from(value.len()).unwrap()];
        out.extend(value);
        out
    };
    let mut body = integer(&raw[..32]);
    body.extend(integer(&raw[32..]));
    let mut out = vec![0x30, u8::try_from(body.len()).unwrap()];
    out.extend(body);
    out
}

fn head(out: &mut Vec<u8>, major: u8, len: usize) {
    match len {
        0..=23 => out.push(major << 5 | u8::try_from(len).unwrap()),
        24..=255 => {
            out.push(major << 5 | 24);
            out.push(u8::try_from(len).unwrap());
        }
        _ => {
            out.push(major << 5 | 25);
            out.extend(u16::try_from(len).unwrap().to_be_bytes());
        }
    }
}

fn int(out: &mut Vec<u8>, value: i64) {
    if value >= 0 {
        head(out, 0, usize::try_from(value).unwrap());
    } else {
        head(out, 1, usize::try_from(-1 - value).unwrap());
    }
}

fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    head(out, 2, value.len());
    out.extend(value);
}

fn text(out: &mut Vec<u8>, value: &str) {
    head(out, 3, value.len());
    out.extend(value.as_bytes());
}

fn map(out: &mut Vec<u8>, entries: usize) {
    head(out, 5, entries);
}
