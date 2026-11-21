//! The live camera surface (spec 3.7 §8.2, X12): watching a stream and acting on it, on the
//! web home and the phone, built to the signed-off boards (`camera-*`, board choices C1–C15).
//!
//! | Part | What |
//! |---|---|
//! | [`CameraSource`] | The seam to a frame pipeline: each camera's [`CameraFacts`] — the stream's state, the frame governor's numbers, tracks, watch conditions, zones — and the acts a person may ask of it |
//! | [`Cameras`] | The hub every frame passes ([`Cameras::offer`]): a camera switched off or stopped has its frames dropped on the server, whatever arrives; the rest are held in memory only, the latest [`HELD_FRAMES`], never written |
//! | The view model ([`CameraView`], [`CamerasView`], ...) | What the screens show, in the boards' words, in the OpenAPI document of every `web` build |
//! | `PipelineCameras` (`vision-camera`) | Each camera on adk-vision's frame pipeline — source, frame governor, privacy, detector, tracker, watch conditions — reporting what the pipeline measured; the voice screen's camera through `SurfaceCameraSource` |
//! | `ScriptedCameras` (`web`) | A fixture-driven source with every board state, for the tests and the screenshots |
//! | `bridge` (`voice`) | A camera's frames fed to a realtime session under the voice screen's `FramePolicy` |
//!
//! # Rules
//!
//! - **Off sends nothing.** The voice screen's rule ([`voice::VisualInputs`](crate::voice)) holds
//!   here: the hub drops every frame of a camera that is off, and switching off forgets the
//!   frames held (board choice C7).
//! - **No frame is stored unless it is evidence on a card.** Raising a card keeps exactly the
//!   frozen frame, through [`CameraSource::keep_frame`], everyone in it blurred first (board
//!   choice C11, spec 3.7 V-I9); the card reaches the agent as the person's message naming the
//!   evidence by digest and artifact (V-I15).
//! - **Authorised as the inbox is.** A camera belongs to its agent's session; only that
//!   session's owner sees or acts on it (spec 3.3 §11 S-I8). Every act is recorded.

mod desk;
mod facts;
mod hub;
#[cfg(feature = "vision-camera")]
mod live;
mod project;
#[cfg(feature = "web")]
mod scripted;
mod view;

#[cfg(feature = "voice")]
pub mod bridge;

pub use facts::{
    CameraFacts, CameraFrame, EscalationFacts, Finding, GovernorFacts, GovernorStep,
    GovernorStepKind, HeldFrame, Ingest, Measurement, Overlay, StreamState, TrackFacts, TrackPhase,
    WatchFacts, ZoneFacts,
};
pub use hub::{
    Acted, CameraSource, CameraUpdate, Cameras, DropReason, FROZEN_FRAMES, FROZEN_MINUTES,
    FrameIntake, HELD_FRAMES, Offered,
};
#[cfg(feature = "vision-camera")]
pub use live::{LiveCamera, LiveWatch, PipelineCameras, PipelineCamerasBuilder};
#[cfg(feature = "web")]
pub use scripted::{CameraBoard, ScriptedCameras};
pub use view::{
    BadgeView, BudgetRow, BudgetView, CameraActions, CameraAgent, CameraRegion, CameraState,
    CameraTile, CameraView, CamerasView, CardRaised, CoverView, FrameView, FreezeFrame,
    MeasurementView, MeterView, NoticeAction, NoticeView, RaiseCard, SourceTile, StageLook,
    StageView, StepView, TagAt, Tone, TrackRow, TrailView, WatchView, ZoneChange, ZoneSaved,
    ZoneView,
};

/// The audit action of switching a camera on or off (spec 3.7 §8.2, board choice C2).
pub const SURFACE_AUDIT_CAMERA_SWITCH: &str = "surface.camera.switch";
/// The audit action of stopping a camera's stream.
pub const SURFACE_AUDIT_CAMERA_STOP: &str = "surface.camera.stop";
/// The audit action of trying a lost camera again.
pub const SURFACE_AUDIT_CAMERA_RETRY: &str = "surface.camera.retry";
/// The audit action of changing a zone (board choice C12).
pub const SURFACE_AUDIT_CAMERA_ZONE: &str = "surface.camera.zone";
/// The audit action of raising a card from a frame (board choice C11).
pub const SURFACE_AUDIT_CAMERA_CARD: &str = "surface.camera.card";
