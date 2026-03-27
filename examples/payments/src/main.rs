use std::env;

struct Scenario {
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    references: &'static [&'static str],
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "acp-human-present",
        title: "ACP human-present checkout",
        summary: "Agent-surface shopper flow using ACP stable checkout, delegated payment, completion, and webhook-style order follow-up against the canonical journal.",
        references: &[
            "adk-payments/tests/acp_integration_tests.rs",
            "adk-payments/tests/acp_experimental_integration_tests.rs",
        ],
    },
    Scenario {
        id: "ap2-human-present",
        title: "AP2 human-present multi-actor flow",
        summary: "Merchant cart mandate, credentials-provider authorization, shopper payment mandate, and payment-processor receipt handling through one AP2 adapter.",
        references: &["adk-payments/tests/ap2_integration_tests.rs"],
    },
    Scenario {
        id: "ap2-human-not-present",
        title: "AP2 human-not-present intent flow",
        summary: "Intent mandate authority, autonomous completion when allowed, and forced buyer reconfirmation when the mandate requires user return.",
        references: &["adk-payments/tests/ap2_integration_tests.rs"],
    },
    Scenario {
        id: "dual-protocol",
        title: "Dual-protocol backend correlation",
        summary: "ACP and AP2 identifiers correlate into one canonical transaction record with explicit lossy-conversion refusal at protocol boundaries.",
        references: &["adk-payments/tests/cross_protocol_correlation_tests.rs"],
    },
    Scenario {
        id: "post-compaction-recall",
        title: "Post-compaction transaction recall",
        summary: "Durable transaction state and masked memory retain order-follow-up continuity even when conversational history is compacted away.",
        references: &[
            "adk-payments/src/journal/store.rs",
            "adk-payments/tests/cross_protocol_correlation_tests.rs",
        ],
    },
];

fn list_scenarios() {
    for scenario in SCENARIOS {
        println!("{}: {}", scenario.id, scenario.title);
    }
}

fn show_scenario(id: &str) {
    let Some(scenario) = SCENARIOS.iter().find(|scenario| scenario.id == id) else {
        eprintln!("unknown scenario `{id}`");
        eprintln!("run `cargo run --manifest-path examples/payments/Cargo.toml -- list`");
        std::process::exit(1);
    };

    println!("{}", scenario.title);
    println!();
    println!("{}", scenario.summary);
    println!();
    println!("Reference paths:");
    for reference in scenario.references {
        println!("- {reference}");
    }
}

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        None | Some("list") => list_scenarios(),
        Some("show") => {
            let Some(id) = args.next() else {
                eprintln!("usage: cargo run --manifest-path examples/payments/Cargo.toml -- show <scenario>");
                std::process::exit(1);
            };
            show_scenario(&id);
        }
        Some(other) => {
            eprintln!("unknown command `{other}`");
            eprintln!("supported commands: list, show <scenario>");
            std::process::exit(1);
        }
    }
}
