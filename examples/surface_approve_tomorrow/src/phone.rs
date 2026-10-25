//! The phone's push notifications in this scenario (spec 3.3 §5.4, §12 phase 1): the hook WP M
//! extends.
//!
//! "Nothing notifies during quiet time" is checked against every notifier `adk-rust up`
//! wires. Today that is the Telegram doorbell, which the fake Bot API records. The phone is
//! woken by Web Push, which arrives with WP M; with the `phone-push` feature this module is
//! where M's push receiver joins the list of notifiers the check reads, and until it does the
//! feature refuses to run rather than pass without looking.

use chrono::{DateTime, Utc};

/// Something that can put a notification in front of the person, as the scenario observes
/// it.
pub trait Notifier {
    /// The notifier in words: "Telegram", "the phone".
    fn name(&self) -> &str;

    /// When it notified anyone between `from` and `to`.
    fn notified_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>>;
}

impl Notifier for crate::telegram::FakeTelegram {
    fn name(&self) -> &str {
        "Telegram"
    }

    fn notified_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
        crate::telegram::FakeTelegram::notified_between(self, from, to)
            .into_iter()
            .map(|call| call.at)
            .collect()
    }
}

/// The phone's Web Push, as a receiver on loopback the kernel delivers to (WP M).
#[cfg(feature = "phone-push")]
pub struct PhonePush;

#[cfg(feature = "phone-push")]
impl PhonePush {
    /// Subscribes a push receiver to the kernel at `base_url`.
    ///
    /// # Errors
    ///
    /// Always, until `adk-rust up` sends Web Push (spec 3.3 §5.4, WP M).
    pub async fn subscribe(_base_url: &str) -> anyhow::Result<Self> {
        anyhow::bail!(
            "adk-rust up sends no Web Push yet (spec 3.3 §5.4, WP M): build this example \
             without the phone-push feature"
        )
    }
}

#[cfg(feature = "phone-push")]
impl Notifier for PhonePush {
    fn name(&self) -> &str {
        "the phone"
    }

    fn notified_between(&self, _from: DateTime<Utc>, _to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
        Vec::new()
    }
}
