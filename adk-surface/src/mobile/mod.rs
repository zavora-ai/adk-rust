//! The phone (spec 3.3 §5.4, §8.2; feature `mobile`): the web home installed on a phone,
//! woken by push from the person's own kernel, approving high-risk calls after "Confirm it's
//! you".
//!
//! [`router`] serves the phone's routes under `/surface`, relative to the `/api` prefix, beside
//! [`web::router`](crate::web::router) and [`web::devices_router`](crate::web::devices_router)
//! over the same engine. Their wire types are in [`web`](crate::web), so the API document is
//! the same in every `web` build.
//!
//! | Route (under `/api`) | Who | Answer |
//! |---|---|---|
//! | `GET /surface/phone` | A phone, the web home | [`PhoneInfo`](crate::web::PhoneInfo) |
//! | `PUT`, `DELETE /surface/phone/push` | A phone | Subscribes to wake-ups, or stops them |
//! | `POST /surface/phone/push/received` | A phone | Acknowledges a wake-up |
//! | `GET /surface/phone/push/deliveries` | The operator | [`DeliveriesResponse`](crate::web::DeliveriesResponse) |
//! | `POST /surface/phone/passkey/options`, `POST /surface/phone/passkey` | A phone within 10 minutes of pairing, the web home | Sets up the passkey |
//! | `DELETE /surface/phone/passkey/{id}` | The web home | Removes a computer passkey |
//! | `POST /surface/decisions/{id}/confirm/options` | A phone, the web home | [`ConfirmOptions`](crate::web::ConfirmOptions), bound to one answer |
//! | `POST /surface/decisions/{id}/confirm` | A phone, the web home | The answer, decided with presence |
//! | `POST /surface/phone/qr` | The operator | The pairing link's QR code, as SVG |
//! | `GET /surface/phone/devices` | The operator | [`PhoneDevicesResponse`](crate::web::PhoneDevicesResponse) |
//!
//! A paired device whose kind is not `phone` gets `403 surface.not_here` on every one of them.
//! Without a home URL they answer `400 surface.phone.needs_home_url`.
//!
//! **Wake-ups carry nothing** (§11 S-I5): every push body is `{"v":1,"id":"…"}`, and the
//! service worker shows the fixed words "A decision is waiting" / "Open the app to see it".
//! A wake-up waits through quiet time; when quiet time ends each phone whose person has a
//! waiting decision gets one. [`PushDoorbell`] hears about each decision; [`Phone::start_push`]
//! sends.

mod doorbell;
mod error;
mod passkey;
mod push;
mod qr;
mod quiet;
mod routes;
mod store;
#[cfg(test)]
mod tests;
mod vapid;
mod webauthn;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditTrail;
use adk_core::{ApprovalService, Clock, SystemClock};
use adk_kernel::QuietHours;
use adk_kernel::pairing::Pairing;
use axum::http::Uri;
use tokio::task::JoinHandle;

pub use doorbell::PushDoorbell;
pub use error::MobileError;
pub use push::PushAllowlist;
pub use routes::router;
pub use vapid::PushKey;

use crate::engine::SurfaceEngine;
use passkey::{PasskeyVerifier, Passkeys, RelyingParty};
use push::{Deliveries, PushSender};
use quiet::{KernelQuiet, QuietTime, Windows};
use store::Attachments;
use vapid::Vapid;
use webauthn::WebauthnVerifier;

/// The audit action of a wake-up held, sent or dropped (spec 3.3 §11).
pub const SURFACE_AUDIT_NOTIFY: &str = "surface.notify";
/// The audit action of a phone's push subscription, made or removed.
pub const SURFACE_AUDIT_PHONE_PUSH: &str = "surface.phone.push";
/// The audit action of a passkey set up or removed.
pub const SURFACE_AUDIT_PHONE_PASSKEY: &str = "surface.phone.passkey";

/// How often the push task looks whether quiet time has ended.
const QUIET_POLL: Duration = Duration::from_secs(60);
/// The first wait before a failed push is sent again.
const PUSH_BACKOFF: Duration = Duration::from_secs(2);

/// The home's public URL, and the passkeys' relying party it gives.
#[derive(Debug, Clone)]
pub(crate) struct Home {
    pub(crate) url: String,
    pub(crate) rp: RelyingParty,
}

impl Home {
    /// `url` must be https, or http on `localhost` for tests: WebAuthn needs a secure context
    /// and refuses IP addresses.
    fn parse(url: &str) -> Result<Self, MobileError> {
        let url = url.trim_end_matches('/').to_string();
        let invalid = |reason: &str| {
            MobileError::Surface(crate::error::SurfaceError::Invalid(format!(
                "the home URL {url} {reason}: give the https URL people open the home at"
            )))
        };
        let uri: Uri = url.parse().map_err(|_| invalid("is not a URL"))?;
        let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
            return Err(invalid("is not an absolute URL"));
        };
        let host = authority.host().to_ascii_lowercase();
        if host.starts_with('[') || host.parse::<std::net::IpAddr>().is_ok() {
            return Err(invalid("is an IP address, which passkeys refuse"));
        }
        if scheme != "https" && !(scheme == "http" && host == "localhost") {
            return Err(invalid("is not https"));
        }
        let origin = format!("{scheme}://{}", authority.as_str().to_ascii_lowercase());
        Ok(Self { url, rp: RelyingParty { id: host, origin } })
    }
}

/// The phone's parts (spec 3.3 §5.4): build it with [`Phone::builder`], serve it with
/// [`router`], and start its wake-ups with [`Phone::start_push`].
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{InMemoryApprovalStore, StoreApprovalService};
/// use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
/// use adk_session::InMemorySessionService;
/// use adk_surface::mobile::{Phone, PushDoorbell, PushKey};
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let doorbell = Arc::new(PushDoorbell::new());
/// let approvals = Arc::new(
///     StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_notifier(doorbell.clone()),
/// );
/// let source = LocalSource::builder("adk-rust")
///     .sessions(Arc::new(InMemorySessionService::new()))
///     .approvals(approvals.clone())
///     .build()?;
/// let engine = SurfaceEngine::builder(Arc::new(source)).stops_need("operator").build();
/// let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
/// let phone = Phone::builder(engine, pairing)
///     .home_url("https://home.example.ts.net")
///     .push_key(PushKey::generate()?)
///     .doorbell(doorbell)
///     .approvals(approvals)
///     .build()?;
/// let routes = adk_surface::mobile::router(phone.clone());
/// # let _ = routes;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Phone {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Phone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Phone")
            .field("home", &self.inner.home.as_ref().map(|home| &home.url))
            .finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub(crate) engine: SurfaceEngine,
    pub(crate) pairing: Pairing,
    pub(crate) home: Option<Home>,
    pub(crate) store: Attachments,
    pub(crate) sender: PushSender,
    pub(crate) quiet: Arc<dyn QuietTime>,
    pub(crate) approvals: Option<Arc<dyn ApprovalService>>,
    pub(crate) audit: Option<AuditTrail>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) deliveries: Arc<Deliveries>,
    pub(crate) passkeys: Passkeys,
    pub(crate) doorbell: Arc<PushDoorbell>,
    pub(crate) operator: String,
    pub(crate) poll: Duration,
}

impl Phone {
    /// A builder over `engine` and the kernel's `pairing`.
    pub fn builder(engine: SurfaceEngine, pairing: Pairing) -> PhoneBuilder {
        PhoneBuilder {
            engine,
            pairing,
            home_url: None,
            push_key: None,
            contact: None,
            allowlist: PushAllowlist::default(),
            quiet: Vec::new(),
            approvals: None,
            audit: None,
            clock: Arc::new(SystemClock),
            doorbell: None,
            operator: "operator".to_string(),
            poll: QUIET_POLL,
            backoff: PUSH_BACKOFF,
            kernel: None,
        }
    }

    /// The doorbell to add to the approval service.
    pub fn doorbell(&self) -> Arc<PushDoorbell> {
        Arc::clone(&self.inner.doorbell)
    }

    /// The home's public URL, when it has one.
    pub fn home_url(&self) -> Option<&str> {
        self.inner.home.as_ref().map(|home| home.url.as_str())
    }

    /// Starts sending wake-ups (spec 3.3 §5.4): one for each decision the doorbell hears about,
    /// held through quiet time; one per phone at the start and when quiet time ends. The task
    /// stops when it is aborted or the doorbell is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`MobileError::NeedsHomeUrl`] without a home URL, and an invalid-input error
    /// without an approval service to read waiting decisions from.
    pub fn start_push(&self) -> Result<JoinHandle<()>, MobileError> {
        let inner = &self.inner;
        if inner.home.is_none() {
            return Err(MobileError::NeedsHomeUrl);
        }
        let Some(approvals) = inner.approvals.clone() else {
            return Err(MobileError::Surface(crate::error::SurfaceError::Invalid(
                "wake-ups read the waiting decisions: build the phone with .approvals(service)"
                    .into(),
            )));
        };
        let ringer = doorbell::Ringer {
            pairing: inner.pairing.clone(),
            store: inner.store.clone(),
            sender: inner.sender.clone(),
            quiet: Arc::clone(&inner.quiet),
            approvals,
            audit: inner.audit.clone(),
            clock: Arc::clone(&inner.clock),
            deliveries: Arc::clone(&inner.deliveries),
            poll: inner.poll,
        };
        Ok(tokio::spawn(ringer.run(inner.doorbell.receiver())))
    }
}

/// Builds a [`Phone`].
pub struct PhoneBuilder {
    engine: SurfaceEngine,
    pairing: Pairing,
    home_url: Option<String>,
    push_key: Option<PushKey>,
    contact: Option<String>,
    allowlist: PushAllowlist,
    quiet: Vec<QuietHours>,
    approvals: Option<Arc<dyn ApprovalService>>,
    audit: Option<AuditTrail>,
    clock: Arc<dyn Clock>,
    doorbell: Option<Arc<PushDoorbell>>,
    operator: String,
    poll: Duration,
    backoff: Duration,
    kernel: Option<adk_kernel::Kernel>,
}

impl std::fmt::Debug for PhoneBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhoneBuilder").field("home_url", &self.home_url).finish_non_exhaustive()
    }
}

impl PhoneBuilder {
    /// The https URL people open the home at, such as `https://home.example.ts.net`: the
    /// passkeys' relying party and the pairing links' base. Without it the phone's routes
    /// answer `400 surface.phone.needs_home_url`.
    #[must_use]
    pub fn home_url(mut self, url: impl Into<String>) -> Self {
        self.home_url = Some(url.into());
        self
    }

    /// The push key; without one a key is made for this process only, and phones subscribe
    /// again after a restart.
    #[must_use]
    pub fn push_key(mut self, key: PushKey) -> Self {
        self.push_key = Some(key);
        self
    }

    /// The contact push services see (`mailto:` or an https URL); the home URL by default.
    #[must_use]
    pub fn contact(mut self, contact: impl Into<String>) -> Self {
        self.contact = Some(contact.into());
        self
    }

    /// The push services a phone may subscribe through; the browsers' own by default.
    #[must_use]
    pub fn push_allowlist(mut self, allowlist: PushAllowlist) -> Self {
        self.allowlist = allowlist;
        self
    }

    /// A quiet time wake-ups wait through, for a phone served without a kernel; may be given
    /// more than once. [`kernel`](Self::kernel) replaces it.
    #[must_use]
    pub fn quiet_hours(mut self, quiet: QuietHours) -> Self {
        self.quiet.push(quiet);
        self
    }

    /// The kernel whose quiet time wake-ups wait through (spec 3.3 §5.4, §10 K3): the area of
    /// the decision's run, the default area otherwise, as the person changed it while the
    /// kernel runs.
    #[must_use]
    pub fn kernel(mut self, kernel: adk_kernel::Kernel) -> Self {
        self.kernel = Some(kernel);
        self
    }

    /// The approval service whose waiting decisions wake the phones.
    #[must_use]
    pub fn approvals(mut self, approvals: Arc<dyn ApprovalService>) -> Self {
        self.approvals = Some(approvals);
        self
    }

    /// Where `surface.notify`, `surface.phone.push` and `surface.phone.passkey` are recorded.
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// The clock; the system's by default.
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The doorbell added to the approval service; a new one by default.
    #[must_use]
    pub fn doorbell(mut self, doorbell: Arc<PushDoorbell>) -> Self {
        self.doorbell = Some(doorbell);
        self
    }

    /// The group of the operator, who reads deliveries, draws pairing QR codes and lists the
    /// phones; `operator` by default.
    #[must_use]
    pub fn operator_group(mut self, group: impl Into<String>) -> Self {
        self.operator = group.into();
        self
    }

    /// How often the push task looks whether quiet time has ended; a minute by default.
    #[must_use]
    pub fn quiet_poll(mut self, every: Duration) -> Self {
        self.poll = every.max(Duration::from_millis(10));
        self
    }

    /// The first wait before a failed push is sent again; two seconds by default.
    #[must_use]
    pub fn push_backoff(mut self, wait: Duration) -> Self {
        self.backoff = wait;
        self
    }

    /// The phone.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when the home URL is not https (or http on
    /// `localhost`) or is an IP address, [`MobileError::PasskeyRefused`] when its host cannot
    /// hold passkeys, [`MobileError::PushKey`] when no key can be made, and
    /// [`MobileError::Source`] (`surface.phone.push_client`) when the push client cannot be
    /// built.
    pub fn build(self) -> Result<Phone, MobileError> {
        let home = self.home_url.as_deref().map(Home::parse).transpose()?;
        let verifier = home
            .as_ref()
            .map(|home| WebauthnVerifier::new(&home.rp))
            .transpose()?
            .map(|verifier| Arc::new(verifier) as Arc<dyn PasskeyVerifier>);
        let key = match self.push_key {
            Some(key) => key,
            None => {
                if home.is_some() {
                    tracing::warn!("no push key given; phones subscribe again after a restart");
                }
                PushKey::generate()?
            }
        };
        let contact = self
            .contact
            .or_else(|| home.as_ref().map(|home| home.url.clone()))
            .unwrap_or_else(|| "https://adk-rust.com".to_string());
        let vapid = Vapid::new(key, contact, Arc::clone(&self.clock));
        // In the kernel's device store, shared by replicas and cleared with the device.
        let store = Attachments::new(self.pairing.store().clone());
        let quiet: Arc<dyn QuietTime> = match self.kernel {
            Some(kernel) => Arc::new(KernelQuiet(kernel)),
            None => Arc::new(Windows(self.quiet)),
        };
        Ok(Phone {
            inner: Arc::new(Inner {
                engine: self.engine,
                pairing: self.pairing,
                home,
                passkeys: Passkeys::new(verifier, store.clone()),
                store,
                sender: PushSender::new(vapid, self.allowlist.clone(), self.backoff)?,
                quiet,
                approvals: self.approvals,
                audit: self.audit,
                clock: self.clock,
                deliveries: Arc::default(),
                doorbell: self.doorbell.unwrap_or_default(),
                operator: self.operator,
                poll: self.poll,
            }),
        })
    }
}
