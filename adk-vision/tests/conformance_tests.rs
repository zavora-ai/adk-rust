//! The conformance suite run against scripted backends of every trait, local and remote
//! (spec 3.7 §7.18).

mod support;

mod local_detector {
    use std::sync::Arc;
    use std::time::Duration;

    use adk_vision::conformance::DetectorTarget;

    use super::support::ColourDetector;

    adk_vision::detector_conformance_tests! {
        backend = DetectorTarget::Local(Arc::new(ColourDetector::local("colour"))),
        slow = DetectorTarget::Local(Arc::new(ColourDetector::local("slow").slow(Duration::from_secs(30)))),
        rate_limited = DetectorTarget::Local(Arc::new(ColourDetector::local("limited").rate_limited())),
    }
}

mod local_detector_with_a_resize {
    use std::sync::Arc;

    use adk_vision::InputLimits;
    use adk_vision::conformance::DetectorTarget;

    use super::support::ColourDetector;

    fn half() -> Arc<ColourDetector> {
        let mut detector = ColourDetector::local("half");
        detector.descriptor.limits = InputLimits::long_edge(320);
        Arc::new(detector)
    }

    adk_vision::detector_conformance_tests! {
        backend = DetectorTarget::Local(half()),
    }
}

mod remote_detector {
    use std::sync::Arc;

    use adk_vision::conformance::DetectorTarget;

    use super::support::ColourDetector;

    adk_vision::detector_conformance_tests! {
        backend = DetectorTarget::Remote(Arc::new(ColourDetector::remote("sam-3.1"))),
        rate_limited = DetectorTarget::Remote(Arc::new(ColourDetector::remote("limited").rate_limited())),
    }
}

mod local_segmenter {
    use std::sync::Arc;

    use adk_vision::conformance::SegmenterTarget;

    use super::support::ColourSegmenter;

    adk_vision::segmenter_conformance_tests! {
        backend = SegmenterTarget::Local(Arc::new(ColourSegmenter::local())),
    }
}

mod local_reader {
    use std::sync::Arc;

    use adk_vision::conformance::ReaderTarget;

    use super::support::ColourReader;

    adk_vision::document_reader_conformance_tests! {
        backend = ReaderTarget::Local(Arc::new(ColourReader::local("reader", 0.9))),
    }
}

mod remote_reader {
    use std::sync::Arc;

    use adk_vision::conformance::ReaderTarget;

    use super::support::RemoteReader;

    adk_vision::document_reader_conformance_tests! {
        backend = ReaderTarget::Remote(Arc::new(RemoteReader::new())),
    }
}

mod remote_describer {
    use std::sync::Arc;

    use adk_vision::conformance::DescriberTarget;

    use super::support::ColourDescriber;

    adk_vision::describer_conformance_tests! {
        backend = DescriberTarget::Remote(Arc::new(ColourDescriber::remote("gemini", 0.9))),
    }
}

mod local_embedder {
    use std::sync::Arc;

    use adk_vision::conformance::EmbedderTarget;

    use super::support::MeanEmbedder;

    adk_vision::embedder_conformance_tests! {
        backend = EmbedderTarget::Local(Arc::new(MeanEmbedder::local())),
    }
}

mod deterministic_editor {
    use std::sync::Arc;

    use adk_vision::DeterministicEditor;
    use adk_vision::conformance::EditorTarget;

    adk_vision::editor_conformance_tests! {
        backend = EditorTarget::Local(Arc::new(DeterministicEditor::new())),
    }
}

mod greedy_tracker {
    adk_vision::tracker_conformance_tests! {
        make = adk_vision::GreedyIouTracker::new(0.3, 2),
    }
}
