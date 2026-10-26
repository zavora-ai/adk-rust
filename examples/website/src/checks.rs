//! A scenario's outcome: named checks, each passed or failed with its evidence.

use std::fmt::Display;

/// One pass condition, checked.
#[derive(Debug, Clone)]
pub struct Check {
    /// What was checked.
    pub name: String,
    /// Whether it held.
    pub passed: bool,
    /// What was seen.
    pub evidence: String,
}

/// The checks of one scenario run.
#[derive(Debug, Default)]
pub struct Checks {
    results: Vec<Check>,
    gaps: Vec<String>,
}

impl Checks {
    /// Records `name` as passed or failed with `evidence`, prints it, and returns `passed`.
    pub fn check(&mut self, name: &str, passed: bool, evidence: impl Display) -> bool {
        let evidence = evidence.to_string();
        println!("    {} {name}: {evidence}", if passed { "ok  " } else { "FAIL" });
        self.results.push(Check { name: name.to_string(), passed, evidence });
        passed
    }

    /// Records `name`, and stops the scenario when it failed: the steps after it depend on it.
    ///
    /// # Errors
    ///
    /// When the check failed.
    pub fn require(
        &mut self,
        name: &str,
        passed: bool,
        evidence: impl Display,
    ) -> anyhow::Result<()> {
        if self.check(name, passed, evidence) {
            Ok(())
        } else {
            anyhow::bail!("required check failed: {name}")
        }
    }

    /// Records a runtime gap the scenario worked around or could not check: reported to the
    /// coordinator, not a pass condition.
    pub fn gap(&mut self, text: impl Display) {
        let text = text.to_string();
        println!("    gap  {text}");
        self.gaps.push(text);
    }

    /// The gaps recorded.
    pub fn gaps(&self) -> &[String] {
        &self.gaps
    }

    /// Whether every check passed, and at least one ran.
    pub fn passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|check| check.passed)
    }

    /// The failed checks' names.
    pub fn failures(&self) -> Vec<String> {
        self.results.iter().filter(|check| !check.passed).map(|check| check.name.clone()).collect()
    }

    /// `passed/total checks`.
    pub fn tally(&self) -> String {
        let passed = self.results.iter().filter(|check| check.passed).count();
        format!("{passed}/{} checks", self.results.len())
    }

    /// Every check, in order.
    pub fn all(&self) -> &[Check] {
        &self.results
    }
}
