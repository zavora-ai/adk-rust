//! The third-party resource check a public body's launch gate adds (spec 3.5 §7.16.1
//! Government: "no third-party scripts or trackers"; S16). It lives in `adk-site`'s gate as
//! [`adk_site::gate::ThirdPartyResources`] and reports as its own check,
//! `GateCheck::ThirdPartyResources`.

/// The third-party resource check, under the name the kinds use.
pub use adk_site::gate::ThirdPartyResources as ThirdPartyScripts;
