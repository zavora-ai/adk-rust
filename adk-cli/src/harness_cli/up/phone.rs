//! `adk-rust up --home-url` (spec 3.3 §5.4): the phone's setup from the flags.

use std::path::Path;
use std::sync::Arc;

use adk_cli::harness::PhoneSetup;
use adk_kernel::pairing::Pairing;
use adk_surface::mobile::{PushDoorbell, PushKey};
use anyhow::{Context, Result, bail};

/// The phone `up` serves: its setup and the doorbell the approval service rings.
pub(super) struct Phone {
    pub(super) setup: PhoneSetup,
}

impl Phone {
    /// The phone at `home_url`, with the push key at `push_key` or `<home>/push.key`; every
    /// caller signs in, so `token` is required.
    pub(super) fn open(
        home_url: &str,
        push_key: Option<&Path>,
        kernel_home: &Path,
        token: Option<&str>,
    ) -> Result<Self> {
        if token.is_none() {
            bail!(
                "--home-url needs --auth-token (or ADK_SERVE_TOKEN): behind a proxy every request \
                 comes from loopback, so the operator signs in to the home with the token and \
                 phones with their device tokens"
            );
        }
        let path = push_key.map_or_else(|| kernel_home.join(PushKey::FILE), Path::to_path_buf);
        let key = PushKey::load_or_create(&path)
            .with_context(|| format!("the push key at {}", path.display()))?;
        let setup = PhoneSetup::new(home_url, key, Arc::new(PushDoorbell::new()))?;
        Ok(Self { setup })
    }

    /// Rings the phone's doorbell when a decision is submitted, beside the chat's.
    pub(super) fn doorbell(&self) -> Arc<PushDoorbell> {
        Arc::clone(self.setup.doorbell())
    }

    /// The kernel's pairing with links into the installed app and the surface's routes open
    /// to devices (spec 3.3 §10 K2).
    pub(super) fn pairing(&self, pairing: Pairing) -> Pairing {
        pairing
            .with_home_url(self.setup.pairing_base())
            .with_device_routes(Arc::new(adk_surface::web::device_scope_for))
    }

    /// Where chat cards link: the home's own address.
    #[cfg_attr(not(feature = "channels"), allow(dead_code))]
    pub(super) fn home_link(&self) -> String {
        format!("{}/", self.setup.pairing_base())
    }
}
