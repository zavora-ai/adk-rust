//! A skill written at runtime must become usable without restarting the process.
//!
//! Before `SkillWriter`, this crate was read-only: every `fs::write` lived behind `#[cfg(test)]`,
//! so an agent could not persist a skill it derived from experience, and `SkillInjector` held an
//! index snapshotted at construction with no way to rescan. Together those two gaps meant a
//! self-improving agent had nowhere to put what it learned.

use adk_core::Content;
use adk_skill::{
    SelectionPolicy, SkillDraft, SkillInjector, SkillInjectorConfig, SkillWriter,
    apply_skill_injection, load_skill_index,
};

/// A permissive policy, so these tests assert discovery and reload rather than ranking.
fn policy() -> SelectionPolicy {
    SelectionPolicy { top_k: 1, min_score: 0.1, ..SelectionPolicy::default() }
}

fn config() -> SkillInjectorConfig {
    SkillInjectorConfig { policy: policy(), ..SkillInjectorConfig::default() }
}

#[test]
fn a_skill_written_at_runtime_is_invisible_until_reload() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = SkillWriter::new(root.path());

    // An injector built before the skill exists.
    let injector = SkillInjector::from_root(root.path(), config()).expect("injector builds");
    assert!(injector.index().is_empty(), "nothing has been written yet");

    writer
        .write(
            &SkillDraft::new("disk-triage", "Diagnose low disk space on a full volume")
                .with_body("Check the largest directories, then report the growth rate."),
        )
        .expect("writes");

    assert!(
        injector.index().is_empty(),
        "the existing injector holds a snapshot, so it must not change underneath the caller"
    );

    let reloaded = injector.reloaded().expect("reloads");
    assert_eq!(reloaded.index().len(), 1, "the reloaded injector sees the new skill");
    assert!(reloaded.index().find_by_name("disk-triage").is_some());
}

#[test]
fn a_reloaded_injector_injects_the_new_skill() {
    let root = tempfile::tempdir().expect("tempdir");
    SkillWriter::new(root.path())
        .write(
            &SkillDraft::new("disk-triage", "Diagnose low disk space on a full volume")
                .with_body("Check the largest directories first."),
        )
        .expect("writes");

    let injector = SkillInjector::from_root(root.path(), config()).expect("injector builds");
    let mut content = Content::new("user").with_text("the disk is full, help me diagnose space");

    let matched = apply_skill_injection(
        &mut content,
        injector.index(),
        injector.policy(),
        injector.max_injected_chars(),
    );

    assert!(matched.is_some(), "a written skill should be selectable");
    let injected = content.parts[0].text().expect("text part");
    assert!(injected.contains("[skill:disk-triage]"), "got {injected}");
    assert!(injected.contains("Check the largest directories first."), "got {injected}");
}

#[test]
fn reload_picks_up_a_revision_of_an_existing_skill() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = SkillWriter::new(root.path());

    writer
        .write(&SkillDraft::new("sweep", "First revision").with_body("Do it the old way."))
        .expect("first write");
    let injector = SkillInjector::from_root(root.path(), config()).expect("injector builds");
    let first_hash = injector.index().find_by_name("sweep").expect("present").hash.clone();

    writer
        .write(&SkillDraft::new("sweep", "Second revision").with_body("Do it the new way."))
        .expect("second write");
    let reloaded = injector.reloaded().expect("reloads");
    let second = reloaded.index().find_by_name("sweep").expect("present");

    assert_eq!(reloaded.index().len(), 1, "a revision replaces rather than duplicates");
    assert_eq!(second.description, "Second revision");
    assert_ne!(second.hash, first_hash, "the content hash must track the revision");
}

#[test]
fn reload_drops_a_removed_skill() {
    let root = tempfile::tempdir().expect("tempdir");
    let writer = SkillWriter::new(root.path());
    writer
        .write(&SkillDraft::new("retired", "Superseded approach").with_body("Old steps."))
        .expect("writes");

    let injector = SkillInjector::from_root(root.path(), config()).expect("injector builds");
    assert_eq!(injector.index().len(), 1);

    assert!(writer.remove("retired").expect("removes"));

    assert_eq!(
        injector.reloaded().expect("reloads").index().len(),
        0,
        "retiring a skill must take effect on reload"
    );
}

#[test]
fn reload_is_refused_when_there_is_no_root_to_rescan() {
    let root = tempfile::tempdir().expect("tempdir");
    let index = load_skill_index(root.path()).expect("index loads");
    let injector = SkillInjector::from_index(index, config());

    assert!(injector.root().is_none());
    let error = injector.reloaded().expect_err("an injector with no root cannot rescan");
    assert!(
        error.to_string().contains("from_index"),
        "the error should name the constructor that caused it: {error}"
    );
}

#[test]
fn provenance_metadata_survives_a_write_and_reload() {
    let root = tempfile::tempdir().expect("tempdir");

    // A learned skill should record where it came from, so a promotion is auditable and
    // revertible rather than an unexplained change in behaviour.
    SkillWriter::new(root.path())
        .write(
            &SkillDraft::new("thermal-triage", "Diagnose thermal throttling")
                .with_body("Correlate throttle minutes with active processes.")
                .with_metadata_entry("incidents", serde_json::json!(["INC-1", "INC-2", "INC-3"]))
                .with_metadata_entry("promoted_at", serde_json::json!("2026-08-22T13:45:00Z")),
        )
        .expect("writes");

    let skill = load_skill_index(root.path())
        .expect("index loads")
        .find_by_name("thermal-triage")
        .cloned()
        .expect("present");

    assert_eq!(
        skill.metadata.get("incidents"),
        Some(&serde_json::json!(["INC-1", "INC-2", "INC-3"])),
        "the evidence behind a promotion must survive the round trip"
    );
    assert_eq!(skill.metadata.get("promoted_at"), Some(&serde_json::json!("2026-08-22T13:45:00Z")));
}
