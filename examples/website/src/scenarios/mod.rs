//! The scenarios of spec 3.5 §5, one module each.

use std::future::Future;
use std::pin::Pin;

use crate::checks::Checks;
use crate::live::Mode;

pub mod s01_from_nothing;
pub mod s02_presence;
pub mod s03_migrate;
pub mod s04_freeze;
pub mod s05_tamper;
pub mod s06_identity;
pub mod s07_injection;
pub mod s08_lead;
pub mod s09_agent_business;
pub mod s10_week;
pub mod s11_experiment;
pub mod s12_money;
pub mod s13_verify;
pub mod s14_autonomy;
pub mod s15_agency;
pub mod s16_government;
pub mod s17_personal;
pub mod s18_publishing;
pub mod s19_constant;

/// A scenario's run.
pub type Run = fn(Mode) -> Pin<Box<dyn Future<Output = anyhow::Result<Checks>> + Send>>;

/// One scenario.
pub struct Scenario {
    /// `S1` to `S19`.
    pub id: &'static str,
    /// Its capability, as §5 names it.
    pub title: &'static str,
    /// Whether an agent's turn is in it, so `--live` runs it on a real model.
    pub agent: bool,
    /// Checks that fail on `main` because of a runtime gap the run reports: while only these
    /// fail, the scenario is reported as `GAP`, not `FAIL`.
    pub known: &'static [&'static str],
    /// The run.
    pub run: Run,
}

macro_rules! scenario {
    ($id:literal, $title:literal, agent: $agent:literal, $module:ident) => {
        scenario!($id, $title, agent: $agent, $module, known: [])
    };
    ($id:literal, $title:literal, agent: $agent:literal, $module:ident, known: [$($known:literal),*]) => {
        Scenario {
            id: $id,
            title: $title,
            agent: $agent,
            known: &[$($known),*],
            run: |mode| Box::pin($module::run(mode)),
        }
    };
}

/// Every scenario, in §5 order.
pub fn all() -> Vec<Scenario> {
    vec![
        scenario!("S1", "From nothing to live", agent: true, s01_from_nothing),
        scenario!("S2", "From an online presence", agent: true, s02_presence),
        scenario!("S3", "Migrate and leave the infection behind", agent: false, s03_migrate),
        scenario!("S4", "Freeze in hours", agent: false, s04_freeze),
        scenario!("S5", "Tamper, detect, reverse", agent: true, s05_tamper),
        scenario!("S6", "Identity, not claims", agent: false, s06_identity),
        scenario!("S7", "Injection reaches nothing privileged", agent: true, s07_injection),
        scenario!("S8", "Lead to warm handoff", agent: true, s08_lead, known: [
            "the nurture emails respect quiet hours",
            "the lead's score crosses the charter threshold",
            "the owner gets one handoff card with the conversation summary on their chosen channel"
        ]),
        scenario!("S9", "Agent-to-agent business", agent: false, s09_agent_business),
        scenario!("S10", "A week on duty", agent: true, s10_week, known: [
            "upgrade and renewal proposals"
        ]),
        scenario!("S11", "An experiment that respects its guardrails", agent: false, s11_experiment),
        scenario!("S12", "Money inside the box", agent: true, s12_money),
        scenario!("S13", "Anyone can check", agent: false, s13_verify),
        scenario!("S14", "Earned autonomy", agent: true, s14_autonomy),
        scenario!("S15", "Agency mode", agent: false, s15_agency),
        scenario!("S16", "A public body", agent: false, s16_government),
        scenario!("S17", "A personal site", agent: false, s17_personal),
        scenario!("S18", "Publishing at speed", agent: true, s18_publishing),
        scenario!("S19", "A site that never stops changing", agent: false, s19_constant),
    ]
}
