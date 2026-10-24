//! Properties of device pairing and chat cards (spec 3.3 §10 K1, K2, §11 S-I5, S-I7):
//!
//! - a device with any set of scopes is admitted to exactly the routes those scopes open,
//!   and to none of the kernel's other routes;
//! - a code claims however it is typed (case, separators, look-alikes), and a code with one
//!   character changed claims nothing;
//! - a decision card's summary never carries an argument value (feature `channels`).

use std::sync::Arc;

use adk_kernel::pairing::{DeviceScope, InMemoryDeviceStore, IssueCode, Pairing, device_scope_for};
use axum::http::Method;
use proptest::prelude::*;

/// Every route of the control plane, the inbox and the server beside it, with the scope a
/// device needs for it, `None` for the routes closed to devices.
const ROUTES: &[(&str, &str, Option<DeviceScope>)] = &[
    ("GET", "/api/kernel/health", Some(DeviceScope::Read)),
    ("GET", "/api/kernel/processes", Some(DeviceScope::Read)),
    ("POST", "/api/kernel/processes", None),
    ("POST", "/api/kernel/processes/p1/kill", None),
    ("POST", "/api/kernel/processes/p1/nice", None),
    ("POST", "/api/kernel/processes/p1/suspend", None),
    ("POST", "/api/kernel/processes/p1/resume", None),
    ("GET", "/api/kernel/events", Some(DeviceScope::Read)),
    ("POST", "/api/kernel/attach/s1", Some(DeviceScope::Read)),
    ("POST", "/api/kernel/freeze", Some(DeviceScope::Stop)),
    ("POST", "/api/kernel/unfreeze", None),
    ("GET", "/api/kernel/agenda", Some(DeviceScope::Read)),
    ("POST", "/api/kernel/agenda/a1/approve", None),
    ("POST", "/api/kernel/agenda/a1/reject", None),
    ("POST", "/api/kernel/agenda/a1/schedule", None),
    ("GET", "/api/kernel/drivers", None),
    ("POST", "/api/kernel/drivers/d1/mount", None),
    ("GET", "/api/kernel/proc/ledger", None),
    ("GET", "/api/kernel/proc/baselines", None),
    ("POST", "/api/kernel/pairing", None),
    ("GET", "/api/kernel/devices", None),
    ("POST", "/api/kernel/devices/d1/revoke", None),
    ("GET", "/api/kernel/channels", None),
    ("POST", "/api/kernel/channels/pair", None),
    ("POST", "/api/kernel/channels/c1/revoke", None),
    ("GET", "/api/approvals", Some(DeviceScope::Read)),
    ("GET", "/api/approvals/a1", Some(DeviceScope::Read)),
    ("POST", "/api/approvals/a1/decide", Some(DeviceScope::Decide)),
    ("POST", "/api/approvals/a1/cancel", None),
    ("POST", "/api/approvals/delegations", None),
    ("DELETE", "/api/approvals/delegations/d1", None),
    ("GET", "/api/rules", None),
    ("POST", "/api/rules", None),
    ("POST", "/api/runs", None),
    ("POST", "/api/run_sse", None),
    ("POST", "/api/admin/unfreeze", None),
    ("GET", "/api/sessions/adk-rust/ana", None),
];

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// A device is admitted to exactly the routes its scopes open.
    #[test]
    fn a_device_reaches_exactly_the_routes_its_scopes_open(
        read in any::<bool>(),
        decide in any::<bool>(),
        stop in any::<bool>(),
    ) {
        let scopes: Vec<DeviceScope> = [(read, DeviceScope::Read), (decide, DeviceScope::Decide), (stop, DeviceScope::Stop)]
            .into_iter()
            .filter_map(|(on, scope)| on.then_some(scope))
            .collect();
        prop_assume!(!scopes.is_empty());
        runtime().block_on(async {
            let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
            let issued = pairing
                .issue(IssueCode::new("ana", ["operator"]).with_scopes(scopes.iter().copied()))
                .await
                .unwrap();
            let token = pairing.claim(&issued.code, None).await.unwrap().token;
            for (method, path, needs) in ROUTES {
                let method: Method = method.parse().unwrap();
                assert_eq!(device_scope_for(&method, path), *needs, "{method} {path}");
                let admitted = pairing.authorize(&token, &method, path).await.is_ok();
                let expected = needs.is_some_and(|scope| scopes.contains(&scope));
                assert_eq!(admitted, expected, "{method} {path} with {scopes:?}");
            }
        });
    }

    /// A code claims however it is typed; one changed character claims nothing.
    #[test]
    fn a_code_claims_however_it_is_typed_and_only_itself(
        lower in any::<bool>(),
        separator in prop::sample::select(vec!["-", " ", "", "."]),
        position in 0usize..12,
        replacement in prop::sample::select("0123456789ABCDEFGHJKMNPQRSTVWXYZ".chars().collect::<Vec<_>>()),
    ) {
        runtime().block_on(async {
            let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
            let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
            let plain: String = issued.code.chars().filter(|c| *c != '-').collect();
            let mut changed: Vec<char> = plain.chars().collect();
            if changed[position] != replacement {
                changed[position] = replacement;
                let changed: String = changed.into_iter().collect();
                assert!(pairing.claim(&changed, None).await.is_err(), "{changed} is not {plain}");
            }
            let typed = format!("{}{separator}{}", &plain[..4], &plain[4..]);
            let typed = if lower { typed.to_lowercase() } else { typed };
            assert!(pairing.claim(&typed, None).await.is_ok(), "{typed} claims {}", issued.code);
        });
    }
}

#[cfg(feature = "channels")]
mod cards {
    use adk_core::{
        ApprovalPolicy, ApprovalRequest, ApprovalScope, ApproverSelector, ToolConfirmationRequest,
    };
    use adk_kernel::channels::DecisionCard;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// No argument value survives into a card's summary: values of any length, with
        /// newlines, tabs and other control characters inside them, standing as words of
        /// their own (review findings 2 and 12).
        #[test]
        fn a_card_summary_never_carries_an_argument_value(
            values in prop::collection::vec("[A-Za-z0-9@._-]{1,8}([\n\r\t\u{7} ][A-Za-z0-9@._-]{1,8}){0,2}", 1..5),
            amount in 0u64..1_000_000,
            words in prop::collection::vec("[a-z]{2,8}", 0..6),
        ) {
            let mut summary = words.join(" ");
            for value in &values {
                summary.push(' ');
                summary.push_str(value);
            }
            summary.push_str(&format!(" for {amount} USD"));
            let args = serde_json::json!({ "values": values, "nested": { "amount": amount } });
            let call = ToolConfirmationRequest::new("tool", args);
            let request = ApprovalRequest::new(
                "a1",
                ApprovalScope::new("adk-rust", "ana", "s1"),
                &call,
                ApprovalPolicy::new([ApproverSelector::group("operator")]),
            )
            .with_summary(summary);
            let card = DecisionCard::from_request(&request, None);
            // The card's words: a value survives only as a word or run of words of its own.
            let shown: Vec<&str> = card.summary.split(' ').collect();
            for value in &values {
                let words: Vec<&str> = value
                    .split(|c: char| c.is_whitespace() || c.is_control())
                    .filter(|word| !word.is_empty())
                    .collect();
                let found = shown.windows(words.len()).any(|window| {
                    window.iter().zip(&words).all(|(shown, word)| shown.eq_ignore_ascii_case(word))
                });
                prop_assert!(!found, "{value:?} in {}", card.summary);
            }
            prop_assert!(
                !shown.contains(&amount.to_string().as_str()),
                "{amount} in {}",
                card.summary
            );
        }
    }
}
