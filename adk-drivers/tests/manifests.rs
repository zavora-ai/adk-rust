//! Driver classes as kernel driver manifests (spec 3.5 §7.13, §7.10; spec 3.2 §7.7): the
//! tables keep the spec's rules, each class's manifest parses and satisfies its class, a
//! weaker or misplaced manifest is caught, an MCP server's listing is checked, and the
//! kernel's own driver policy holds what the manifests declare.

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_core::{PolicyDecision, ToolEffect};
use adk_drivers::ads::{InMemoryAds, ads_tools};
use adk_drivers::analytics::{
    InMemoryAnalytics, InMemorySearchConsole, analytics_tools, search_console_tools,
};
use adk_drivers::dns::{InMemoryDns, InMemoryRegistrar, dns_tools, registrar_tools};
use adk_drivers::email::{EmailOutbox, InMemoryEmail, Mailbox, email_tools};
use adk_drivers::social::{InMemorySocial, SocialOutbox, social_tools};
use adk_drivers::{AutonomyCap, Disclosure, DriverKind, Gap, Side, all_tools};
use adk_kernel::drivers::{DriverManifest, DriverPolicy, DriverSupervisor, EffectClass};
use adk_kernel::stores::InMemoryDriverStore;
use adk_tool::mcp::manager::McpServerConfig;

#[test]
fn the_tables_keep_the_spec_rules() {
    let mut names = BTreeSet::new();
    for kind in DriverKind::ALL {
        let class = kind.class();
        assert!(!class.tools.is_empty(), "{kind} has tools");
        for spec in class.tools {
            assert!(names.insert(spec.name), "`{}` is named once", spec.name);
            assert!(
                spec.name.starts_with(&format!("{}_", kind.as_str())),
                "`{}` carries its class prefix",
                spec.name
            );
            let schema = (spec.schema)();
            let properties = schema["properties"].as_object().expect("an object schema");
            for required in spec.required_args() {
                assert!(
                    properties.contains_key(&required),
                    "`{}` requires a declared `{required}`",
                    spec.name
                );
            }
            if let Some(pointer) = spec.budget_arg {
                assert_eq!(
                    spec.cap,
                    AutonomyCap::WithinBudget,
                    "`{}` is measured only when within budget",
                    spec.name
                );
                assert!(
                    properties.contains_key(pointer.trim_start_matches('/')),
                    "`{}` declares its amount",
                    spec.name
                );
                assert!(
                    properties.contains_key("currency"),
                    "`{}` declares its currency",
                    spec.name
                );
            }
            // A-I2: public agents read and reply in thread; they never publish, spend or send in bulk.
            if spec.side == Side::Public {
                assert!(
                    matches!(spec.class, EffectClass::Read | EffectClass::SendExternal),
                    "`{}` on the public side is only a read or a reply",
                    spec.name
                );
            }
            if spec.class == EffectClass::Read {
                assert_eq!(spec.cap, AutonomyCap::Read, "`{}` reads at Read", spec.name);
            }
        }
    }
    let cap = |name: &str| all_tools().find(|spec| spec.name == name).unwrap();
    // A-I21 and the §7.13 table: DNS changes and campaign launches are always held.
    assert_eq!(cap("dns_change_records").cap, AutonomyCap::AlwaysHeld);
    assert_eq!(cap("dns_change_records").class, EffectClass::PublishPublic);
    assert_eq!(cap("ads_launch_campaign").cap, AutonomyCap::AlwaysHeld);
    assert_eq!(cap("ads_launch_campaign").also, Some(EffectClass::PublishPublic));
    assert_eq!(cap("registrar_renew_domain").class, EffectClass::Financial);
    assert_eq!(cap("social_post").cap, AutonomyCap::Propose);
    assert_eq!(cap("social_reply_in_thread").cap, AutonomyCap::Propose);
    assert_eq!(cap("email_send_newsletter").cap, AutonomyCap::WithinPlaybook);
    assert_eq!(
        cap("email_list_inbox").side,
        Side::Public,
        "the inbox is read in the public partition"
    );
}

#[test]
fn every_class_manifest_parses_and_satisfies_its_class() {
    for kind in DriverKind::ALL {
        let class = kind.class();
        for side in [Side::Public, Side::BackOffice] {
            if !class.serves(side) {
                assert!(class.manifest(kind.as_str(), McpServerConfig::default(), side).is_err());
                continue;
            }
            let text = class.manifest_toml(kind.as_str(), "mcp-driver", &["--stdio"], side);
            let parsed = DriverManifest::from_toml(&text)
                .unwrap_or_else(|error| panic!("{kind} {side:?}: {error}\n{text}"));
            let check = class.check_manifest(&parsed, side);
            assert!(check.is_satisfied(), "{kind} {side:?}: {check}");
            let server = McpServerConfig {
                command: "mcp-driver".into(),
                args: vec!["--stdio".into()],
                ..Default::default()
            };
            let built = class.manifest(kind.as_str(), server, side).unwrap();
            assert_eq!(built.tools, parsed.tools, "{kind} {side:?}: one tool table");
            assert_eq!(
                built.partitions,
                [side.partition()],
                "{kind} {side:?}: the side's partition"
            );
        }
    }
}

#[test]
fn a_weaker_or_misplaced_manifest_is_caught() {
    let dns = DriverKind::Dns.class();
    let weak = DriverManifest::from_toml(
        "id = \"dns\"\ncommand = \"mcp-dns\"\npartitions = [\"public\"]\n[tools]\ndns_list_records = \"read\"\ndns_change_records = \"write_local\"\n",
    )
    .unwrap();
    let gaps = dns.check_manifest(&weak, Side::BackOffice).gaps;
    assert!(gaps.contains(&Gap::WrongPartition { partition: "public".into() }), "{gaps:?}");
    assert!(
        gaps.contains(&Gap::WeakerClass {
            tool: "dns_change_records".into(),
            contract: EffectClass::PublishPublic,
            manifest: EffectClass::WriteLocal,
        }),
        "{gaps:?}"
    );
    let email = DriverKind::Email.class();
    let leaky = DriverManifest::from_toml(
        "id = \"email\"\ncommand = \"mcp-email\"\npartitions = [\"public\"]\n[tools]\nemail_list_inbox = \"read\"\n\
         email_reply_in_thread = \"send_external\"\nemail_send_newsletter = \"send_external\"\n",
    )
    .unwrap();
    let gaps = email.check_manifest(&leaky, Side::Public).gaps;
    assert_eq!(
        gaps,
        [Gap::ForeignTool { tool: "email_send_newsletter".into() }],
        "bulk sending never reaches the public partition"
    );
    let stricter = DriverManifest::from_toml(
        "id = \"email\"\ncommand = \"mcp-email\"\npartitions = [\"public\"]\n[tools]\nemail_list_inbox = \"read\"\n\
         email_reply_in_thread = \"publish_public\"\n",
    )
    .unwrap();
    assert!(
        email.check_manifest(&stricter, Side::Public).is_satisfied(),
        "a stricter class satisfies the contract"
    );
}

#[tokio::test]
async fn a_server_listing_is_checked_against_the_class() {
    let dns: Vec<(String, serde_json::Value)> = dns_tools(Arc::new(InMemoryDns::new()))
        .iter()
        .map(|tool| (tool.name().to_string(), tool.parameters_schema().unwrap()))
        .collect();
    let class = DriverKind::Dns.class();
    assert!(
        class.check_listing(Side::BackOffice, &dns).is_satisfied(),
        "the in-process tools satisfy their class"
    );
    let mut short = dns.clone();
    short[1].1["properties"].as_object_mut().unwrap().remove("changes");
    short.push(("dns_delete_zone".into(), serde_json::json!({"type": "object"})));
    let gaps = class.check_listing(Side::BackOffice, &short).gaps;
    assert!(
        gaps.contains(&Gap::MissingArgument {
            tool: "dns_change_records".into(),
            argument: "changes".into()
        }),
        "{gaps:?}"
    );
    assert!(
        gaps.contains(&Gap::ForeignTool { tool: "dns_delete_zone".into() }),
        "an unlisted tool would be served as `other`: {gaps:?}"
    );
    assert!(
        class
            .check_listing(Side::BackOffice, &dns[..1])
            .gaps
            .contains(&Gap::MissingTool { tool: "dns_change_records".into() })
    );
}

#[test]
fn in_process_toolsets_name_and_class_their_tools_as_the_manifests_do() {
    let disclosure = Disclosure::new("Wanjiru's Bakery").unwrap();
    let email = Arc::new(
        EmailOutbox::builder(
            Arc::new(InMemoryEmail::new()),
            Mailbox::new("hello@bakery.example"),
            disclosure.clone(),
        )
        .build(),
    );
    let social =
        Arc::new(SocialOutbox::builder(Arc::new(InMemorySocial::new()), disclosure).build());
    let sets = [
        (DriverKind::Dns, Side::BackOffice, dns_tools(Arc::new(InMemoryDns::new()))),
        (
            DriverKind::Registrar,
            Side::BackOffice,
            registrar_tools(Arc::new(InMemoryRegistrar::new())),
        ),
        (DriverKind::Email, Side::Public, email_tools(email.clone(), Side::Public)),
        (DriverKind::Email, Side::BackOffice, email_tools(email, Side::BackOffice)),
        (
            DriverKind::Analytics,
            Side::BackOffice,
            analytics_tools(Arc::new(InMemoryAnalytics::new())),
        ),
        (
            DriverKind::SearchConsole,
            Side::BackOffice,
            search_console_tools(Arc::new(InMemorySearchConsole::new())),
        ),
        (DriverKind::Social, Side::Public, social_tools(social.clone(), Side::Public)),
        (DriverKind::Social, Side::BackOffice, social_tools(social, Side::BackOffice)),
        (DriverKind::Advertising, Side::BackOffice, ads_tools(Arc::new(InMemoryAds::new()))),
    ];
    for (kind, side, tools) in sets {
        let class = kind.class();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
        let expected: Vec<&str> = class.tools_for(side).map(|spec| spec.name).collect();
        assert_eq!(names, expected, "{kind} {side:?}");
        for tool in &tools {
            let spec = class.tool(tool.name()).unwrap();
            assert_eq!(tool.effect(), spec.class.tool_effect(), "`{}`", tool.name());
            assert_eq!(
                tool.effect() == ToolEffect::ReadOnly,
                tool.is_read_only(),
                "`{}`",
                tool.name()
            );
            assert!(
                tool.origin().starts_with(adk_drivers::IN_PROCESS_ORIGIN_PREFIX),
                "`{}`",
                tool.name()
            );
        }
    }
}

#[tokio::test]
async fn the_kernel_holds_what_a_class_manifest_declares() {
    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
    for kind in [DriverKind::Dns, DriverKind::Advertising, DriverKind::Registrar] {
        let text = kind.class().manifest_toml(kind.as_str(), "mcp-driver", &[], Side::BackOffice);
        drivers.mount(DriverManifest::from_toml(&text).unwrap(), "owner").await.unwrap();
    }
    let policy = DriverPolicy::new(drivers);
    for (driver, tool, held) in [
        ("dns", "dns_list_records", false),
        ("dns", "dns_change_records", true),
        ("ads", "ads_performance", false),
        ("ads", "ads_launch_campaign", true),
        ("ads", "ads_set_bid", true),
        ("registrar", "registrar_renew_domain", true),
    ] {
        let decision = policy.decide(driver, tool, "sha256:any");
        assert_eq!(
            matches!(decision, PolicyDecision::RequireApproval { .. }),
            held,
            "`{tool}`: {decision:?}"
        );
        assert!(
            !matches!(decision, PolicyDecision::Deny { .. }),
            "`{tool}` is mounted: {decision:?}"
        );
    }
}
