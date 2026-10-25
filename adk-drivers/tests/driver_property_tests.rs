//! Property tests for the drivers' canonical forms, signatures, manifests and rules (spec
//! 3.5 §7.13, §11, A-I17, A-I21), 256 cases each.

use std::collections::BTreeMap;

use adk_core::PolicyDecision;
use adk_drivers::dns::{DnsRecord, RecordChange};
use adk_drivers::email::{InboundEmail, Mailbox, OutboundEmail};
use adk_drivers::{
    AutonomyCap, Disclosure, DriverBudget, DriverCapPolicy, DriverKind, InMemorySuppressions,
    ReplyTemplates, Side, Suppression, SuppressionList, SuppressionScope, all_tools,
};
use adk_kernel::drivers::{DriverManifest, EffectClass};
use proptest::prelude::*;
use serde_json::json;

fn config() -> ProptestConfig {
    ProptestConfig { cases: 256, ..ProptestConfig::default() }
}

fn label() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9]([a-zA-Z0-9-]{0,10}[a-zA-Z0-9])?"
}

fn text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 .,!?é€—'\\n]{0,300}"
}

proptest! {
    #![proptest_config(config())]

    /// A-I17: the disclosure ends every body once, however often it is applied.
    #[test]
    fn the_disclosure_ends_every_body_once(body in text(), owner in "[A-Z][a-z]{2,12}( [A-Z][a-z]{2,12})?") {
        let disclosure = Disclosure::new(owner.as_str()).unwrap();
        let once = disclosure.apply(&body);
        prop_assert!(disclosure.is_present(&once));
        prop_assert_eq!(disclosure.apply(&once), once.clone());
        prop_assert_eq!(once.matches(disclosure.line()).count(), body.matches(disclosure.line()).count() + usize::from(!disclosure.is_present(&body)));
    }

    /// A reworded disclosure that drops the owner or the AI statement is refused.
    #[test]
    fn a_disclosure_cannot_be_reworded_away(line in "[a-z ]{0,60}") {
        let disclosure = Disclosure::new("Wanjiru's Bakery").unwrap();
        prop_assert!(disclosure.with_line(line).is_err());
    }

    /// Canonical DNS records are stable: normalising twice is normalising once, and case
    /// never matters.
    #[test]
    fn dns_records_normalise_idempotently(
        labels in prop::collection::vec(label(), 1..4),
        upper in any::<bool>(),
        kind in 0..4usize,
        octets in prop::array::uniform4(any::<u8>()),
        preference in any::<u16>(),
    ) {
        let name = labels.join(".");
        let name = if upper { name.to_ascii_uppercase() } else { name };
        let (rtype, value) = match kind {
            0 => ("a", format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3])),
            1 => ("CNAME", format!("Target.{name}")),
            2 => ("mx", format!("{preference} Mail.{name}")),
            _ => ("TXT", format!("v=spf1 ip4:{}.{}.{}.{} -all", octets[0], octets[1], octets[2], octets[3])),
        };
        let record = DnsRecord::new(name.as_str(), rtype, 300, value.as_str()).normalized().unwrap();
        prop_assert_eq!(record.normalized().unwrap(), record.clone());
        let lower = DnsRecord::new(name.to_ascii_lowercase(), rtype.to_ascii_lowercase(), 300, value.as_str()).normalized().unwrap();
        prop_assert_eq!(record.name.clone(), lower.name);
        prop_assert!(record.name.ends_with('.'));
    }

    /// A change outside its zone is always refused.
    #[test]
    fn changes_outside_the_zone_are_refused(zone in label(), other in label(), name in label()) {
        prop_assume!(!zone.eq_ignore_ascii_case(&other));
        let zone = format!("{zone}.test");
        let record = DnsRecord::new(format!("{name}.{other}.example"), "A", 300, "192.0.2.1");
        let refused = RecordChange::Add { record }.validated(&zone).is_err();
        prop_assert!(refused);
    }

    /// A-I21: a held tool is held whatever its arguments; a within-budget call is allowed
    /// exactly when its amount is within the cap in the cap's currency.
    #[test]
    fn held_tools_stay_held_and_budgets_hold(amount in 0u64..10_000, cap in 0u64..10_000, euro in any::<bool>(), noise in "[a-z]{0,8}") {
        let policy = DriverCapPolicy::new(DriverBudget::new().cap("ads_set_bid", "USD", cap).cap("registrar_renew_domain", "USD", cap));
        for spec in all_tools().filter(|spec| spec.cap == AutonomyCap::AlwaysHeld) {
            let decision = policy.decide(spec.name, &json!({ "noise": noise, "amount": amount }));
            let held = matches!(decision, PolicyDecision::RequireApproval { .. });
            prop_assert!(held, "{}", spec.name);
        }
        let currency = if euro { "EUR" } else { "USD" };
        let bid = policy.decide("ads_set_bid", &json!({"campaign": "c", "bid_minor": amount, "currency": currency}));
        prop_assert_eq!(bid == PolicyDecision::Allow, amount <= cap && !euro);
        let renew = policy.decide("registrar_renew_domain", &json!({"domain": "d.test", "years": 1, "max_price_minor": amount, "currency": currency}));
        prop_assert_eq!(renew == PolicyDecision::Allow, amount <= cap && !euro);
        prop_assert_eq!(policy.decide("ads_pause_campaign", &json!({"campaign": noise})), PolicyDecision::Allow);
    }

    /// Lowering any tool's class in a class manifest is caught; raising it is not.
    #[test]
    fn a_manifest_that_loosens_a_tool_is_caught(kind in 0..DriverKind::ALL.len(), pick in any::<prop::sample::Index>(), raise in any::<bool>()) {
        let kind = DriverKind::ALL[kind];
        let class = kind.class();
        let side = if class.serves(Side::BackOffice) { Side::BackOffice } else { Side::Public };
        let tools: Vec<_> = class.tools_for(side).collect();
        let spec = tools[pick.index(tools.len())];
        let text = class.manifest_toml(kind.as_str(), "mcp-driver", &[], side);
        let changed = if raise { EffectClass::Financial } else { EffectClass::Read };
        let text = text.replace(&format!("{} = \"{}\"", spec.name, spec.class.as_str()), &format!("{} = \"{}\"", spec.name, changed.as_str()));
        let manifest = DriverManifest::from_toml(&text).unwrap();
        let satisfied = class.check_manifest(&manifest, side).is_satisfied();
        let loosened = changed.strictness() < spec.class.strictness() || (changed != spec.class && changed.strictness() == spec.class.strictness());
        prop_assert_eq!(satisfied, !loosened, "{} {:?} -> {:?}", spec.name, spec.class, changed);
    }

    /// An outbound message reads back as written: subject, text and threading.
    #[test]
    fn outbound_mail_round_trips(subject in "[a-zA-Z0-9 é€—'?]{1,80}", body in text(), parent in "[a-z0-9]{4,12}") {
        let message: OutboundEmail = serde_json::from_value(json!({
            "messageId": "out@bakery.example",
            "from": {"address": "hello@bakery.example"},
            "to": [{"address": "ana@example.org"}],
            "subject": subject,
            "text": body,
            "inReplyTo": format!("{parent}@example.org"),
        })).unwrap();
        let wire = message.to_wire(chrono::Utc::now()).unwrap();
        prop_assert!(String::from_utf8(wire.clone()).unwrap().lines().all(|line| line.len() <= 998));
        let back = InboundEmail::parse(&wire, chrono::Utc::now());
        prop_assert_eq!(back.subject.trim(), subject.trim());
        prop_assert_eq!(back.text, body.trim().to_string());
        prop_assert_eq!(back.in_reply_to, Some(format!("{parent}@example.org")));
        prop_assert_eq!(back.to, vec![Mailbox::new("ana@example.org")]);
    }

    /// A header value with a line break never reaches the wire (no header injection).
    #[test]
    fn header_injection_is_refused(prefix in "[a-z ]{0,20}", suffix in "[A-Za-z: ]{1,30}") {
        let message: OutboundEmail = serde_json::from_value(json!({
            "messageId": "out@bakery.example",
            "from": {"address": "hello@bakery.example"},
            "to": [{"address": "ana@example.org"}],
            "subject": format!("{prefix}\r\nBcc: {suffix}"),
            "text": "hi",
        })).unwrap();
        prop_assert!(message.to_wire(chrono::Utc::now()).is_err());
    }

    /// Templates fill every field and refuse unknown ones.
    #[test]
    fn templates_fill_their_fields(name in "[A-Za-z]{1,20}", time in "[0-9]{1,2}(am|pm)", extra in "[a-z]{1,8}") {
        let templates = ReplyTemplates::new().template("hours", "Hi {name}, we open at {time}.");
        let fields = BTreeMap::from([("name".to_string(), name.clone()), ("time".to_string(), time.clone())]);
        prop_assert_eq!(templates.render("hours", &fields).unwrap(), format!("Hi {name}, we open at {time}."));
        let mut more = fields.clone();
        more.insert(format!("x{extra}"), "y".into());
        prop_assert!(templates.render("hours", &more).is_err());
    }
}

proptest! {
    #![proptest_config(config())]

    /// A-I17: a wider suppression always wins, in whatever order they arrive.
    #[test]
    fn the_widest_suppression_wins(order in prop::collection::vec(any::<bool>(), 1..6), address in "[a-z]{1,8}@[a-z]{1,8}\\.test") {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            let list = InMemorySuppressions::new();
            for all in &order {
                let scope = if *all { SuppressionScope::All } else { SuppressionScope::Marketing };
                list.suppress(Suppression::new(&address.to_uppercase(), scope, "test")).await.unwrap();
            }
            let expected = if order.iter().any(|all| *all) { SuppressionScope::All } else { SuppressionScope::Marketing };
            let found = list.lookup(&address).await.unwrap().unwrap();
            assert_eq!(found.scope, expected);
        });
    }
}

#[cfg(feature = "rfc2136")]
mod tsig {
    use adk_drivers::dns::tsig::TsigKey;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(super::config())]

        /// A signed message verifies, and changing any byte after the header's id fails it.
        #[test]
        fn a_signature_covers_the_whole_message(id in any::<u16>(), name in "[a-z]{1,12}", flip in 2usize..40, bit in 0u8..8) {
            let key = TsigKey::hmac_sha256("prop-key", "BwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSY=").unwrap();
            let mut message = hickory_proto::op::Message::new(id, hickory_proto::op::MessageType::Query, hickory_proto::op::OpCode::Update);
            let zone = hickory_proto::rr::Name::from_ascii(format!("{name}.test.")).unwrap();
            message.add_query(hickory_proto::op::Query::query(zone, hickory_proto::rr::RecordType::SOA));
            let wire = message.to_vec().unwrap();
            let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap();
            let (signed, mac) = key.sign(&wire, None, now).unwrap();
            prop_assert_eq!(key.verify(&signed, None, true, now).unwrap(), mac);
            let at = flip % wire.len();
            prop_assume!(at >= 2);
            let mut altered = signed.clone();
            altered[at] ^= 1 << bit;
            prop_assert!(key.verify(&altered, None, true, now).is_err());
        }
    }
}
