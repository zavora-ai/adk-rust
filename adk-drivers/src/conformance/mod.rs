//! Conformance suites for every driver contract (spec 3.5 §11; feature `conformance`).
//!
//! Each suite runs its cases against fresh adapters a harness makes, and panics naming the
//! case and the rule that failed. The crate runs every suite against its in-memory adapters
//! and its reference adapters over local servers or recorded responses; a third-party
//! adapter runs the same suite to show it keeps the contract.
//!
//! | Suite | Harness | Contract |
//! |---|---|---|
//! | [`dns_driver_contract`] | [`DnsHarness`] | [`DnsDriver`](crate::dns::DnsDriver) |
//! | [`registrar_driver_contract`] | [`RegistrarHarness`] | [`RegistrarDriver`](crate::dns::RegistrarDriver) |
//! | [`email_driver_contract`] | [`EmailHarness`] | [`EmailDriver`](crate::email::EmailDriver) and the [`EmailOutbox`](crate::email::EmailOutbox) rules over it |
//! | [`analytics_driver_contract`] | [`AnalyticsHarness`] | [`AnalyticsDriver`](crate::analytics::AnalyticsDriver) |
//! | [`search_console_driver_contract`] | [`SearchConsoleHarness`] | [`SearchConsoleDriver`](crate::analytics::SearchConsoleDriver) |
//! | [`social_driver_contract`] | [`SocialHarness`] | [`SocialDriver`](crate::social::SocialDriver) and the [`SocialOutbox`](crate::social::SocialOutbox) rules over it |
//! | [`ads_driver_contract`] | [`AdsHarness`] | [`AdsDriver`](crate::ads::AdsDriver) |

mod ads;
mod analytics;
mod dns;
mod email;
mod social;

pub use ads::{AdsHarness, ads_driver_contract};
pub use analytics::{
    AnalyticsHarness, SearchConsoleHarness, analytics_driver_contract,
    search_console_driver_contract,
};
pub use dns::{DnsHarness, RegistrarHarness, dns_driver_contract, registrar_driver_contract};
pub use email::{EmailHarness, EmailServer, InMemoryEmailHarness, email_driver_contract};
pub use social::{
    InMemorySocialHarness, Published, SocialHarness, SocialServer, social_driver_contract,
};
