//! JUnit XML report generation for CI system integration.
//!
//! Generates valid JUnit XML from evaluation reports, enabling CI systems
//! (GitHub Actions, Jenkins, GitLab CI) to display test results natively.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_eval::{JunitReporter, EvaluationReport};
//!
//! let report: EvaluationReport = /* run evaluation */;
//! let xml = JunitReporter::generate(&report, "my_eval_suite")?;
//! println!("{xml}");
//! ```

use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

use crate::error::Result;
use crate::report::EvaluationReport;

/// Generates JUnit XML output from evaluation reports.
///
/// Maps each [`EvalCase`](crate::schema::EvalCase) to a `<testcase>` element
/// and failures to `<failure>` child elements within a `<testsuite>` wrapper.
pub struct JunitReporter;

impl JunitReporter {
    /// Generate JUnit XML string from an evaluation report.
    ///
    /// Produces a valid XML document conforming to the JUnit XML schema with
    /// `<testsuite>` and `<testcase>` elements.
    ///
    /// # Arguments
    ///
    /// * `report` - The evaluation report to convert
    /// * `suite_name` - Name for the `<testsuite>` element
    ///
    /// # Returns
    ///
    /// A JUnit XML string suitable for CI system consumption.
    pub fn generate(report: &EvaluationReport, suite_name: &str) -> Result<String> {
        let mut writer = Writer::new(Vec::new());

        // XML declaration
        writer
            .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
            .map_err(|e| crate::error::EvalError::IoError(std::io::Error::other(e.to_string())))?;

        let total_cases = report.results.len();
        let failures = report.results.iter().filter(|r| !r.passed).count();
        let total_time = report.duration.as_secs_f64();

        // <testsuite>
        let mut testsuite = BytesStart::new("testsuite");
        testsuite.push_attribute(("name", suite_name));
        testsuite.push_attribute(("tests", total_cases.to_string().as_str()));
        testsuite.push_attribute(("failures", failures.to_string().as_str()));
        testsuite.push_attribute(("errors", "0"));
        testsuite.push_attribute(("time", format!("{total_time:.3}").as_str()));

        writer
            .write_event(Event::Start(testsuite))
            .map_err(|e| crate::error::EvalError::IoError(std::io::Error::other(e.to_string())))?;

        // Each test case
        for result in &report.results {
            let case_time = result.duration.as_secs_f64();

            let mut testcase = BytesStart::new("testcase");
            testcase.push_attribute(("name", result.eval_id.as_str()));
            testcase.push_attribute(("classname", suite_name));
            testcase.push_attribute(("time", format!("{case_time:.3}").as_str()));

            if result.failures.is_empty() {
                // Self-closing testcase
                writer.write_event(Event::Empty(testcase)).map_err(|e| {
                    crate::error::EvalError::IoError(std::io::Error::other(e.to_string()))
                })?;
            } else {
                writer.write_event(Event::Start(testcase.clone())).map_err(|e| {
                    crate::error::EvalError::IoError(std::io::Error::other(e.to_string()))
                })?;

                for failure in &result.failures {
                    let mut failure_elem = BytesStart::new("failure");
                    failure_elem.push_attribute(("type", failure.criterion.as_str()));

                    let details = failure.details.as_deref().unwrap_or("Score below threshold");

                    let failure_text = format!(
                        "Criterion '{}': score {:.3} < threshold {:.3}. {}",
                        failure.criterion, failure.score, failure.threshold, details
                    );

                    writer.write_event(Event::Start(failure_elem)).map_err(|e| {
                        crate::error::EvalError::IoError(std::io::Error::other(e.to_string()))
                    })?;
                    writer.write_event(Event::Text(BytesText::new(&failure_text))).map_err(
                        |e| crate::error::EvalError::IoError(std::io::Error::other(e.to_string())),
                    )?;
                    writer.write_event(Event::End(BytesEnd::new("failure"))).map_err(|e| {
                        crate::error::EvalError::IoError(std::io::Error::other(e.to_string()))
                    })?;
                }

                writer.write_event(Event::End(BytesEnd::new("testcase"))).map_err(|e| {
                    crate::error::EvalError::IoError(std::io::Error::other(e.to_string()))
                })?;
            }
        }

        // </testsuite>
        writer
            .write_event(Event::End(BytesEnd::new("testsuite")))
            .map_err(|e| crate::error::EvalError::IoError(std::io::Error::other(e.to_string())))?;

        let xml_bytes = writer.into_inner();
        String::from_utf8(xml_bytes)
            .map_err(|e| crate::error::EvalError::IoError(std::io::Error::other(e.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{EvaluationReport, EvaluationResult, Failure};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::time::Duration;

    fn make_report(results: Vec<EvaluationResult>) -> EvaluationReport {
        let started_at = chrono::Utc::now();
        EvaluationReport::new("test-run", results, started_at)
    }

    #[test]
    fn test_generate_empty_report() {
        let report = make_report(vec![]);
        let xml = JunitReporter::generate(&report, "empty_suite").unwrap();
        assert!(xml.contains("<testsuite"));
        assert!(xml.contains("tests=\"0\""));
        assert!(xml.contains("failures=\"0\""));
    }

    #[test]
    fn test_generate_with_passing_cases() {
        let results = vec![
            EvaluationResult::passed(
                "case_1",
                HashMap::from([("quality".to_string(), 0.9)]),
                Duration::from_millis(100),
            ),
            EvaluationResult::passed(
                "case_2",
                HashMap::from([("quality".to_string(), 0.85)]),
                Duration::from_millis(150),
            ),
        ];
        let report = make_report(results);
        let xml = JunitReporter::generate(&report, "pass_suite").unwrap();
        assert!(xml.contains("tests=\"2\""));
        assert!(xml.contains("failures=\"0\""));
        assert!(xml.contains("name=\"case_1\""));
        assert!(xml.contains("name=\"case_2\""));
    }

    #[test]
    fn test_generate_with_failures() {
        let results = vec![EvaluationResult::failed(
            "case_fail",
            HashMap::from([("accuracy".to_string(), 0.3)]),
            vec![Failure::new("accuracy", Value::Null, Value::Null, 0.3, 0.8)],
            Duration::from_millis(200),
        )];
        let report = make_report(results);
        let xml = JunitReporter::generate(&report, "fail_suite").unwrap();
        assert!(xml.contains("tests=\"1\""));
        assert!(xml.contains("failures=\"1\""));
        assert!(xml.contains("<failure"));
        assert!(xml.contains("type=\"accuracy\""));
    }
}
