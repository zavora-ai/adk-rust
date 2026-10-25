//! The passkey checks of "Confirm it's you" with webauthn-rs (spec 3.3 §8.2): passkeys with
//! user verification, attestation `none`, the home's origin and RP id, and a counter that goes
//! forward. webauthn-rs links the system's OpenSSL.

use std::fmt;
use std::sync::Arc;

use base64ct::{Base64UrlUnpadded, Encoding as _};
use serde_json::{Value, json};
use webauthn_rs::prelude::{
    Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
};

use super::error::MobileError;
use super::passkey::{
    CEREMONY_LIFETIME, Ceremony, PasskeyVerifier, RelyingParty, StoredPasskey, Verified,
};

/// The verifier for one relying party: the home's host, and its origin exactly.
pub(crate) struct WebauthnVerifier {
    webauthn: Webauthn,
}

impl fmt::Debug for WebauthnVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebauthnVerifier")
            .field("origins", &self.webauthn.get_allowed_origins())
            .finish_non_exhaustive()
    }
}

impl WebauthnVerifier {
    pub(crate) fn new(rp: &RelyingParty) -> Result<Self, MobileError> {
        let unusable = |reason: String| MobileError::PasskeyRefused {
            reason: format!("the home URL cannot hold passkeys: {reason}"),
        };
        let origin = Url::parse(&rp.origin).map_err(|error| unusable(error.to_string()))?;
        let webauthn = WebauthnBuilder::new(&rp.id, &origin)
            .map_err(|error| unusable(error.to_string()))?
            .rp_name("ADK-Rust")
            .timeout(CEREMONY_LIFETIME.to_std().unwrap_or_default())
            .build()
            .map_err(|error| unusable(error.to_string()))?;
        Ok(Self { webauthn })
    }
}

fn keys(passkeys: &[StoredPasskey]) -> Vec<Passkey> {
    passkeys
        .iter()
        .filter_map(|passkey| serde_json::from_value(passkey.record.clone()).ok())
        .collect()
}

fn public_key(options: &impl serde::Serialize) -> Result<Value, String> {
    let mut options = serde_json::to_value(options).map_err(|error| error.to_string())?;
    Ok(options["publicKey"].take())
}

impl PasskeyVerifier for WebauthnVerifier {
    fn start_registration(
        &self,
        user: [u8; 16],
        name: &str,
        exclude: &[StoredPasskey],
    ) -> Result<(Value, Ceremony), String> {
        let exclude: Vec<_> = keys(exclude).iter().map(|key| key.cred_id().clone()).collect();
        let (options, state) = self
            .webauthn
            .start_passkey_registration(
                Uuid::from_bytes(user),
                name,
                name,
                (!exclude.is_empty()).then_some(exclude),
            )
            .map_err(|error| error.to_string())?;
        let mut options = public_key(&options)?;
        // The phone's or the computer's own authenticator, never a key someone carries off.
        options["authenticatorSelection"]["authenticatorAttachment"] = json!("platform");
        Ok((options, Arc::new(state)))
    }

    fn finish_registration(
        &self,
        credential: &Value,
        ceremony: &Ceremony,
    ) -> Result<Verified, String> {
        let state = ceremony
            .downcast_ref::<PasskeyRegistration>()
            .ok_or("the ceremony is not a registration")?;
        let credential: RegisterPublicKeyCredential = serde_json::from_value(credential.clone())
            .map_err(|error| format!("the credential is not a registration response: {error}"))?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(&credential, state)
            .map_err(|error| error.to_string())?;
        Ok(Verified {
            credential_id: Base64UrlUnpadded::encode_string(passkey.cred_id().as_ref()),
            record: serde_json::to_value(&passkey).map_err(|error| error.to_string())?,
        })
    }

    fn start_authentication(
        &self,
        passkeys: &[StoredPasskey],
    ) -> Result<(Value, Ceremony), String> {
        let (options, state) = self
            .webauthn
            .start_passkey_authentication(&keys(passkeys))
            .map_err(|error| error.to_string())?;
        Ok((public_key(&options)?, Arc::new(state)))
    }

    fn finish_authentication(
        &self,
        credential: &Value,
        ceremony: &Ceremony,
        passkeys: &[StoredPasskey],
    ) -> Result<Verified, String> {
        let state = ceremony
            .downcast_ref::<PasskeyAuthentication>()
            .ok_or("the ceremony is not a confirmation")?;
        let credential: PublicKeyCredential = serde_json::from_value(credential.clone())
            .map_err(|error| format!("the credential is not an assertion: {error}"))?;
        let result = self
            .webauthn
            .finish_passkey_authentication(&credential, state)
            .map_err(|error| error.to_string())?;
        let id = Base64UrlUnpadded::encode_string(result.cred_id().as_ref());
        let mut key = keys(passkeys)
            .into_iter()
            .find(|key| Base64UrlUnpadded::encode_string(key.cred_id().as_ref()) == id)
            .ok_or("the passkey is not one set up here")?;
        key.update_credential(&result);
        Ok(Verified {
            credential_id: id,
            record: serde_json::to_value(&key).map_err(|error| error.to_string())?,
        })
    }
}
