//! What the live camera surface shows (spec 3.7 §8.2), in the words of the signed-off boards
//! (`camera-*`, board choices C1–C15): the wire types of its routes, in the OpenAPI document of
//! every `web` build.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::CommandOutcome;
use crate::model::{AgentRef, DecisionCard};

/// A camera's state, as its pill and its picture show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CameraState {
    /// "Live".
    Live,
    /// "Saving frames": the governor looks less often or stopped escalating (board choice C4).
    SavingFrames,
    /// "Paused until midnight": the day's frames are used (board choice C5).
    Paused,
    /// "Can't reach the camera" (board choice C6).
    Lost,
    /// "Camera off": a person switched it off; nothing is captured or sent (board choice C7).
    Off,
    /// "Stream stopped": a person ended the stream (board choice C2).
    Stopped,
}

/// The colour a line of words takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    /// Green: live, watching, within budget.
    Ok,
    /// Amber: saving frames, near the limit.
    Warn,
    /// Red: paused, lost, at the limit.
    Fail,
    /// Grey: off, nothing to say.
    Muted,
    /// The text's own colour.
    Plain,
}

/// How the picture is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StageLook {
    /// The live frame.
    Live,
    /// The last frame, frozen and dimmed under the words (board choice C5).
    Dimmed,
    /// The last frame in grey under the words (board choice C6).
    Grey,
    /// A black panel: nothing is shown because nothing is captured (board choice C7).
    Dark,
}

/// Where a region's tag sits: above its box, below it, or inside it at the frame's edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TagAt {
    /// Above the box.
    Above,
    /// Below the box: a finding, so it does not cover the part's tag.
    Below,
    /// Inside the box, at its top.
    In,
}

/// `GET /surface/cameras/{camera}` answer: one camera, watched large (board choice C1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CameraView {
    /// The camera's id.
    pub id: String,
    /// "Line 2 camera".
    pub name: String,
    /// "RTSP · 1920 × 1080 · on the line 2 server".
    pub ingest: String,
    /// The state.
    pub state: CameraState,
    /// "Live", "Saving frames", "Paused until midnight", "Can't reach the camera", "Camera off".
    pub state_text: String,
    /// The pill's colour.
    pub tone: Tone,
    /// The switch: on, or off (board choice C2).
    pub on: bool,
    /// The agent that watches it.
    pub agent: CameraAgent,
    /// The picture.
    pub stage: StageView,
    /// The banner over the picture, when there is something to warn about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<NoticeView>,
    /// What the person may do here now.
    pub actions: CameraActions,
    /// The agent's open decisions on this session, such as the held line stop (board choice C13).
    pub decisions: Vec<DecisionCard>,
    /// "Nothing is waiting for you."
    pub nothing_waiting: String,
    /// "Watching for".
    pub watches: Vec<WatchView>,
    /// "Frame budget".
    pub budget: BudgetView,
    /// "Tracks now".
    pub tracks: Vec<TrackRow>,
    /// The words under the tracks: "Tracks are anonymous and start again when the camera does."
    pub tracks_note: String,
    /// The phone's words under the budget while the camera is off: "Tracks start again when the
    /// camera is back on."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracks_short_note: Option<String>,
    /// The zones a person may edit, in the camera's pixels.
    pub zones: Vec<ZoneView>,
    /// "Sources": every camera, this one's tile following its state (board choice C15).
    pub sources: Vec<SourceTile>,
}

/// The agent that watches a camera.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CameraAgent {
    /// The agent.
    pub agent: AgentRef,
    /// "Asleep until a defect", "Paused by the frame budget", "Not watching: no frames".
    pub line: String,
    /// The line's colour.
    pub tone: Tone,
    /// The session its thread follows: "Open the thread".
    pub session_id: String,
}

/// What a person may do with a camera now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CameraActions {
    /// "Camera on / Off".
    pub switch: bool,
    /// "Stop the stream".
    pub stop: bool,
    /// "Try again now".
    pub retry: bool,
    /// "Draw a zone", "Edit the zone".
    pub zone: bool,
    /// "Raise a card from this frame".
    pub raise_card: bool,
}

/// The picture and what is drawn over it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StageView {
    /// The camera's width in pixels; every region is in these pixels.
    pub width: u32,
    /// The camera's height in pixels.
    pub height: u32,
    /// How the picture is drawn.
    pub look: StageLook,
    /// The frame shown, when one is held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<FrameView>,
    /// "Live · 10:41:09", "Last frame · 15:12:03".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<BadgeView>,
    /// "Looking 5 a second · overlays 0.1 s behind" (board choice C10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<String>,
    /// The words over the picture when it is not live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverView>,
    /// What was found on the frame.
    pub regions: Vec<RegionView>,
    /// The zones.
    pub zones: Vec<ZoneView>,
    /// Where each track was over the frames held.
    pub trails: Vec<TrailView>,
}

/// A frame the hub holds, served by its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FrameView {
    /// `sha256:` of its bytes.
    pub digest: String,
    /// `/api/surface/cameras/{camera}/frames/{digest}`: the bytes, while the hub holds them.
    pub href: String,
    /// When it was captured.
    pub at: DateTime<Utc>,
    /// "10:41:09".
    pub at_text: String,
}

/// A badge on the picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BadgeView {
    /// "Live · 10:41:09".
    pub text: String,
    /// The dot's colour.
    pub tone: Tone,
}

/// The words over a picture that is not live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoverView {
    /// "Paused until midnight", "Can't reach the line 2 camera", "Camera off".
    pub title: String,
    /// "Nothing is captured or sent. You switched line 2 off at 11:05."
    pub text: String,
    /// A spinner: the source is being tried again.
    pub retrying: bool,
    /// The camera-off icon.
    pub off: bool,
}

/// Something found on the frame, in the camera's pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegionView {
    /// "part", "seal tear", "fiducial", "person": the overlay's colour (board choice S1).
    pub label: String,
    /// `[x0, y0, x1, y1]`; absent for a mask drawn alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
    /// "part · T45", "seal tear · 0.91 · 5 frames · 14.3 mm", "person · blurred".
    pub tag: String,
    /// The phone's and a tile's tag: "T45", "14.3 mm".
    pub short_tag: String,
    /// Where the tag sits.
    pub tag_at: TagAt,
    /// The anonymous track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<u64>,
    /// A mask's outline (the Masks layer).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outline: Vec<[f64; 2]>,
    /// A measured length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<MeasurementView>,
    /// What a watch looks for: drawn selected.
    pub finding: bool,
}

/// A measured length, drawn as a line with its ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementView {
    /// One end, in the camera's pixels.
    pub from: [f64; 2],
    /// The other end.
    pub to: [f64; 2],
    /// "14.3 mm".
    pub text: String,
}

/// A zone, in the camera's pixels (board choice C12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ZoneView {
    /// Its id.
    pub id: String,
    /// "Inspection window".
    pub name: String,
    /// `[x0, y0, x1, y1]` in the camera's pixels.
    pub corners: [f64; 4],
    /// "700, 200 → 1600, 880".
    pub corners_text: String,
    /// The watches that use it: "Seal tear watch".
    pub used_by: Vec<String>,
}

/// Where a track was over the frames held: its trail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrailView {
    /// The track.
    pub track: u64,
    /// Its centres, newest first, in the camera's pixels.
    pub points: Vec<[f64; 2]>,
}

/// The banner over the picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoticeView {
    /// Its colour.
    pub tone: Tone,
    /// "Saving frames.", "Parts on line 2 are passing without inspection."
    pub title: String,
    /// What follows the title.
    pub text: String,
    /// The error, credentials hidden: "connection refused · rtsp://[credentials hidden]@…".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The phone's shorter title.
    pub short_title: String,
    /// The phone's shorter text.
    pub short_text: String,
    /// What the banner offers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<NoticeAction>,
}

/// What a banner offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum NoticeAction {
    /// "Try again now".
    Retry {
        /// The words.
        text: String,
        /// The phone's words.
        short_text: String,
    },
    /// "Change the daily frame limit", in Memory & rules.
    Limit {
        /// The words.
        text: String,
        /// The phone's words.
        short_text: String,
        /// Where it opens.
        href: String,
    },
}

/// One watch condition (board choice C1 "Watching for").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WatchView {
    /// Its id.
    pub id: String,
    /// "Seal tear".
    pub name: String,
    /// The label it looks for: its colour.
    pub label: String,
    /// "Watching", "Paused", "No frames", "Camera off".
    pub pill: String,
    /// It looks at frames now.
    pub active: bool,
    /// "Score 0.80 or more, 3 frames in a row, inside".
    pub before: String,
    /// The zone it looks inside: "Inspection window".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    /// ". Then wait 30 s before waking again for the same track."
    pub after: String,
    /// The phone's line: "Seal tear, 0.80+, 3 frames in".
    pub short_before: String,
    /// What follows the zone on the phone: " · not watching" when it does not look.
    pub short_after: String,
    /// "Woke the Inspector 3 times today".
    pub woke: String,
}

/// "Frame budget" (board choice C3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BudgetView {
    /// "for line 2".
    pub scope: String,
    /// Looking, Asking, Frames today.
    pub meters: Vec<MeterView>,
    /// "Steps down in this order: …", while nothing has stepped down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    /// "What the budget did": each step with its time, then the next.
    pub steps: Vec<StepView>,
    /// "Change the daily frame limit", once the camera paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    /// The phone's rows.
    pub rows: Vec<BudgetRow>,
}

/// One meter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeterView {
    /// "Looking", "Asking sam-3.1 about unclear tracks", "Frames today".
    pub label: String,
    /// "5 a second", "2 in the last minute", "61,240 of 300,000".
    pub value: String,
    /// How full the bar is, in hundredths.
    pub percent: u8,
    /// The bar's colour.
    pub tone: Tone,
    /// "As set", "At most 6 a minute · 14 today · $0.14", "Renews at midnight".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One step the governor took, or the next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepView {
    /// "14:02", or "next".
    pub at: String,
    /// "Looked less often: 5 → 2 a second (90% used)".
    pub text: String,
    /// The next step, not taken yet.
    pub next: bool,
}

/// One of the phone's budget rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BudgetRow {
    /// "Looking".
    pub label: String,
    /// "2 a second (was 5)".
    pub value: String,
    /// Its colour; `muted` for plain words.
    pub tone: Tone,
}

/// One track now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackRow {
    /// "T43".
    pub id: String,
    /// "part".
    pub label: String,
    /// "seal tear 0.91 · in the zone · 5 frames".
    pub note: String,
    /// The phone's note: "seal tear 0.91 · in the zone".
    pub short_note: String,
    /// The swatch: the finding's label, else the track's.
    pub swatch: String,
    /// A watch looks for what was found on it: the row is marked.
    pub finding: bool,
}

/// One camera in "Sources", the strip under a single camera (board choice C15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceTile {
    /// The camera's id.
    pub id: String,
    /// "Line 3 camera".
    pub name: String,
    /// The state.
    pub state: CameraState,
    /// "Live", "Saving frames", "Paused", "Lost", "Off".
    pub state_text: String,
    /// Its colour.
    pub tone: Tone,
    /// The camera shown large.
    pub selected: bool,
    /// The small picture.
    pub stage: StageView,
}

/// `GET /surface/cameras` answer: every camera, a grid of them (board choice C8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CamerasView {
    /// "Cameras".
    pub title: String,
    /// "4 sources · 3 on".
    pub count_text: String,
    /// The phone's: "4 · 3 on".
    pub count_short: String,
    /// The agent that watches them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<CameraAgent>,
    /// The camera shown first, by its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    /// The phone's line under the large picture: "Line 2 camera · Live · 5 a second".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_line: Option<String>,
    /// Every camera.
    pub tiles: Vec<CameraTile>,
    /// "Each source has its own frame budget. Stop all pauses every one of them."
    pub note: String,
    /// "Each camera has its own frame budget. People are blurred before any frame is shown or kept."
    pub short_note: String,
    /// "Select a camera to watch it large. People are blurred before any frame is shown or kept."
    pub footer: String,
    /// The agents' open decisions on these cameras' sessions.
    pub decisions: Vec<DecisionCard>,
    /// "Nothing is waiting for you."
    pub nothing_waiting: String,
    /// Each camera's frames today.
    pub budget: Vec<MeterView>,
}

/// One camera of the grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CameraTile {
    /// The camera's id.
    pub id: String,
    /// "Line 2 camera".
    pub name: String,
    /// The state.
    pub state: CameraState,
    /// "Live", "Saving frames", "Off".
    pub state_text: String,
    /// Its colour.
    pub tone: Tone,
    /// The switch.
    pub on: bool,
    /// The switch may be changed here.
    pub can_switch: bool,
    /// The picture, with its overlays.
    pub stage: StageView,
    /// "watching for seal tears", "shown only, nothing watched", "from the voice screen".
    pub caption: String,
    /// The switch list's line: "5 a second · 20% of today's frames".
    pub line: String,
    /// The phone's line: "watching", "2 a second", "shown only".
    pub short_line: String,
}

/// `PUT /surface/cameras/{camera}/zones/{zone}` body: the zone's new name and corners, and the
/// corners the person saw before changing them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ZoneChange {
    /// The zone's name.
    pub name: String,
    /// `[x0, y0, x1, y1]` in the camera's pixels.
    pub corners: [f64; 4],
    /// The corners the person started from: a zone changed since is refused, not overwritten.
    pub was: [f64; 4],
}

/// `PUT /surface/cameras/{camera}/zones/{zone}` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ZoneSaved {
    /// The zone now.
    pub zone: ZoneView,
    /// "Saved. The seal tear watch uses it from the next frame."
    pub text: String,
}

/// `POST /surface/cameras/{camera}/freeze` body: the frame on screen, by its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FreezeFrame {
    /// The frame's `sha256:` digest; the hub must still hold it.
    pub frame: String,
}

/// `POST /surface/cameras/{camera}/cards` body: a card from a frozen frame (board choice C11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RaiseCard {
    /// The frozen frame, by its digest; the hub must still hold it.
    pub frame: String,
    /// "What should it look at?"
    pub note: String,
}

/// `POST /surface/cameras/{camera}/cards` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardRaised {
    /// The kept frame's digest, after blurring: the evidence the card and a held call carry.
    pub digest: String,
    /// The short form: `sha256:c52ca5…ce7550`.
    pub short_digest: String,
    /// The regions that went with it.
    pub regions: usize,
    /// "Kept as evidence until 18 Dec".
    pub kept_until: String,
    /// The kept frame in the annotated viewer.
    pub href: String,
    /// What sending it to the agent did: sent, or queued behind a decision or a turn.
    pub outcome: CommandOutcome,
    /// "Raised to the Inspector. This one frame is kept as evidence until 18 Dec."
    pub text: String,
}
