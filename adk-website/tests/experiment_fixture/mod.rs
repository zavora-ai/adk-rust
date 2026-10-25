//! A site set up for experiments (spec 3.5 §7.12, S11): the business template with the KPIs
//! an experiment measures and the template's unsubscribe guardrail extended to the
//! experiments job.

#![allow(dead_code)]

use std::sync::Mutex;
use std::time::Duration;

use adk_kernel::charter::{CharterEvent, CharterEventSink};
use adk_website::kinds::CharterInputs;
use awp_types::{Cadence, Charter, Direction, Kpi, Measurement, SourceRef};
use chrono::{TimeZone, Utc};

use crate::common::{OWNER, Site};

/// Collects charter events.
#[derive(Default)]
pub struct Events(pub Mutex<Vec<CharterEvent>>);

#[async_trait::async_trait]
impl CharterEventSink for Events {
    async fn emit(&self, event: &CharterEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

/// The business template with the KPIs the experiment measures: sign-ups (4% baseline) and
/// the unsubscribe rate (1.5% baseline), and the template's unsubscribe guardrail (above 2%)
/// extended to the experiments job.
pub fn charter(site: &Site) -> Charter {
    let inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        OWNER,
        "Fresh bread for Nakuru every morning.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    let mut charter = site.template.draft_charter(&site.team, &inputs);
    let baseline = |value: f64| Measurement {
        value,
        window: Duration::from_secs(14 * 86_400),
        measured_at: Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
    };
    for (id, name, metric, source, direction, value) in [
        (
            "signups",
            "Newsletter sign-ups",
            "newsletter.signups",
            SourceRef::Beacon,
            Direction::Up,
            0.04,
        ),
        (
            "unsubscribe_rate",
            "Unsubscribe rate",
            "email.unsubscribe_rate",
            SourceRef::Email,
            Direction::Down,
            0.015,
        ),
    ] {
        charter.kpis.push(Kpi {
            id: id.into(),
            name: name.into(),
            metric: metric.into(),
            source,
            direction,
            baseline: Some(baseline(value)),
            target: None,
            cadence: Cadence::Daily,
        });
    }
    let unsubscribes =
        charter.guardrails.iter_mut().find(|g| g.id == "unsubscribes").expect("template guardrail");
    unsubscribes.jobs.push("experiments".into());
    charter.validate().unwrap();
    charter
}

/// The first `<h1>` of `html`, the headline the variant replaces.
pub fn headline(html: &str) -> String {
    let start = html.find("<h1>").expect("a headline");
    let end = start + html[start..].find("</h1>").unwrap() + "</h1>".len();
    html[start..end].to_string()
}
