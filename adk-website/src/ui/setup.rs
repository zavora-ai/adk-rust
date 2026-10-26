//! Pairing the owner's device with the setup code (spec 3.5 §7.17, A-I20).
//!
//! A freshly started site cannot be claimed without its setup code, which the host prints to
//! its logs. The code pairs one device through the kernel's pairing; from then on onboarding
//! input is accepted only from that device's token. Until then the site serves its holding
//! page and every onboarding input is refused.

use adk_core::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{WebsiteDesk, forbidden, invalid};

/// What the setup screen shows (spec 3.5 §7.17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetupView {
    /// The site's name.
    pub site_name: String,
    /// Whether this site asks for a setup code at all.
    pub needed: bool,
    /// Whether the owner's device has paired.
    pub paired: bool,
    /// Where the code is.
    pub where_code: String,
    /// What visitors see until the site is launched.
    pub holding: String,
}

/// The device the setup code paired; its token is shown once (spec 3.5 A-I20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaimedView {
    /// The device's token: the page keeps it and sends it with every request.
    pub token: String,
    /// The device's name.
    pub device: String,
    /// When the pairing ends.
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// `POST …/setup/claim` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRequest {
    /// The setup code from the host's logs.
    pub code: String,
    /// A name for this device, such as "Amina's laptop".
    #[serde(default)]
    pub device: Option<String>,
}

impl WebsiteDesk {
    /// The setup screen (spec 3.5 §7.17).
    pub async fn setup_view(&self) -> SetupView {
        let paired = match &self.inner.setup {
            Some(gate) => gate.paired().await,
            None => true,
        };
        SetupView {
            site_name: self.inner.name.clone(),
            needed: self.inner.setup.is_some(),
            paired,
            where_code: "The setup code is in the site's logs: in the terminal where you started \
                         it, or in your host's log viewer."
                .into(),
            holding: "This site is being set up. Please come back soon.".into(),
        }
    }

    /// Claims the setup code from this device (spec 3.5 §7.17, A-I20): one code pairs one
    /// device, once.
    ///
    /// # Errors
    ///
    /// `Unauthorized` for an unknown, used or expired code; `Forbidden` when the site needs no
    /// setup code or a device has paired already.
    pub async fn claim(&self, code: &str, device: Option<&str>) -> Result<ClaimedView> {
        let gate = self.inner.setup.as_ref().ok_or_else(|| {
            forbidden("website.setup.not_needed", "this site was set up without a setup code")
        })?;
        let code = code.trim();
        if code.is_empty() {
            return Err(invalid("website.setup.code_missing", "enter the setup code"));
        }
        let mut paired = self.inner.paired.lock().await;
        if paired.is_some() || gate.paired().await {
            return Err(forbidden(
                "website.setup.already_paired",
                "a device has already claimed this site; ask its owner to add you",
            ));
        }
        let name =
            device.map(str::trim).filter(|name| !name.is_empty()).unwrap_or("Owner's device");
        let claimed = gate.claim(code, name).await?;
        *paired = Some(claimed.device.id.clone());
        drop(paired);
        // The paired device is the only one onboarding admits, after a restart too (A-I20).
        self.persist().await;
        tracing::info!(website.site = %self.inner.site.id, device = %claimed.device.id, "website.setup.paired");
        Ok(ClaimedView {
            token: claimed.token,
            device: claimed.device.name.clone(),
            expires_at: claimed.device.expires_at,
        })
    }

    /// Admits one piece of onboarding input carrying `token` (spec 3.5 A-I20): only the
    /// device the setup code paired gets through. Returns the principal it acts as, the
    /// device's owner. A desk built without a setup gate admits the authenticated `person`.
    ///
    /// # Errors
    ///
    /// `Unauthorized` before the code is claimed, without a token, or for another device's.
    pub async fn admit(&self, person: &str, token: Option<&str>) -> Result<String> {
        let Some(gate) = &self.inner.setup else { return Ok(person.to_string()) };
        let record = gate.admit(token).await?;
        let paired = self.inner.paired.lock().await.clone();
        if paired.as_deref() != Some(record.id.as_str()) {
            return Err(adk_core::AdkError::new(
                adk_core::ErrorComponent::Server,
                adk_core::ErrorCategory::Unauthorized,
                "website.setup.unpaired",
                "this device is not the one the setup code paired; onboarding input comes only \
                 from the owner's paired device (spec 3.5 A-I20)",
            ));
        }
        Ok(record.owner)
    }
}
