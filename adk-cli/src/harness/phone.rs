//! The phone on a served harness (spec 3.3 §5.4; feature `mobile`): the home's public URL,
//! the kernel's push key and the doorbell the approval service rings.

use std::sync::Arc;

use adk_core::{AdkError, Result};
use adk_surface::mobile::{PushAllowlist, PushDoorbell, PushKey};

/// What `adk-rust up --home-url` gives the served harness for the phone (spec 3.3 §5.4).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_cli::harness::PhoneSetup;
/// use adk_surface::mobile::{PushDoorbell, PushKey};
///
/// let setup = PhoneSetup::new("https://home.example.ts.net", PushKey::generate().unwrap(), Arc::new(PushDoorbell::new())).unwrap();
/// assert_eq!(setup.host(), "home.example.ts.net");
/// assert_eq!(setup.origin(), "https://home.example.ts.net");
/// ```
#[derive(Clone)]
pub struct PhoneSetup {
    pub(crate) home_url: String,
    pub(crate) key: PushKey,
    pub(crate) doorbell: Arc<PushDoorbell>,
    pub(crate) push_allowlist: Option<PushAllowlist>,
    host: String,
    origin: String,
}

impl std::fmt::Debug for PhoneSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhoneSetup").field("home_url", &self.home_url).finish_non_exhaustive()
    }
}

impl PhoneSetup {
    /// The phone at `home_url`, the https URL people open the home at, waking phones with
    /// `key`; `doorbell` is the one the approval service rings.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when `home_url` is not an https URL (or http on
    /// `localhost`).
    pub fn new(home_url: &str, key: PushKey, doorbell: Arc<PushDoorbell>) -> Result<Self> {
        let home_url = home_url.trim_end_matches('/').to_string();
        let url = reqwest::Url::parse(&home_url).map_err(|error| {
            AdkError::config(format!("--home-url `{home_url}` is not a URL: {error}"))
        })?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let local = url.scheme() == "http" && host == "localhost";
        if host.is_empty() || (url.scheme() != "https" && !local) || url.path() != "/" {
            return Err(AdkError::config(format!(
                "--home-url `{home_url}` must be the https address people open the home at, \
                 such as https://home.example.ts.net, with no path"
            )));
        }
        let origin = url.origin().ascii_serialization();
        Ok(Self { home_url, key, doorbell, push_allowlist: None, host, origin })
    }

    /// Lets phones subscribe through `allowlist`'s push services instead of the browsers' own
    /// (spec 3.3 §5.4), such as a fake push service on `http://localhost` in an end-to-end test.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_cli::harness::PhoneSetup;
    /// use adk_surface::mobile::{PushAllowlist, PushDoorbell, PushKey};
    ///
    /// let setup = PhoneSetup::new("http://localhost:8080", PushKey::generate().unwrap(), Arc::new(PushDoorbell::new()))
    ///     .unwrap()
    ///     .with_push_allowlist(PushAllowlist::new(["localhost"]).with_plain_http_to_localhost());
    /// # let _ = setup;
    /// ```
    #[must_use]
    pub fn with_push_allowlist(mut self, allowlist: PushAllowlist) -> Self {
        self.push_allowlist = Some(allowlist);
        self
    }

    /// The home's host, which the server answers beside loopback.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The home's origin, which the server accepts requests from.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The doorbell the approval service rings.
    pub fn doorbell(&self) -> &Arc<PushDoorbell> {
        &self.doorbell
    }

    /// Where pairing links open: inside the installed app's scope, `<home>/ui`.
    pub fn pairing_base(&self) -> String {
        format!("{}/ui", self.home_url)
    }
}
