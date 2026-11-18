//! Approval seals (spec 3.3 §7.4 rule 5): when a person approves a call they asked for with a
//! drawn form's Send, the home seals what they approved — the tool, a hash of the arguments, a
//! hash of what the card showed, the drawing's digest and version, its surface, the person and
//! when it expires — with an HMAC under a key only the home holds. Right before the call runs,
//! the run checks it ([`adk_core::RequestedCallSeal`]): a call whose tool or arguments changed
//! since, or whose seal was forged, spent or expired, is refused. Each seal runs one call.
//!
//! A seal is tamper evidence and a record: the approval itself already binds the call to its
//! arguments, and the seal binds it to the drawing the person saw, in the audit trail.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{RequestedCallSeal, SealedCall};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// How long a seal stays good after the person decides.
pub const SEAL_LIFETIME: chrono::Duration = chrono::Duration::hours(24);

/// What a seal binds: the call as approved and the drawing it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealInput {
    /// The requested call's function-call id.
    pub function_call_id: String,
    /// The tool.
    pub tool_name: String,
    /// The arguments as approved, after any edit.
    pub args: Value,
    /// What the card showed the person: the tool and the arguments as the home renders them.
    pub card_text: String,
    /// The digest of the drawing the person pressed Send on.
    pub drawing_digest: Option<String>,
    /// The surface and its version.
    pub surface_id: String,
    /// The version of the surface.
    pub version: u64,
    /// Who approved.
    pub approver: String,
}

#[derive(Debug, Clone)]
struct Seal {
    input: SealInput,
    card_hash: String,
    expires_at: DateTime<Utc>,
    mac: Vec<u8>,
}

/// The home's approval seals: made when a person approves a form's call, checked and spent
/// when the call runs. Give the same `ApprovalSeals` to the surface engine
/// (`SurfaceEngineBuilder::approval_seals`) and to the harness that runs the drawing agent
/// (`HarnessAgentBuilder::requested_call_seal`).
///
/// # Example
///
/// ```rust
/// use adk_core::{RequestedCallSeal, SealedCall};
/// use adk_surface::genui::{ApprovalSeals, SealInput};
/// use serde_json::json;
///
/// let seals = ApprovalSeals::new();
/// let input = SealInput {
///     function_call_id: "adk-call-requested-1".into(),
///     tool_name: "send_invoice".into(),
///     args: json!({ "to": "ana@example.com" }),
///     card_text: "send_invoice {\"to\":\"ana@example.com\"}".into(),
///     drawing_digest: Some("sha256:ab".into()),
///     surface_id: "invoice-10".into(),
///     version: 1,
///     approver: "ana".into(),
/// };
/// seals.seal(input);
/// let approved = json!({ "to": "ana@example.com" });
/// let changed = json!({ "to": "mallory@example.com" });
/// let call = |args| SealedCall {
///     function_call_id: "adk-call-requested-1",
///     tool_name: "send_invoice",
///     args,
/// };
/// assert!(seals.verify(&call(&changed)).is_err());
/// // A refused check spends the seal too: the person decides again.
/// assert!(!seals.is_sealed("adk-call-requested-1"));
/// ```
///
/// A call that runs as approved passes once:
///
/// ```rust
/// # use adk_core::{RequestedCallSeal, SealedCall};
/// # use adk_surface::genui::{ApprovalSeals, SealInput};
/// # use serde_json::json;
/// let seals = ApprovalSeals::new();
/// let args = json!({ "to": "ana@example.com" });
/// seals.seal(SealInput {
///     function_call_id: "c1".into(),
///     tool_name: "send_invoice".into(),
///     args: args.clone(),
///     card_text: String::new(),
///     drawing_digest: None,
///     surface_id: "invoice-10".into(),
///     version: 1,
///     approver: "ana".into(),
/// });
/// let call = SealedCall { function_call_id: "c1", tool_name: "send_invoice", args: &args };
/// assert!(seals.verify(&call).is_ok());
/// assert!(seals.clone().strict().verify(&call).is_err(), "spent");
/// ```
#[derive(Clone)]
pub struct ApprovalSeals {
    inner: Arc<Inner>,
}

struct Inner {
    key: [u8; 32],
    seals: Mutex<HashMap<String, Seal>>,
    strict: bool,
}

impl std::fmt::Debug for ApprovalSeals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalSeals")
            .field("key", &"<redacted>")
            .field("strict", &self.inner.strict)
            .finish_non_exhaustive()
    }
}

impl Default for ApprovalSeals {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalSeals {
    /// Seals under a fresh random key held in memory: a seal outlives no restart, and a call
    /// without a seal runs (a decision made elsewhere — the terminal over the control plane,
    /// say — makes none).
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self::with_key(key)
    }

    /// Seals under `key`, which the home keeps secret.
    pub fn with_key(key: [u8; 32]) -> Self {
        Self { inner: Arc::new(Inner { key, seals: Mutex::new(HashMap::new()), strict: false }) }
    }

    /// The same seals, refusing a call that has none.
    pub fn strict_from(seals: &ApprovalSeals) -> Self {
        let seals_now =
            seals.inner.seals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        Self {
            inner: Arc::new(Inner {
                key: seals.inner.key,
                seals: Mutex::new(seals_now),
                strict: true,
            }),
        }
    }

    /// Refuses a requested call that has no seal: for a home where every decision is made
    /// through this engine.
    #[must_use]
    pub fn strict(self) -> Self {
        Self::strict_from(&self)
    }

    fn hash(value: &[u8]) -> String {
        let digest = Sha256::digest(value);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn canonical(args: &Value) -> String {
        adk_core::tool_call_fingerprint("", args)
    }

    fn mac(
        &self,
        input: &SealInput,
        args_hash: &str,
        card_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> HmacSha256 {
        // A key of any length is accepted by HMAC.
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.inner.key)
            .unwrap_or_else(|_| unreachable!("HMAC takes a key of any length"));
        for part in [
            "adk.surface.approval-seal.v1",
            &input.function_call_id,
            &input.tool_name,
            args_hash,
            card_hash,
            input.drawing_digest.as_deref().unwrap_or(""),
            &input.surface_id,
            &input.version.to_string(),
            &input.approver,
            &expires_at.timestamp().to_string(),
        ] {
            mac.update(&(part.len() as u64).to_be_bytes());
            mac.update(part.as_bytes());
        }
        mac
    }

    /// Seals `input`, replacing any seal of the same call, and returns the seal in hex for the
    /// audit trail.
    pub fn seal(&self, input: SealInput) -> String {
        let args_hash = Self::hash(Self::canonical(&input.args).as_bytes());
        let card_hash = Self::hash(input.card_text.as_bytes());
        let expires_at = Utc::now() + SEAL_LIFETIME;
        let mac =
            self.mac(&input, &args_hash, &card_hash, expires_at).finalize().into_bytes().to_vec();
        let hex: String = mac.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut seals = self.inner.seals.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        seals.retain(|_, seal| seal.expires_at > Utc::now());
        seals.insert(input.function_call_id.clone(), Seal { input, card_hash, expires_at, mac });
        hex
    }

    /// Whether a seal waits for the call `function_call_id`.
    pub fn is_sealed(&self, function_call_id: &str) -> bool {
        self.inner
            .seals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(function_call_id)
    }
}

impl RequestedCallSeal for ApprovalSeals {
    fn verify(&self, call: &SealedCall<'_>) -> Result<(), String> {
        let seal = {
            let mut seals =
                self.inner.seals.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            seals.remove(call.function_call_id)
        };
        let Some(seal) = seal else {
            return if self.inner.strict {
                Err("the person's approval carries no seal".to_string())
            } else {
                Ok(())
            };
        };
        if seal.expires_at <= Utc::now() {
            return Err("the person's approval expired before it ran".to_string());
        }
        if seal.input.tool_name != call.tool_name {
            return Err("it is not the tool the person approved".to_string());
        }
        // The seal recomputed over the arguments the call would run with, compared in
        // constant time with the one made when the person decided.
        let args_hash = Self::hash(Self::canonical(call.args).as_bytes());
        self.mac(&seal.input, &args_hash, &seal.card_hash, seal.expires_at)
            .verify_slice(&seal.mac)
            .map_err(|_| "its arguments changed after the person approved it".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input() -> SealInput {
        SealInput {
            function_call_id: "call-1".into(),
            tool_name: "send_invoice".into(),
            args: json!({ "to": "ana@example.com", "amount": 4200 }),
            card_text: "send_invoice".into(),
            drawing_digest: Some("sha256:1".into()),
            surface_id: "invoice-10".into(),
            version: 2,
            approver: "ana".into(),
        }
    }

    fn call<'a>(args: &'a Value, tool: &'a str) -> SealedCall<'a> {
        SealedCall { function_call_id: "call-1", tool_name: tool, args }
    }

    #[test]
    fn a_sealed_call_runs_once_as_approved_and_never_changed() {
        let seals = ApprovalSeals::new();
        let hex = seals.seal(input());
        assert_eq!(hex.len(), 64);
        assert!(seals.is_sealed("call-1"));
        // Key order does not change the arguments.
        let same = json!({ "amount": 4200, "to": "ana@example.com" });
        assert!(seals.verify(&call(&same, "send_invoice")).is_ok());
        assert!(!seals.is_sealed("call-1"), "spent");

        seals.seal(input());
        let more = json!({ "to": "ana@example.com", "amount": 42000 });
        assert!(seals.verify(&call(&more, "send_invoice")).unwrap_err().contains("changed"));
        seals.seal(input());
        assert!(seals.verify(&call(&same, "wire_money")).unwrap_err().contains("not the tool"));
    }

    #[test]
    fn a_call_without_a_seal_runs_unless_seals_are_strict() {
        let args = json!({});
        assert!(ApprovalSeals::new().verify(&call(&args, "send_invoice")).is_ok());
        assert!(ApprovalSeals::new().strict().verify(&call(&args, "send_invoice")).is_err());
    }

    #[test]
    fn a_seal_made_under_another_key_does_not_pass() {
        let ours = ApprovalSeals::with_key([1; 32]);
        ours.seal(input());
        // Forge: move the seal under another key's store.
        let theirs = ApprovalSeals::with_key([2; 32]);
        {
            let taken = ours.inner.seals.lock().unwrap().remove("call-1").unwrap();
            theirs.inner.seals.lock().unwrap().insert("call-1".into(), taken);
        }
        let args = input().args;
        assert!(theirs.verify(&call(&args, "send_invoice")).is_err());
    }
}
