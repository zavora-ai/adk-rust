//! The setup code (spec 3.5 §7.17, A-I20): a freshly started site cannot be claimed without
//! it. The code pairs the owner's device through the kernel's pairing; until then the site
//! serves a holding page and accepts no onboarding input.

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_kernel::pairing::{
    ClaimedDevice, DeviceKind, DeviceRecord, DeviceScope, IssueCode, IssuedCode, Pairing,
};
use tokio::sync::Mutex;

/// The approval group the owner's device answers decisions in.
const OWNER_GROUP: &str = "owner";

/// The setup-code gate of a fresh deployment (spec 3.5 §7.17, A-I20).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
/// use adk_website::onboarding::SetupGate;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let gate = SetupGate::new(Pairing::new(Arc::new(InMemoryDeviceStore::new())), "owner");
/// assert!(gate.admit(None).await.is_err(), "no input before pairing");
/// let issued = gate.issue().await.unwrap();
/// let claimed = gate.claim(&issued.code, "Amina's phone").await.unwrap();
/// assert_eq!(gate.admit(Some(&claimed.token)).await.unwrap().owner, "owner");
/// # }
/// ```
pub struct SetupGate {
    pairing: Pairing,
    owner: String,
    paired: Mutex<Option<String>>,
}

impl std::fmt::Debug for SetupGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupGate").field("owner", &self.owner).finish_non_exhaustive()
    }
}

fn unpaired(reason: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Unauthorized,
        "website.setup.unpaired",
        format!(
            "{reason}: this site accepts no onboarding input until its setup code pairs the \
             owner's device; the code is in the site's logs (spec 3.5 A-I20)"
        ),
    )
}

impl SetupGate {
    /// The gate over the kernel's `pairing`; the device it pairs acts as `owner`.
    pub fn new(pairing: Pairing, owner: impl Into<String>) -> Self {
        Self { pairing, owner: owner.into(), paired: Mutex::new(None) }
    }

    /// Issues the setup code and writes it to the logs, where the host shows it (spec 3.5
    /// §7.17): a pairing code for a phone that reads, decides and stops.
    ///
    /// # Errors
    ///
    /// Returns the pairing store's error.
    pub async fn issue(&self) -> Result<IssuedCode> {
        let issued = self
            .pairing
            .issue(
                IssueCode::new(self.owner.clone(), [OWNER_GROUP])
                    .with_scopes(DeviceScope::ALL)
                    .with_kind(DeviceKind::Phone)
                    .with_name("Owner's device"),
            )
            .await
            .map_err(|error| AdkError::config(error.to_string()))?;
        tracing::info!(
            setup.code = %issued.code,
            setup.expires_at = %issued.expires_at,
            "website setup code: enter it on the device that will own this site"
        );
        Ok(issued)
    }

    /// Claims the setup code from the owner's device; the device's token is returned once.
    ///
    /// # Errors
    ///
    /// `Unauthorized` for an unknown, used or expired code.
    pub async fn claim(&self, code: &str, device: &str) -> Result<ClaimedDevice> {
        let claimed = self
            .pairing
            .claim(code, Some(device))
            .await
            .map_err(|error| unpaired(&error.to_string()))?;
        *self.paired.lock().await = Some(claimed.device.id.clone());
        Ok(claimed)
    }

    /// Admits one piece of onboarding input from the device holding `token` (A-I20).
    ///
    /// # Errors
    ///
    /// `Unauthorized` without a token, or for a token the kernel's pairing does not
    /// authenticate.
    pub async fn admit(&self, token: Option<&str>) -> Result<DeviceRecord> {
        let Some(token) = token else {
            return Err(unpaired("no device token"));
        };
        self.pairing.authenticate(token).await.map_err(|error| unpaired(&error.to_string()))
    }

    /// Whether the owner's device has paired.
    pub async fn paired(&self) -> bool {
        self.paired.lock().await.is_some()
    }
}

/// The page a fresh site serves until onboarding launches it (spec 3.5 §7.17).
///
/// # Example
///
/// ```rust
/// assert!(adk_website::onboarding::holding_page("Juniper Bakery").contains("Juniper Bakery"));
/// ```
pub fn holding_page(site_name: &str) -> String {
    let name = site_name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" \
         content=\"width=device-width, initial-scale=1\">\n<title>{name}</title>\n</head>\n<body>\n\
         <main>\n<h1>{name}</h1>\n<p>This site is being set up. Please come back soon.</p>\n</main>\n\
         </body>\n</html>\n"
    )
}
