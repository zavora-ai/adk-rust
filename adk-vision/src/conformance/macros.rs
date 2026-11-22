//! The per-trait test macros.

/// Expands to the fixture expression, or [`Fixture::shapes`](crate::conformance::Fixture::shapes).
#[doc(hidden)]
#[macro_export]
macro_rules! __vision_fixture {
    () => {
        $crate::conformance::Fixture::shapes()
    };
    ($fixture:expr) => {
        $fixture
    };
}

/// The cases every image-taking backend runs.
#[doc(hidden)]
#[macro_export]
macro_rules! __vision_common_cases {
    ([$($attr:meta),*] $target:expr, $fixture:expr) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_coordinates() {
            let probe = $crate::conformance::AnyProbe::from($target);
            $crate::conformance::coordinates(&probe, &$fixture).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_usage() {
            let probe = $crate::conformance::AnyProbe::from($target);
            $crate::conformance::usage(&probe, &$fixture).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_cancellation() {
            let probe = $crate::conformance::AnyProbe::from($target);
            $crate::conformance::cancellation(&probe, &$fixture).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_kill_switch() {
            let probe = $crate::conformance::AnyProbe::from($target);
            $crate::conformance::kill_switch(&probe, &$fixture).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_egress() {
            let probe = $crate::conformance::AnyProbe::from($target);
            $crate::conformance::egress(&probe, &$fixture).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_unreleased() {
            let probe = $crate::conformance::AnyProbe::from($target);
            $crate::conformance::unreleased(&probe, &$fixture).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_oversized() {
            let probe = $crate::conformance::AnyProbe::from($target);
            $crate::conformance::oversized(&probe, &$fixture).await;
        }
    };
}

/// The optional cases: a slow backend for cancellation within one call, a rate-limited one
/// for error mapping.
#[doc(hidden)]
#[macro_export]
macro_rules! __vision_optional_cases {
    ($attrs:tt $fixture:expr, $($slow:expr)?, $($limited:expr)?) => {
        $( $crate::__vision_slow_case!($attrs $fixture, $slow); )?
        $( $crate::__vision_rate_limited_case!($attrs $fixture, $limited); )?
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __vision_slow_case {
    ([$($attr:meta),*] $fixture:expr, $slow:expr) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_cancellation_within_one_call() {
            let probe = $crate::conformance::AnyProbe::from($slow);
            $crate::conformance::cancellation_within_one_call(&probe, &$fixture).await;
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __vision_rate_limited_case {
    ([$($attr:meta),*] $fixture:expr, $limited:expr) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_rate_limited() {
            let probe = $crate::conformance::AnyProbe::from($limited);
            $crate::conformance::rate_limited(&probe, &$fixture).await;
        }
    };
}

/// Generates the conformance cases for a [`Detector`](crate::Detector).
///
/// `backend` is a [`DetectorTarget`](crate::conformance::DetectorTarget) expression,
/// evaluated once per case: `DetectorTarget::Local(Arc::new(my_detector))`, or
/// `DetectorTarget::Remote(..)` for a backend implemented for `ReleasedImage`. `fixture` defaults to
/// [`Fixture::shapes`](crate::conformance::Fixture::shapes); `slow` names a target that
/// takes long enough to cancel, `rate_limited` one that answers with a rate limit.
/// Attributes before `backend` apply to every case.
///
/// See the [module documentation](crate::conformance).
#[macro_export]
macro_rules! detector_conformance_tests {
    ($(#[$attr:meta])* backend = $target:expr $(, fixture = $fixture:expr)? $(, slow = $slow:expr)? $(, rate_limited = $limited:expr)? $(,)?) => {
        $crate::__vision_common_cases!([$($attr),*] $target, $crate::__vision_fixture!($($fixture)?));
        $crate::__vision_optional_cases!([$($attr),*] $crate::__vision_fixture!($($fixture)?), $($slow)?, $($limited)?);
    };
}

/// Generates the conformance cases for a [`Segmenter`](crate::Segmenter). Arguments as
/// [`detector_conformance_tests!`](crate::detector_conformance_tests).
#[macro_export]
macro_rules! segmenter_conformance_tests {
    ($(#[$attr:meta])* backend = $target:expr $(, fixture = $fixture:expr)? $(, slow = $slow:expr)? $(, rate_limited = $limited:expr)? $(,)?) => {
        $crate::__vision_common_cases!([$($attr),*] $target, $crate::__vision_fixture!($($fixture)?));
        $crate::__vision_optional_cases!([$($attr),*] $crate::__vision_fixture!($($fixture)?), $($slow)?, $($limited)?);
    };
}

/// Generates the conformance cases for a [`DocumentReader`](crate::DocumentReader).
/// Arguments as [`detector_conformance_tests!`](crate::detector_conformance_tests).
#[macro_export]
macro_rules! document_reader_conformance_tests {
    ($(#[$attr:meta])* backend = $target:expr $(, fixture = $fixture:expr)? $(, slow = $slow:expr)? $(, rate_limited = $limited:expr)? $(,)?) => {
        $crate::__vision_common_cases!([$($attr),*] $target, $crate::__vision_fixture!($($fixture)?));
        $crate::__vision_optional_cases!([$($attr),*] $crate::__vision_fixture!($($fixture)?), $($slow)?, $($limited)?);
    };
}

/// Generates the conformance cases for a [`Describer`](crate::Describer). A describer with
/// no region adapter passes the coordinates case by refusing `want_regions` with
/// `Unsupported`. Arguments as [`detector_conformance_tests!`](crate::detector_conformance_tests).
#[macro_export]
macro_rules! describer_conformance_tests {
    ($(#[$attr:meta])* backend = $target:expr $(, fixture = $fixture:expr)? $(, slow = $slow:expr)? $(, rate_limited = $limited:expr)? $(,)?) => {
        $crate::__vision_common_cases!([$($attr),*] $target, $crate::__vision_fixture!($($fixture)?));
        $crate::__vision_optional_cases!([$($attr),*] $crate::__vision_fixture!($($fixture)?), $($slow)?, $($limited)?);
    };
}

/// Generates the conformance cases for an [`Embedder`](crate::Embedder), with a case for
/// the space's dimensions. Arguments as
/// [`detector_conformance_tests!`](crate::detector_conformance_tests).
#[macro_export]
macro_rules! embedder_conformance_tests {
    ($(#[$attr:meta])* backend = $target:expr $(, fixture = $fixture:expr)? $(, slow = $slow:expr)? $(, rate_limited = $limited:expr)? $(,)?) => {
        $crate::__vision_common_cases!([$($attr),*] $target, $crate::__vision_fixture!($($fixture)?));
        $crate::__vision_optional_cases!([$($attr),*] $crate::__vision_fixture!($($fixture)?), $($slow)?, $($limited)?);
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_dimensions() {
            $crate::conformance::embedder_dimensions($target, &$crate::__vision_fixture!($($fixture)?)).await;
        }
    };
}

/// Generates the conformance cases for an [`Editor`](crate::Editor), with a case for
/// provenance. Arguments as [`detector_conformance_tests!`](crate::detector_conformance_tests).
#[macro_export]
macro_rules! editor_conformance_tests {
    ($(#[$attr:meta])* backend = $target:expr $(, fixture = $fixture:expr)? $(, slow = $slow:expr)? $(, rate_limited = $limited:expr)? $(,)?) => {
        $crate::__vision_common_cases!([$($attr),*] $target, $crate::__vision_fixture!($($fixture)?));
        $crate::__vision_optional_cases!([$($attr),*] $crate::__vision_fixture!($($fixture)?), $($slow)?, $($limited)?);
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn conformance_provenance() {
            let probe = $crate::conformance::AnyProbe::from($target);
            $crate::conformance::editor_provenance(&probe, &$crate::__vision_fixture!($($fixture)?)).await;
        }
    };
}

/// Generates the conformance cases for a [`Tracker`](crate::Tracker). `make` is an
/// expression that builds a new tracker, evaluated once per tracker a case needs.
#[macro_export]
macro_rules! tracker_conformance_tests {
    ($(#[$attr:meta])* make = $make:expr $(,)?) => {
        $(#[$attr])*
        #[test]
        fn conformance_stable_ids() {
            $crate::conformance::tracker::stable_ids($make);
        }
        $(#[$attr])*
        #[test]
        fn conformance_new_objects_get_new_ids() {
            $crate::conformance::tracker::new_objects_get_new_ids($make);
        }
        $(#[$attr])*
        #[test]
        fn conformance_reset_restarts_ids() {
            $crate::conformance::tracker::reset_restarts_ids(|| $make);
        }
    };
}

/// The video cases, each through [`__vision_video_case!`].
#[doc(hidden)]
#[macro_export]
macro_rules! __vision_video_cases {
    ($attrs:tt $target:expr, $fixture:expr; $($name:ident => $case:ident),* $(,)?) => {
        $( $crate::__vision_video_case!($attrs $name, $case, $target, $fixture); )*
    };
}

/// One video case: evaluates the target and fixture in the case's async body.
#[doc(hidden)]
#[macro_export]
macro_rules! __vision_video_case {
    ([$($attr:meta),*] $name:ident, $case:ident, $target:expr, $fixture:expr) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            let fixture = $fixture;
            $crate::conformance::$case($target, &fixture).await;
        }
    };
}

/// Generates the conformance cases for a [`VideoAnalyzer`](crate::VideoAnalyzer): usage,
/// egress, normalisation of event times and track regions, and the refusals — an
/// unsupported request, cancellation, the kill switch, a frame cap, and a remote analyzer
/// without an egress gate.
///
/// `backend` is a [`VideoAnalyzerTarget`](crate::conformance::VideoAnalyzerTarget)
/// expression and `fixture` a [`VideoFixture`](crate::conformance::VideoFixture) one; both
/// are evaluated once per case inside its async body, so they may `.await`, for example to
/// start the local server a recorded answer replays from. Attributes before `backend` apply
/// to every case.
///
/// See the [module documentation](crate::conformance).
#[macro_export]
macro_rules! video_analyzer_conformance_tests {
    ($(#[$attr:meta])* backend = $target:expr, fixture = $fixture:expr $(,)?) => {
        $crate::__vision_video_cases!([$($attr),*] $target, $fixture;
            conformance_video_usage => video_usage,
            conformance_video_egress => video_egress,
            conformance_video_normalisation => video_normalisation,
            conformance_video_refuses_unsupported => video_refuses_unsupported,
            conformance_video_cancellation => video_cancellation,
            conformance_video_kill_switch => video_kill_switch,
            conformance_video_frame_cap => video_frame_cap,
            conformance_video_unreleased => video_unreleased,
        );
    };
}
