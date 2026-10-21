//! Property tests P1–P8 of spec 3.1 §11 for standing approvals, modes and the policy
//! table.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    ActingContext, PolicyDecision, RunConfig, ToolApproval, ToolConfirmationRequest, ToolOrigin,
};
use adk_harness::approvals::matcher::{
    RuleCall, RuleOutcome, evaluate_standing_rules, is_protected_path, path_within,
    shell_prefix_matches,
};
use adk_harness::{
    HarnessMode, Isolation, ModeCell, ModeRecord, OriginPin, PolicySwitches, RulePredicate,
    RuleScope, StandingDecision, StandingRule, TableDecision, TableRow, ToolKind, apply_rule,
    decision_cell,
};
use chrono::Utc;
use proptest::prelude::*;
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

const CASES: u32 = 256;

/// Characters a shell gives meaning to, plus lookalikes and separators.
const META: &[&str] = &[
    ";", "&", "|", "$", "`", "<", ">", "(", ")", "{", "}", "*", "?", "[", "]", "~", "!", "\n",
    "\\", "'", "\"", "${IFS}", "\0", "\u{ff1b}", "\u{037e}", "\u{2028}", "\u{a0}", "#", "\r",
];

fn shell_fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-z0-9]{1,6}",
        2 => Just(" ".to_string()),
        1 => Just("\t".to_string()),
        1 => Just("--quiet".to_string()),
        1 => Just("A=1".to_string()),
        3 => proptest::sample::select(META).prop_map(str::to_string),
    ]
}

fn command() -> impl Strategy<Value = String> {
    (proptest::bool::ANY, proptest::collection::vec(shell_fragment(), 0..8)).prop_map(
        |(prefixed, parts)| {
            let tail: String = parts.concat();
            if prefixed { format!("cargo test{tail}") } else { tail }
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// P1: a ShellPrefix match implies no metacharacter and token-prefix equality.
    #[test]
    fn p1_shell_prefix_matches_only_plain_words(cmd in command(), width in 1usize..3) {
        let tokens: Vec<String> = ["cargo", "test", "--quiet"][..width].iter().map(|t| t.to_string()).collect();
        if shell_prefix_matches(&cmd, &tokens) {
            prop_assert!(cmd.is_ascii(), "{cmd:?}");
            for meta in META {
                prop_assert!(!cmd.contains(meta), "{cmd:?} contains {meta:?}");
            }
            let words: Vec<&str> = cmd.split([' ', '\t']).filter(|w| !w.is_empty()).collect();
            prop_assert!(!words[0].contains('='));
            let expected: Vec<&str> = tokens.iter().map(String::as_str).collect();
            prop_assert_eq!(&words[..tokens.len()], expected.as_slice());
        }
    }
}

const PATH_PARTS: &[&str] = &[
    "src",
    "a.rs",
    "..",
    ".",
    "",
    "AGENTS.md",
    "agents.md",
    "Agents.MD",
    ".git",
    ".GIT",
    "hooks",
    "config",
    "caf\u{e9}",
    "cafe\u{301}",
    "MEMORY.md",
    ".claude",
    ".skills",
    ".adk",
    ".cargo",
    "config.toml",
    "x",
    "CLAUDE.md",
];

fn oracle_protected(relative: &Path) -> bool {
    let parts: Vec<String> = relative
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => {
                Some(part.to_string_lossy().nfc().collect::<String>().to_lowercase())
            }
            _ => None,
        })
        .collect();
    parts.iter().any(|p| [".git", ".adk", ".skills", ".claude"].contains(&p.as_str()))
        || parts.last().is_some_and(|p| {
            [
                "agents.md",
                "agent.md",
                "claude.md",
                "memory.md",
                ".envrc",
                "lefthook.yml",
                "lefthook.yaml",
            ]
            .contains(&p.as_str())
        })
        || parts.windows(2).any(|w| w[0] == ".cargo" && (w[1] == "config.toml" || w[1] == "config"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// P2: a PathWithin match implies a resolvable path under the prefix that is not
    /// protected, whatever `..`, `//`, `./`, case or NFC/NFD spelling it uses.
    #[test]
    fn p2_path_within_implies_contained_and_unprotected(
        parts in proptest::collection::vec(proptest::sample::select(PATH_PARTS), 1..6),
        double in proptest::bool::ANY,
        dir in proptest::sample::select(&[".", "src", "src/.."][..]),
    ) {
        let root = PathBuf::from("/workspace/root");
        let raw = parts.join(if double { "//" } else { "/" });
        if path_within(&root, &raw, dir) {
            prop_assert!(!raw.split('/').any(|p| p == ".."), "{raw}");
            prop_assert!(dir != "src/..");
            let joined: PathBuf = root.join(&raw).components().filter(|c| !matches!(c, Component::CurDir)).collect();
            let base: PathBuf = root.join(dir).components().filter(|c| !matches!(c, Component::CurDir)).collect();
            prop_assert!(joined.starts_with(&base), "{raw} not under {dir}");
            let relative = joined.strip_prefix(&root).unwrap();
            prop_assert!(!oracle_protected(relative), "{raw} is protected");
            prop_assert!(!is_protected_path(&relative.to_string_lossy()));
        }
    }
}

fn all_switches() -> Vec<PolicySwitches> {
    let mut out = Vec::new();
    for bits in 0u8..16 {
        out.push(PolicySwitches {
            auto_allow_sandboxed: bits & 1 != 0,
            allow_host_shell_in_auto: bits & 2 != 0,
            enforcer_active: bits & 4 != 0,
            sandbox_protects_paths: bits & 8 != 0,
        });
    }
    out
}

fn severity(decision: TableDecision) -> u8 {
    match decision {
        TableDecision::Allow => 0,
        TableDecision::RequireApproval { non_waivable: false } => 1,
        TableDecision::RequireApproval { non_waivable: true } => 2,
        TableDecision::Deny => 3,
    }
}

/// P3a: the mode lattice is monotone: ReadOnly ⪰ Plan ⪰ Ask ⪰ Auto for every row and
/// switch. Two cells are by design not comparable and are left out: `exit_plan_mode` is
/// meaningful only in Plan, and Plan's sandboxed shell runs with a read-only workspace and
/// no network, a narrower capability than Ask's read-write sandbox.
#[test]
fn p3_mode_lattice_is_monotone() {
    for switches in all_switches() {
        for row in TableRow::ALL {
            if row == TableRow::PlanExit {
                continue;
            }
            for pair in HarnessMode::ALL.windows(2) {
                let (stricter, looser) = (pair[0], pair[1]);
                if row == TableRow::ShellOsSandbox && stricter == HarnessMode::Plan {
                    continue;
                }
                let s = decision_cell(stricter, row).resolve(&switches);
                let l = decision_cell(looser, row).resolve(&switches);
                assert!(
                    severity(s) >= severity(l),
                    "{row:?}: {stricter} {s:?} looser than {looser} {l:?} under {switches:?}"
                );
            }
        }
    }
    // The ReadOnly column denies everything that is not a read; a fetch of an allowlisted
    // host is a read (spec 3.1 §7.4, §7.20).
    for row in TableRow::ALL {
        let cell = decision_cell(HarnessMode::ReadOnly, row);
        let read = matches!(
            row,
            TableRow::ReadTodoDelegate
                | TableRow::ExternalReadOnlyTrusted
                | TableRow::WebAllowlisted
        );
        assert_eq!(cell == ModeCell::Allow, read, "{row:?}");
    }
}

fn table_decisions() -> impl Strategy<Value = TableDecision> {
    prop_oneof![
        Just(TableDecision::Allow),
        Just(TableDecision::Deny),
        Just(TableDecision::RequireApproval { non_waivable: false }),
        Just(TableDecision::RequireApproval { non_waivable: true }),
    ]
}

fn rule_outcomes() -> impl Strategy<Value = RuleOutcome> {
    prop_oneof![
        Just(RuleOutcome::NoMatch),
        "[a-z]{1,4}".prop_map(|id| RuleOutcome::Allow { rule_id: id }),
        "[a-z]{1,4}".prop_map(|id| RuleOutcome::Deny { rule_id: id, reason: "no".into() }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// P3b: an Allow rule never changes a Deny or a non-waivable approval; a Deny rule
    /// always denies.
    #[test]
    fn p3_allow_rules_never_loosen_deny_or_non_waivable(base in table_decisions(), rule in rule_outcomes()) {
        let decision = apply_rule(base, &rule, "reason");
        match (&rule, base) {
            (RuleOutcome::Deny { .. }, _) | (_, TableDecision::Deny) => prop_assert!(matches!(decision, PolicyDecision::Deny { .. }), "{:?}", decision),
            (_, TableDecision::RequireApproval { non_waivable: true }) => prop_assert!(matches!(decision, PolicyDecision::RequireApproval { .. }), "{:?}", decision),
            (RuleOutcome::Allow { .. }, _) => prop_assert_eq!(decision, PolicyDecision::Allow),
            (RuleOutcome::NoMatch, TableDecision::Allow) => prop_assert_eq!(decision, PolicyDecision::Allow),
            (RuleOutcome::NoMatch, _) => prop_assert!(matches!(decision, PolicyDecision::RequireApproval { .. }), "{:?}", decision),
        }
    }
}

fn bash_origin(description: &str) -> ToolOrigin {
    ToolOrigin::new("static", &json!({ "name": "bash", "description": description }), None)
}

fn rule_for(user: &str, kind: SubjectKind, origin: &ToolOrigin) -> StandingRule {
    StandingRule {
        id: "r".into(),
        scope: RuleScope::User { app_name: "app".into(), user_id: user.into(), subject_kind: kind },
        tool: "bash".into(),
        origin: OriginPin::exact(origin),
        agent: None,
        predicates: vec![RulePredicate::ShellPrefix {
            pointer: "/command".into(),
            tokens: vec!["cargo".into(), "test".into()],
        }],
        decision: StandingDecision::Allow,
        created_by: SubjectRef::new(kind, user),
        created_at: Utc::now(),
        expires_at: None,
        label: "cargo test".into(),
        source_fingerprint: None,
    }
}

fn evaluate(
    rule: &StandingRule,
    user: &str,
    acting: &ActingContext,
    origin: &ToolOrigin,
    now: chrono::DateTime<Utc>,
) -> adk_harness::approvals::matcher::RuleEvaluation {
    let args = json!({ "command": "cargo test --quiet" });
    evaluate_standing_rules(
        std::slice::from_ref(rule),
        &RuleCall {
            app_name: "app",
            user_id: user,
            session_id: "s",
            acting: Some(acting),
            tool_name: "bash",
            args: &args,
            origin: Some(origin),
            kind: ToolKind::Shell(Isolation::Host),
            workspace_root: Path::new("/w"),
        },
        now,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// P4: a rule applies only to its own subject, as the run's user and as the acting
    /// subject, of the same kind.
    #[test]
    fn p4_rules_never_cross_subjects(owner in "[a-z]{1,8}", other in "[a-z]{1,8}") {
        let origin = bash_origin("run");
        let rule = rule_for(&owner, SubjectKind::Human, &origin);
        let own = ActingContext::derived("app", "harness", &owner, SubjectKind::Human);
        prop_assert_eq!(evaluate(&rule, &owner, &own, &origin, Utc::now()).outcome, RuleOutcome::Allow { rule_id: "r".into() });
        let service = ActingContext::derived("app", "harness", &owner, SubjectKind::Service);
        prop_assert_eq!(evaluate(&rule, &owner, &service, &origin, Utc::now()).outcome, RuleOutcome::NoMatch);
        if other != owner {
            let theirs = ActingContext::derived("app", "harness", &other, SubjectKind::Human);
            prop_assert_eq!(evaluate(&rule, &other, &theirs, &origin, Utc::now()).outcome, RuleOutcome::NoMatch);
            // The run's user is the owner but the acting subject is not.
            prop_assert_eq!(evaluate(&rule, &owner, &theirs, &origin, Utc::now()).outcome, RuleOutcome::NoMatch);
        }
    }

    /// P5: any change to the declaration stops the rule matching and reports it stale.
    #[test]
    fn p5_declaration_changes_stale_the_rule(description in "[ -~]{0,40}", edit in "[ -~]{1,5}") {
        let origin = bash_origin(&description);
        let changed = bash_origin(&format!("{description}{edit}"));
        let rule = rule_for("alice", SubjectKind::Human, &origin);
        let acting = ActingContext::derived("app", "harness", "alice", SubjectKind::Human);
        let evaluation = evaluate(&rule, "alice", &acting, &changed, Utc::now());
        prop_assert_eq!(evaluation.outcome, RuleOutcome::NoMatch);
        prop_assert_eq!(evaluation.stale.len(), 1);
        let republished = ToolOrigin::new("mcp:other", &json!({ "name": "bash", "description": description }), None);
        prop_assert_eq!(evaluate(&rule, "alice", &acting, &republished, Utc::now()).outcome, RuleOutcome::NoMatch);
    }

    /// P6: a rule matches strictly before its expiry and never after.
    #[test]
    fn p6_expired_rules_never_match(offset in -1_000i64..1_000) {
        let origin = bash_origin("run");
        let now = Utc::now();
        let mut rule = rule_for("alice", SubjectKind::Human, &origin);
        rule.expires_at = Some(now + chrono::Duration::seconds(offset));
        let acting = ActingContext::derived("app", "harness", "alice", SubjectKind::Human);
        let evaluation = evaluate(&rule, "alice", &acting, &origin, now);
        if offset > 0 {
            prop_assert_eq!(evaluation.outcome, RuleOutcome::Allow { rule_id: "r".into() });
        } else {
            prop_assert_eq!(evaluation.outcome, RuleOutcome::NoMatch);
            prop_assert_eq!(evaluation.expired.len(), 1);
        }
    }
}

fn harness_agent() -> Arc<adk_harness::HarnessAgent> {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(adk_model::ScriptedLlm::new(
        "scripted",
        Vec::<adk_model::ScriptedResponse>::new(),
    ));
    let agent = adk_harness::HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(adk_harness::InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..Default::default()
        })
        .without_compaction()
        .build()
        .unwrap();
    std::mem::forget(dir);
    Arc::new(agent)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// P7: no arrangement of U+001F separators in agent ids, origins or arguments lets a
    /// run-wide approval of a non-waivable call survive `configure_run`.
    #[test]
    fn p7_separators_cannot_forge_a_non_waivable_approval(
        agent in "[a-z\u{1f}]{1,8}",
        source in "[a-z:\u{1f}]{1,8}",
        text in "[a-z \u{1f}]{0,12}",
        which in 0usize..3,
    ) {
        let harness = harness_agent();
        let (tool, args) = match which {
            0 => ("exit_plan_mode", json!({ "plan": text })),
            1 => ("bash", json!({ "command": text, "unsandboxed": true })),
            _ => ("write_file", json!({ "path": format!("{text}/AGENTS.md"), "content": text })),
        };
        let request = ToolConfirmationRequest::new(tool, args)
            .with_acting_context(ActingContext::derived("app", &agent, "alice", SubjectKind::Human))
            .with_origin(ToolOrigin::new(source, &json!({ "name": tool }), None));
        let mut config = RunConfig::default();
        config.tool_approvals.insert(request.fingerprint(), ToolApproval::approve());
        adk_core::Agent::configure_run(harness.as_ref(), "harness", &mut config);
        prop_assert!(config.tool_approvals.is_empty(), "{:?}", request.fingerprint());
    }
}

/// JSON with the field names of the stored records, to make near misses likely.
fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        proptest::bool::ANY.prop_map(Value::Bool),
        (-5i64..5).prop_map(|n| json!(n)),
        proptest::sample::select(
            &[
                "auto",
                "ask",
                "plan",
                "read_only",
                "allow",
                "deny",
                "user",
                "exact",
                "human",
                "bash",
                "x"
            ][..]
        )
        .prop_map(|s| json!(s)),
    ];
    leaf.prop_recursive(3, 24, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::btree_map(
                proptest::sample::select(
                    &[
                        "mode",
                        "revision",
                        "setBy",
                        "at",
                        "kind",
                        "decision",
                        "scope",
                        "tool",
                        "origin",
                        "id",
                        "label",
                        "predicates",
                        "createdBy",
                        "createdAt",
                        "extra"
                    ][..]
                ),
                inner,
                0..6,
            )
            .prop_map(|map| Value::Object(
                map.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
            )),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// P8: arbitrary JSON reads as ReadOnly or as an ignored rule, never as Auto or Allow.
    #[test]
    fn p8_garbage_reads_as_read_only_or_is_ignored(value in json_value()) {
        let record = ModeRecord::from_json_lenient(&value);
        if record.mode != HarnessMode::ReadOnly {
            let strict: ModeRecord = serde_json::from_value(value.clone()).expect("only a valid record reads as another mode");
            prop_assert_eq!(strict.mode, record.mode);
        }
        prop_assert!(serde_json::from_value::<StandingRule>(value.clone()).is_err());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = adk_harness::FileStandingRuleStore::new(dir.path());
        let probe = rule_for("alice", SubjectKind::Human, &bash_origin("x"));
        runtime.block_on(async {
            use adk_harness::StandingRuleStore;
            store.grant(probe).await.unwrap();
            let file = std::fs::read_dir(dir.path().join("app")).unwrap().next().unwrap().unwrap().path();
            let mut entries: Vec<Value> = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            entries[0] = value.clone();
            entries.push(value);
            std::fs::write(&file, serde_json::to_vec(&entries).unwrap()).unwrap();
            assert!(store.rules_for("app", "alice", "s").await.unwrap().is_empty());
        });
    }
}
