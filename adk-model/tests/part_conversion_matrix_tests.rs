//! Every content part must be accounted for on the way to a provider.
//!
//! `Content` can express more than any single provider transport accepts, so adapters have
//! to drop or downgrade some parts. The decisions are legitimate; making them invisible is
//! not. The Bedrock converter previously carried a literal `// Unsupported MIME type — skip
//! silently` and returned `None` at five sites, so a request could reach the model without
//! material the caller supplied — and the model could answer as though it had seen a
//! document it never received.
//!
//! This matrix walks every `Part` variant and asserts each one is either carried or
//! recorded. A part that vanishes with no outcome fails the test.

#![cfg(feature = "bedrock")]

use adk_core::{Content, Part};
use adk_model::part_conversion::{ConversionReport, PartDisposition};

/// The parts a caller can supply, one per shape the matrix must cover.
fn every_part_shape() -> Vec<(&'static str, Part)> {
    vec![
        ("text", Part::Text { text: "hello".to_string() }),
        ("inline image (supported)", Part::inline_data("image/png", vec![1, 2, 3])),
        ("inline pdf (supported document)", Part::inline_data("application/pdf", vec![4, 5])),
        ("inline audio (unsupported)", Part::inline_data("audio/wav", vec![6, 7])),
        ("inline video (unsupported)", Part::inline_data("video/mp4", vec![8, 9])),
        ("inline arbitrary binary", Part::inline_data("application/octet-stream", vec![0])),
        ("file image reference", Part::file_data("image/png", "https://example.test/a.png")),
        ("file pdf reference", Part::file_data("application/pdf", "https://example.test/a.pdf")),
        ("file audio reference", Part::file_data("audio/wav", "https://example.test/a.wav")),
    ]
}

/// Converts one part through the Bedrock adapter and returns its recorded outcomes.
///
/// Uses the public request builder, so this exercises the same path a live call takes.
fn bedrock_outcomes(part: Part) -> ConversionReport {
    let content = Content { role: "user".to_string(), parts: vec![part] };
    adk_model::bedrock::convert::report_for_contents(std::slice::from_ref(&content))
}

#[test]
fn no_part_reaches_bedrock_unaccounted_for() {
    let mut unaccounted = Vec::new();

    for (label, part) in every_part_shape() {
        let report = bedrock_outcomes(part);
        if report.outcomes().is_empty() {
            unaccounted.push(label);
        }
    }

    assert!(
        unaccounted.is_empty(),
        "these parts produced no recorded outcome, so their fate is invisible: {unaccounted:?}"
    );
}

#[test]
fn unsupported_media_is_recorded_as_omitted_with_a_reason() {
    for (label, mime) in
        [("audio", "audio/wav"), ("video", "video/mp4"), ("binary", "application/octet-stream")]
    {
        let report = bedrock_outcomes(Part::inline_data(mime, vec![1, 2, 3]));

        let omitted: Vec<_> = report.omitted_parts().collect();
        assert_eq!(omitted.len(), 1, "{label} ({mime}) must be recorded exactly once: {report:?}");
        assert!(!omitted[0].detail.is_empty(), "{label} must carry a reason a caller can act on");
        assert_eq!(omitted[0].mime_type.as_deref(), Some(mime));
    }
}

#[test]
fn a_textualized_file_reference_is_recorded_as_downgraded_not_converted() {
    // The model reads the reference but cannot fetch it, so this is a loss and must say so.
    let report = bedrock_outcomes(Part::file_data("image/png", "https://example.test/a.png"));

    let downgraded: Vec<_> = report.downgraded_parts().collect();
    assert_eq!(downgraded.len(), 1, "the reference must be recorded as a downgrade: {report:?}");
    assert_eq!(downgraded[0].disposition, PartDisposition::Downgraded { to: "text" });
    assert!(!report.has_omissions(), "it still reaches the model, so it is not an omission");
    assert!(report.has_losses(), "but it is a loss of fidelity");
}

#[test]
fn supported_media_records_no_loss() {
    for (label, mime) in [("png", "image/png"), ("pdf", "application/pdf")] {
        let report = bedrock_outcomes(Part::inline_data(mime, vec![1, 2, 3]));
        assert!(
            !report.has_losses(),
            "{label} is natively supported, so nothing is lost: {report:?}"
        );
    }
}

#[test]
fn omissions_can_be_turned_into_a_pre_dispatch_error() {
    // A caller that cannot tolerate a partial request should fail before the network call
    // rather than receive an answer about material the model never saw.
    let report = bedrock_outcomes(Part::inline_data("audio/wav", vec![1, 2, 3]));
    let error = report.into_error().expect("an omission must be convertible to an error");

    let message = error.to_string();
    assert!(message.contains("audio/wav"), "{message}");
    assert!(message.contains("bedrock"), "{message}");
}
