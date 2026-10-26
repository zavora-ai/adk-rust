//! "Confirm it's you and approve" (spec 3.3 §7.2, §8.2): a passkey set up on the phone, or on
//! the computer for the web home, and the confirmation that binds one answer to one decision.
//!
//! The server binds every confirmation to the holder, the decision, the version seen, the
//! answer and the edited arguments; the assertion only proves the person is there. Presence
//! never comes from request metadata here: only a verified assertion by one of the holder's own
//! passkeys, for the bound answer, sets it.
//!
//! The WebAuthn checks are behind [`PasskeyVerifier`]; [`super::webauthn::WebauthnVerifier`]
//! implements them with webauthn-rs.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use base64ct::{Base64UrlUnpadded, Encoding as _};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::MobileError;
use super::store::{Attachment, Attachments, DEVICE_PASSKEY_KEY, Holder, PASSKEY};
use crate::error::SurfaceError;
use crate::rules::Answer;
use crate::web::phone::{ConfirmOptions, ConfirmOptionsRequest, ConfirmRequest, PasskeyOptions};

/// How long after pairing a phone may set up its passkey.
pub(crate) const REGISTRATION_WINDOW: Duration = Duration::minutes(10);
/// How long a registration or a confirmation may take.
pub(crate) const CEREMONY_LIFETIME: Duration = Duration::minutes(2);
/// The most ceremonies kept waiting at once.
const CEREMONIES_KEPT: usize = 1024;

/// The passkeys' relying party: the home's host and its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelyingParty {
    pub(crate) id: String,
    pub(crate) origin: String,
}

/// A verifier's state for one ceremony, kept on the server between its two requests.
pub(crate) type Ceremony = Arc<dyn Any + Send + Sync>;

/// A passkey as the store keeps it: its credential id and the verifier's own record of it
/// (its public key and signature counter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredPasskey {
    /// The credential id, base64url.
    pub(crate) credential_id: String,
    /// The verifier's record.
    pub(crate) record: Value,
    /// When it was set up.
    pub(crate) created_at: DateTime<Utc>,
}

/// A passkey a registration created, or one an assertion updated.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Verified {
    /// The credential id, base64url.
    pub(crate) credential_id: String,
    /// The verifier's record, to keep.
    pub(crate) record: Value,
}

/// The WebAuthn checks (spec 3.3 §8.2), for one relying party.
///
/// A registration is a platform passkey with user verification and attestation `none`; an
/// assertion is by one of the given passkeys, with user verification, for the ceremony's
/// challenge, origin and RP id, and a signature counter that goes forward when it counts.
pub(crate) trait PasskeyVerifier: Send + Sync + fmt::Debug {
    /// The creation options (`publicKey` of `navigator.credentials.create`) and the state that
    /// checks the answer. `user` identifies the holder; `exclude` are its passkeys.
    fn start_registration(
        &self,
        user: [u8; 16],
        name: &str,
        exclude: &[StoredPasskey],
    ) -> Result<(Value, Ceremony), String>;

    /// Checks a registration response against its ceremony.
    fn finish_registration(
        &self,
        credential: &Value,
        ceremony: &Ceremony,
    ) -> Result<Verified, String>;

    /// The request options (`publicKey` of `navigator.credentials.get`) for `passkeys` and the
    /// state that checks the answer.
    fn start_authentication(&self, passkeys: &[StoredPasskey])
    -> Result<(Value, Ceremony), String>;

    /// Checks an assertion against its ceremony, by one of `passkeys`; that passkey, updated.
    fn finish_authentication(
        &self,
        credential: &Value,
        ceremony: &Ceremony,
        passkeys: &[StoredPasskey],
    ) -> Result<Verified, String>;
}

/// The decision id a confirmation of an agent change is bound under: the change itself is the
/// binding's edited arguments, so the passkey confirms exactly that command (spec 3.3 §7.2).
/// No decision has it, so the decision routes never accept such a binding.
pub(crate) const AGENT_CHANGE: &str = "agent-change";

/// What a confirmation is bound to.
#[derive(Clone)]
pub(crate) struct Binding {
    pub(crate) holder: Holder,
    pub(crate) decision_id: String,
    pub(crate) version: u64,
    pub(crate) answer: Answer,
    pub(crate) edited_args: Option<Value>,
    pub(crate) expanded: bool,
    ceremony: Ceremony,
    expires_at: DateTime<Utc>,
}

impl fmt::Debug for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Binding")
            .field("holder", &self.holder)
            .field("decision_id", &self.decision_id)
            .field("version", &self.version)
            .field("answer", &self.answer)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct Registration {
    holder: Holder,
    ceremony: Ceremony,
    expires_at: DateTime<Utc>,
}

/// The passkeys and the ceremonies in flight. Ceremonies live in memory: the two requests of
/// one confirmation reach the same server (sticky routing behind a load balancer).
#[derive(Clone)]
pub(crate) struct Passkeys {
    verifier: Option<Arc<dyn PasskeyVerifier>>,
    store: Attachments,
    confirms: Arc<Mutex<HashMap<String, Binding>>>,
    registrations: Arc<Mutex<HashMap<String, Registration>>>,
}

impl fmt::Debug for Passkeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Passkeys").field("verifier", &self.verifier).finish_non_exhaustive()
    }
}

fn ceremony_id() -> Result<String, MobileError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| MobileError::PasskeyRefused {
        reason: format!("the operating system's random source failed: {error}"),
    })?;
    Ok(Base64UrlUnpadded::encode_string(&bytes))
}

/// The digest an edit's arguments are compared by.
fn args_digest(args: Option<&Value>) -> Option<Vec<u8>> {
    use sha2::{Digest as _, Sha256};
    args.map(|args| Sha256::digest(serde_json::to_vec(args).unwrap_or_default()).to_vec())
}

/// A stable 16-byte WebAuthn user handle for `holder`, which names no person.
fn user_handle(holder: &Holder) -> [u8; 16] {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(holder.key().as_bytes());
    let mut handle = [0u8; 16];
    handle.copy_from_slice(&digest[..16]);
    handle
}

fn refused(reason: impl Into<String>) -> MobileError {
    MobileError::PasskeyRefused { reason: reason.into() }
}

impl Passkeys {
    pub(crate) fn new(verifier: Option<Arc<dyn PasskeyVerifier>>, store: Attachments) -> Self {
        Self { verifier, store, confirms: Arc::default(), registrations: Arc::default() }
    }

    fn verifier(&self) -> Result<&Arc<dyn PasskeyVerifier>, MobileError> {
        self.verifier.as_ref().ok_or(MobileError::PasskeyUnavailable)
    }

    /// `holder`'s passkeys, by their attachment key.
    async fn rows(&self, holder: &Holder) -> Result<Vec<(String, StoredPasskey)>, MobileError> {
        Ok(self
            .store
            .list(holder, PASSKEY)
            .await?
            .into_iter()
            .filter_map(|row| Some((row.key, serde_json::from_value(row.record).ok()?)))
            .collect())
    }

    /// `holder`'s passkeys.
    pub(crate) async fn of(&self, holder: &Holder) -> Result<Vec<StoredPasskey>, MobileError> {
        Ok(self.rows(holder).await?.into_iter().map(|(_, passkey)| passkey).collect())
    }

    /// Starts setting up a passkey for `holder` (`navigator.credentials.create`).
    pub(crate) async fn registration_options(
        &self,
        holder: Holder,
        name: &str,
        now: DateTime<Utc>,
    ) -> Result<PasskeyOptions, MobileError> {
        let verifier = self.verifier()?;
        let existing = self.of(&holder).await?;
        let (public_key, ceremony) =
            verifier.start_registration(user_handle(&holder), name, &existing).map_err(refused)?;
        let registration_id = ceremony_id()?;
        let mut registrations = self.registrations.lock().unwrap_or_else(|e| e.into_inner());
        registrations.retain(|_, waiting| waiting.expires_at > now);
        if registrations.len() >= CEREMONIES_KEPT {
            return Err(MobileError::Challenge);
        }
        registrations.insert(
            registration_id.clone(),
            Registration { holder, ceremony, expires_at: now + CEREMONY_LIFETIME },
        );
        Ok(PasskeyOptions { registration_id, public_key })
    }

    /// Finishes setting up a passkey for `holder`: one per device, one per credential for a
    /// person.
    pub(crate) async fn register(
        &self,
        holder: &Holder,
        registration_id: &str,
        credential: &Value,
        now: DateTime<Utc>,
    ) -> Result<StoredPasskey, MobileError> {
        let verifier = self.verifier()?;
        let waiting = self
            .registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(registration_id)
            .filter(|waiting| &waiting.holder == holder && waiting.expires_at > now)
            .ok_or(MobileError::Challenge)?;
        let created =
            verifier.finish_registration(credential, &waiting.ceremony).map_err(refused)?;
        let passkey = StoredPasskey {
            credential_id: created.credential_id,
            record: created.record,
            created_at: now,
        };
        // One passkey per device; a person may set one up on each computer.
        let key = match holder {
            Holder::Device(_) => DEVICE_PASSKEY_KEY.to_string(),
            _ => passkey.credential_id.clone(),
        };
        let record = serde_json::to_value(&passkey).unwrap_or(Value::Null);
        let row = Attachment::new(holder.clone(), PASSKEY, key, record, now);
        if !self.store.put_if_absent(row).await? {
            return Err(MobileError::PasskeyExists);
        }
        Ok(passkey)
    }

    /// Removes a person's computer passkey; whether it was there.
    pub(crate) async fn remove(
        &self,
        holder: &Holder,
        credential_id: &str,
    ) -> Result<bool, MobileError> {
        Ok(self.store.remove(holder, PASSKEY, credential_id).await?)
    }

    /// Starts confirming `request.answer` to decision `decision_id` as `holder`
    /// (`navigator.credentials.get`), bound to all of it.
    pub(crate) async fn confirm_options(
        &self,
        holder: Holder,
        decision_id: &str,
        request: ConfirmOptionsRequest,
        now: DateTime<Utc>,
    ) -> Result<ConfirmOptions, MobileError> {
        let verifier = self.verifier()?;
        if !matches!(request.answer, Answer::AllowOnce | Answer::Edit) {
            return Err(SurfaceError::Invalid(
                "only allow once and edit are confirmed; deny needs no confirmation".into(),
            )
            .into());
        }
        if request.answer == Answer::Edit && request.edited_args.is_none() {
            return Err(SurfaceError::Invalid("an edit needs the changed arguments".into()).into());
        }
        let passkeys = self.of(&holder).await?;
        if passkeys.is_empty() {
            let hint = match holder {
                Holder::Device(_) => {
                    "this phone has no passkey yet: approve it in the home on your computer"
                }
                _ => {
                    "this computer has no passkey yet: choose \"Use your face or fingerprint on \
                     this computer\" under where you can say yes from, in Memory and rules"
                }
            };
            return Err(MobileError::NoPasskey { hint });
        }
        let (public_key, ceremony) = verifier.start_authentication(&passkeys).map_err(refused)?;
        let challenge_id = ceremony_id()?;
        let binding = Binding {
            holder,
            decision_id: decision_id.to_string(),
            version: request.version,
            answer: request.answer,
            edited_args: request.edited_args,
            expanded: request.expanded,
            ceremony,
            expires_at: now + CEREMONY_LIFETIME,
        };
        let mut confirms = self.confirms.lock().unwrap_or_else(|e| e.into_inner());
        confirms.retain(|_, waiting| waiting.expires_at > now);
        if confirms.len() >= CEREMONIES_KEPT {
            return Err(MobileError::Challenge);
        }
        confirms.insert(challenge_id.clone(), binding);
        Ok(ConfirmOptions { challenge_id, public_key })
    }

    /// Checks a confirmation: its binding against `holder`, the path's decision and what the
    /// page sent again, then the assertion by one of `holder`'s passkeys, whose updated record
    /// is kept. The binding is used up whatever the outcome. Only then may the caller decide
    /// with presence.
    pub(crate) async fn confirm(
        &self,
        holder: &Holder,
        decision_id: &str,
        request: &ConfirmRequest,
        now: DateTime<Utc>,
    ) -> Result<Binding, MobileError> {
        let verifier = self.verifier()?;
        let binding = self
            .confirms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request.challenge_id)
            .ok_or(MobileError::Challenge)?;
        let sent_again_differs = request.answer.is_some_and(|answer| answer != binding.answer)
            || (request.edited_args.is_some()
                && args_digest(request.edited_args.as_ref())
                    != args_digest(binding.edited_args.as_ref()));
        if &binding.holder != holder
            || binding.decision_id != decision_id
            || binding.expires_at <= now
            || sent_again_differs
        {
            return Err(MobileError::Challenge);
        }
        let rows = self.rows(holder).await?;
        let passkeys: Vec<StoredPasskey> =
            rows.iter().map(|(_, passkey)| passkey.clone()).collect();
        let verified = verifier
            .finish_authentication(&request.credential, &binding.ceremony, &passkeys)
            .map_err(refused)?;
        if let Some((key, passkey)) =
            rows.into_iter().find(|(_, passkey)| passkey.credential_id == verified.credential_id)
        {
            let updated = StoredPasskey { record: verified.record, ..passkey };
            let record = serde_json::to_value(&updated).unwrap_or(Value::Null);
            self.store.put(Attachment::new(holder.clone(), PASSKEY, key, record, now)).await?;
        }
        Ok(binding)
    }
}
