//! Verdicts and the summary table.

/// Whether a scenario talks to a live provider or to a scripted local upstream.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    Scripted,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Scripted => "scripted",
        }
    }
}

pub enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

pub fn pass(detail: impl Into<String>) -> Verdict {
    Verdict::Pass(detail.into())
}

pub fn fail(detail: impl Into<String>) -> Verdict {
    Verdict::Fail(detail.into())
}

pub fn skip(detail: impl Into<String>) -> Verdict {
    Verdict::Skip(detail.into())
}

pub fn check(ok: bool, detail: impl Into<String>) -> Verdict {
    if ok { pass(detail) } else { fail(detail) }
}

struct Outcome {
    id: &'static str,
    item: u8,
    mode: Mode,
    verdict: Verdict,
}

/// Collects outcomes and prints each as it completes.
#[derive(Default)]
pub struct Report {
    outcomes: Vec<Outcome>,
}

impl Report {
    pub fn record(&mut self, id: &'static str, item: u8, mode: Mode, verdict: Verdict) {
        let (label, detail) = label(&verdict);
        println!("{label} {id} [{}] {}", mode.label(), short(detail, 240));
        self.outcomes.push(Outcome { id, item, mode, verdict });
    }

    /// Records the scenario, or a failure when it could not be set up.
    pub fn record_result(
        &mut self,
        id: &'static str,
        item: u8,
        mode: Mode,
        result: anyhow::Result<Verdict>,
    ) {
        let verdict = result.unwrap_or_else(|error| fail(format!("{error:#}")));
        self.record(id, item, mode, verdict);
    }

    /// Prints the summary table and returns the number of failures.
    pub fn summarize(&self) -> usize {
        println!();
        println!("| Scenario | Item | Mode | Result | Detail |");
        println!("|---|---|---|---|---|");
        let mut failures = 0;
        for outcome in &self.outcomes {
            let (result, detail) = label(&outcome.verdict);
            if matches!(outcome.verdict, Verdict::Fail(_)) {
                failures += 1;
            }
            println!(
                "| `{}` | {} | {} | {result} | {} |",
                outcome.id,
                outcome.item,
                outcome.mode.label(),
                short(detail, 160).replace('|', "/")
            );
        }
        println!();
        println!("{} scenarios, {failures} failed", self.outcomes.len());
        failures
    }
}

fn label(verdict: &Verdict) -> (&'static str, &str) {
    match verdict {
        Verdict::Pass(detail) => ("PASS", detail),
        Verdict::Fail(detail) => ("FAIL", detail),
        Verdict::Skip(detail) => ("SKIP", detail),
    }
}

/// Cuts `text` to at most `max` characters for a one-line report.
pub fn short(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

/// Reads a non-blank environment variable.
pub fn key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}
