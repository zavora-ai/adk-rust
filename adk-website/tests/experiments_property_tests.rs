//! Property tests of the experiment runtime and the host-log parser (spec 3.5 §7.12, §11):
//! assignment is stable and splits by its allocation, every whole day is counted exactly
//! once whatever the look schedule, builds add and remove an experiment cleanly, and parsed
//! log lines keep no query.

use std::sync::Arc;

use adk_core::ManualClock;
use adk_core::principal::SubjectRef;
use adk_harness::website::SiteRef;
use adk_kernel::charter::{CharterStore, CommitDecision, InMemoryCharterStore, RoleApproval};
use adk_server::awp::{BeaconCounts, BeaconStore, InMemoryBeaconStore};
use adk_site::content::{SiteConfig, SiteProject};
use adk_site::generator::{BuiltinGenerator, ChangeSet, SiteGenerator};
use adk_website::experiments::{
    Arm, ExperimentGenerator, ExperimentPlan, ExperimentRunner, ExperimentStatus, ExperimentStore,
    InMemoryExperimentStore, assign, bucket_hash,
};
use adk_website::kinds::{CharterInputs, template};
use adk_website::measurement::{BeaconKpi, HostLogParser};
use adk_website::team::SiteTeam;
use awp_types::{Cadence, Charter, Direction, Kpi, Role, SiteKind, SourceRef};
use chrono::{NaiveDate, TimeZone, Utc};
use proptest::prelude::*;
use uuid::Uuid;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

/// A bucket as the client draws it: two random 32-bit words in hex.
fn bucket(seed: u64, index: u64) -> String {
    let mut z = seed.wrapping_add(index.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    format!("{:x}{:x}", z >> 32, z & 0xffff_ffff)
}

/// A business charter whose `signups` KPI an experiment can move.
async fn committed(site: Uuid) -> Arc<InMemoryCharterStore> {
    let kind = template(SiteKind::Business);
    let team = SiteTeam::new(SiteRef::new(site, "juniper", "https://juniper.example"), &kind);
    let inputs =
        CharterInputs::new(site, "Juniper", "amina", "Bread.", vec!["en".parse().unwrap()], "KES");
    let mut charter: Charter = kind.draft_charter(&team, &inputs);
    charter.kpis.push(Kpi {
        id: "signups".into(),
        name: "Sign-ups".into(),
        metric: "newsletter.signups".into(),
        source: SourceRef::Beacon,
        direction: Direction::Up,
        baseline: None,
        target: None,
        cadence: Cadence::Daily,
    });
    let charters = Arc::new(InMemoryCharterStore::new());
    let decision = CommitDecision::new("dec-1", Utc::now()).with_approval(RoleApproval::new(
        Role::Owner,
        SubjectRef::human("amina"),
        Utc::now(),
    ));
    charters.commit(charter, decision).await.unwrap();
    charters
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// A bucket keeps its arm, and the variant's share of buckets is its allocation.
    #[test]
    fn assignment_is_stable_and_splits_by_its_allocation(
        id in "[a-z0-9-]{1,24}",
        allocation in 0.05f64..0.95,
        seed in any::<u64>(),
    ) {
        let n = 4_000;
        let mut variant = 0;
        for index in 0..n {
            let bucket = bucket(seed, index);
            let arm = assign(&id, &bucket, allocation);
            prop_assert_eq!(arm, assign(&id, &bucket, allocation));
            if arm == Arm::Variant {
                variant += 1;
            }
        }
        // Four and a half standard deviations of a binomial share over 4,000 buckets.
        let share = f64::from(variant) / n as f64;
        prop_assert!((share - allocation).abs() < 0.036, "{share} against {allocation}");
    }

    /// On ASCII the hash is the reference FNV-1a over bytes.
    #[test]
    fn the_bucket_hash_is_fnv_1a(text in "[ -~]{0,64}") {
        let reference = text.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
        prop_assert_eq!(bucket_hash(&text), reference);
    }

    /// Whatever days the looks fall on, every whole day before the last look is counted
    /// once: no gap, no duplicate.
    #[test]
    fn every_whole_day_is_counted_exactly_once(
        days in proptest::collection::vec((0u64..400, 0u64..40), 1..12),
        looks in proptest::collection::vec(any::<bool>(), 12),
    ) {
        runtime().block_on(async {
            let site = Uuid::new_v4();
            let charters = committed(site).await;
            let counts = Arc::new(InMemoryBeaconStore::new());
            let start = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
            let clock = ManualClock::new(Utc.from_utc_datetime(&start.and_hms_opt(9, 0, 0).unwrap()));
            let runner = ExperimentRunner::new(site, charters, Arc::new(InMemoryExperimentStore::new()))
                .with_source(Arc::new(BeaconKpi::new(site, BeaconCounts::new(counts.clone()))))
                .with_clock(Arc::new(clock.clone()));
            runner
                .start(ExperimentPlan::new("headline", "/", "signups").replace("<h1>", "<h1 class=\"b\">"))
                .await
                .unwrap();
            let mut expected = 0;
            let mut seen = 0;
            for (index, (exposures, signups)) in days.iter().enumerate() {
                let date = start + chrono::Duration::days(index as i64);
                // Both arms alike, so no look ever decides and stops the counting.
                for arm in ["a", "b"] {
                    counts.add(date, &format!("events:exp.headline.{arm}.exposure"), *exposures).await.unwrap();
                    counts
                        .add(date, &format!("events:exp.headline.{arm}.newsletter.signups"), (*signups).min(*exposures))
                        .await
                        .unwrap();
                }
                let last = index + 1 == days.len();
                if looks[index] || last {
                    clock.set(Utc.from_utc_datetime(&(date + chrono::Duration::days(1)).and_hms_opt(4, 0, 0).unwrap()));
                    expected = days[..=index].iter().map(|(e, _)| e).sum::<u64>();
                    let look = runner.look("headline").await.unwrap().expect("a new whole day");
                    prop_assert!(!look.stopped);
                    prop_assert_eq!(look.record.counts.exposures.control, expected);
                    prop_assert_eq!(look.record.counts.exposures.variant, expected);
                    seen += 1;
                    // A second look at the same moment adds nothing.
                    prop_assert!(runner.look("headline").await.unwrap().is_none());
                }
            }
            let record = runner.get("headline").await.unwrap().unwrap();
            prop_assert_eq!(record.counts.exposures.control, expected);
            prop_assert_eq!(record.latest.unwrap().look, seen);
            Ok::<(), TestCaseError>(())
        })?;
    }

    /// Building with an experiment running and then stopped, in any order of full and
    /// incremental builds, leaves one tag per page while it runs and the plain build after.
    #[test]
    fn builds_add_and_remove_an_experiment_cleanly(
        steps in proptest::collection::vec((any::<bool>(), any::<bool>()), 1..8),
    ) {
        runtime().block_on(async {
            let site = Uuid::new_v4();
            let config = SiteConfig::new("Juniper", "juniper.example", vec!["en".parse().unwrap()]);
            let project = SiteProject::new(config, Vec::new(), Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap());
            let plain_dir = tempfile::tempdir().unwrap();
            BuiltinGenerator.build(&project, plain_dir.path()).await.unwrap();
            let plain = std::fs::read_to_string(plain_dir.path().join("index.html")).unwrap();
            let store = Arc::new(InMemoryExperimentStore::new());
            let charters = committed(site).await;
            let runner = ExperimentRunner::new(site, charters, store.clone());
            let start = plain.find("<h1>").unwrap();
            let headline = &plain[start..start + plain[start..].find("</h1>").unwrap() + 5];
            let mut record = runner
                .start(ExperimentPlan::new("headline", "/", "signups").replace(headline, "<h1>5am</h1>"))
                .await
                .unwrap();
            let generator = ExperimentGenerator::new(Arc::new(BuiltinGenerator), store.clone(), site, "https://edge.example/awp/v2/beacon");
            let out = tempfile::tempdir().unwrap();
            let mut graph = generator.build(&project, out.path()).await.unwrap().graph;
            for (running, full) in steps {
                record.status = if running {
                    ExperimentStatus::Running
                } else {
                    ExperimentStatus::Stopped {
                        decision: adk_eval::experiments::Decision::AdoptVariant,
                        at: Utc::now(),
                        paused_job: None,
                    }
                };
                store.put(&record).await.unwrap();
                let report = if full {
                    generator.build(&project, out.path()).await.unwrap()
                } else {
                    let changes = ChangeSet { items: Default::default(), previous: graph.clone() };
                    generator.build_incremental(&project, out.path(), &changes).await.unwrap()
                };
                graph = report.graph.clone();
                let home = std::fs::read_to_string(out.path().join("index.html")).unwrap();
                let variant = out.path().join("_exp/headline/index.html");
                let script = out.path().join("_awp/experiments.js");
                if running {
                    prop_assert_eq!(home.matches("data-awp-experiment=").count(), 1);
                    prop_assert!(variant.exists() && script.exists());
                    prop_assert!(report.outputs.contains("_exp/headline/index.html"));
                } else {
                    prop_assert_eq!(&home, &plain);
                    prop_assert!(!variant.exists() && !script.exists());
                    prop_assert!(!report.outputs.contains("_exp/headline/index.html"));
                }
            }
            Ok::<(), TestCaseError>(())
        })?;
    }

    /// The parser never panics, and a parsed path carries no query or fragment.
    #[test]
    fn parsed_log_lines_keep_no_query(
        noise in "\\PC{0,160}",
        path in "/[a-z0-9/._-]{0,30}",
        query in "[?#][a-z0-9=&]{0,20}",
        status in 100u16..600,
    ) {
        let mut parser = HostLogParser::new();
        let _ = parser.parse_line(&noise);
        let line = format!(
            r#"192.0.2.1 - - [24/Oct/2026:09:15:02 +0000] "GET {path}{query} HTTP/1.1" {status} 10 "-" "agent""#
        );
        let parsed = parser.parse_line(&line).expect("a combined line parses");
        prop_assert_eq!(parsed.path, path);
        prop_assert_eq!(parsed.status, status);
    }
}
