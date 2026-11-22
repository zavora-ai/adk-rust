//! Camera facts into the words of the boards (board choices C1–C15): one projection for the
//! web home and the phone, so both say the same thing.

use chrono::{DateTime, FixedOffset, Timelike as _, Utc};

use super::facts::{
    CameraFacts, GovernorFacts, GovernorStepKind, HeldFrame, Overlay, StreamState, TrackPhase,
    WatchFacts,
};
use super::hub::Gate;
use super::view::{
    BadgeView, BudgetRow, BudgetView, CameraActions, CameraRegion, CameraState, CameraTile,
    CoverView, FrameView, MeasurementView, MeterView, NoticeAction, NoticeView, SourceTile,
    StageLook, StageView, StepView, TagAt, Tone, TrackRow, TrailView, WatchView, ZoneView,
};

/// What every projection needs beside the facts.
pub(crate) struct Ctx<'a> {
    pub(crate) offset: FixedOffset,
    pub(crate) now: DateTime<Utc>,
    /// The person reading: "You switched line 2 off".
    pub(crate) user: &'a str,
    /// The watching agent's display name: "Woke the Inspector 3 times today".
    pub(crate) agent_name: String,
}

/// One camera with its gate and the frames the hub holds for it.
pub(crate) struct Camera<'a> {
    pub(crate) facts: &'a CameraFacts,
    pub(crate) gate: Gate,
    pub(crate) recent: Vec<HeldFrame>,
}

/// Where a picture is drawn: large, a tile of the grid, or the strip under a camera.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    Main,
    Tile { selected: bool },
    Strip,
}

/// The route a held frame is served at.
pub(crate) fn frame_href(camera: &str, digest: &str) -> String {
    format!("/api/surface/cameras/{}/frames/{}", encode(camera), encode(digest))
}

fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

impl Camera<'_> {
    pub(crate) fn state(&self) -> CameraState {
        if self.gate.stopped.is_some() {
            return CameraState::Stopped;
        }
        if !self.gate.on {
            return CameraState::Off;
        }
        if self.gate.frozen {
            return CameraState::Frozen;
        }
        match self.facts.state {
            StreamState::Paused { .. } => CameraState::Paused,
            StreamState::Lost { .. } => CameraState::Lost,
            StreamState::Live if self.facts.governor.stepped_down() => CameraState::SavingFrames,
            StreamState::Live => CameraState::Live,
        }
    }

    fn watching(&self) -> bool {
        matches!(self.state(), CameraState::Live | CameraState::SavingFrames)
    }

    fn latest(&self) -> Option<&HeldFrame> {
        self.recent.last()
    }
}

/// `at` as local `HH:MM`.
fn hh_mm(at: DateTime<Utc>, offset: FixedOffset) -> String {
    at.with_timezone(&offset).format("%H:%M").to_string()
}

/// `at` as local `HH:MM:SS`.
fn hh_mm_ss(at: DateTime<Utc>, offset: FixedOffset) -> String {
    at.with_timezone(&offset).format("%H:%M:%S").to_string()
}

/// When the budget renews: "midnight", else `HH:MM`.
fn renews(at: DateTime<Utc>, offset: FixedOffset) -> String {
    let local = at.with_timezone(&offset);
    if local.hour() == 0 && local.minute() == 0 {
        "midnight".into()
    } else {
        local.format("%H:%M").to_string()
    }
}

/// `61240` as "61,240".
pub(crate) fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `5.0` as "5", `0.5` as "0.5".
fn rate(value: f64) -> String {
    if (value - value.round()).abs() < 1e-6 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.1}")
    }
}

/// A share as whole hundredths, `0..=100`.
fn percent(share: f64) -> u8 {
    (share.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// The first letter in capitals: "line 2" → "Line 2".
fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// "1 time", "3 times".
fn times(count: u32) -> String {
    if count == 1 { "once".into() } else { format!("{count} times") }
}

pub(crate) fn state_text(state: CameraState, facts: &CameraFacts, offset: FixedOffset) -> String {
    match state {
        CameraState::Live => "Live".into(),
        CameraState::SavingFrames => "Saving frames".into(),
        CameraState::Paused => format!("Paused until {}", renews(facts.governor.renews_at, offset)),
        CameraState::Lost => "Can't reach the camera".into(),
        CameraState::Off => "Camera off".into(),
        CameraState::Stopped => "Stream stopped".into(),
        CameraState::Frozen => "Paused by Stop all".into(),
    }
}

/// A tile's short state: "Paused", "Lost", "Off".
fn short_state(state: CameraState) -> &'static str {
    match state {
        CameraState::Live => "Live",
        CameraState::SavingFrames => "Saving frames",
        CameraState::Paused => "Paused",
        CameraState::Lost => "Lost",
        CameraState::Off => "Off",
        CameraState::Stopped => "Stopped",
        CameraState::Frozen => "Paused",
    }
}

pub(crate) fn tone(state: CameraState) -> Tone {
    match state {
        CameraState::Live => Tone::Ok,
        CameraState::SavingFrames => Tone::Warn,
        CameraState::Paused | CameraState::Lost | CameraState::Frozen => Tone::Fail,
        CameraState::Off | CameraState::Stopped => Tone::Muted,
    }
}

/// "RTSP · 1920 × 1080 · on the line 2 server".
pub(crate) fn ingest(facts: &CameraFacts) -> String {
    let mut parts = vec![
        facts.ingest.kind.clone(),
        format!("{} × {}", facts.ingest.width, facts.ingest.height),
    ];
    if !facts.ingest.at.is_empty() {
        parts.push(format!("on {}", facts.ingest.at));
    }
    parts.join(" · ")
}

/// What the agent is doing about this camera: "Asleep until a defect".
pub(crate) fn agent_line(camera: &Camera<'_>) -> (String, Tone) {
    match camera.state() {
        CameraState::Live | CameraState::SavingFrames => match camera.facts.watches.first() {
            Some(watch) => (format!("Asleep until {}", wakes_for(watch)), Tone::Muted),
            None => ("Not watching: shown only".into(), Tone::Muted),
        },
        CameraState::Paused => ("Paused by the frame budget".into(), Tone::Fail),
        CameraState::Lost => ("Not watching: no frames".into(), Tone::Fail),
        CameraState::Off => ("Not watching: camera off".into(), Tone::Muted),
        CameraState::Stopped => ("Not watching: stream stopped".into(), Tone::Muted),
        CameraState::Frozen => ("Not watching: Stop all".into(), Tone::Fail),
    }
}

fn wakes_for(watch: &WatchFacts) -> String {
    watch.wakes_for.clone().unwrap_or_else(|| format!("a {}", watch.label))
}

pub(crate) fn actions(camera: &Camera<'_>) -> CameraActions {
    let state = camera.state();
    let stopped = state == CameraState::Stopped;
    CameraActions {
        switch: !stopped,
        stop: !stopped,
        retry: state == CameraState::Lost,
        zone: matches!(state, CameraState::Live | CameraState::SavingFrames | CameraState::Paused)
            && !camera.facts.zones.is_empty(),
        raise_card: camera.watching() && camera.latest().is_some(),
    }
}

// ---------------------------------------------------------------------------
// The picture
// ---------------------------------------------------------------------------

pub(crate) fn stage(camera: &Camera<'_>, place: Place, ctx: &Ctx<'_>) -> StageView {
    let facts = camera.facts;
    let state = camera.state();
    let look = match state {
        CameraState::Live | CameraState::SavingFrames => StageLook::Live,
        CameraState::Paused => StageLook::Dimmed,
        CameraState::Lost => StageLook::Grey,
        CameraState::Off | CameraState::Stopped => StageLook::Dark,
        CameraState::Frozen => StageLook::Dimmed,
    };
    let held = match look {
        StageLook::Dark => None,
        StageLook::Live | StageLook::Dimmed | StageLook::Grey => camera.latest(),
    };
    let frame = held.map(|held| FrameView {
        digest: held.digest.clone(),
        href: frame_href(&facts.id, &held.digest),
        at: held.frame.at,
        at_text: hh_mm_ss(held.frame.at, ctx.offset),
    });
    let live = look == StageLook::Live;
    let badge = match (place, &frame) {
        (Place::Strip, _) | (_, None) => None,
        (Place::Tile { selected: false }, Some(_)) if live => Some(BadgeView {
            text: format!("Live · {} a second", rate(facts.governor.rate)),
            tone: Tone::Ok,
        }),
        (_, Some(frame)) => Some(match state {
            CameraState::Live => {
                BadgeView { text: format!("Live · {}", frame.at_text), tone: Tone::Ok }
            }
            CameraState::SavingFrames => {
                BadgeView { text: format!("Live · {}", frame.at_text), tone: Tone::Warn }
            }
            CameraState::Paused => {
                BadgeView { text: format!("Last frame · {}", frame.at_text), tone: Tone::Warn }
            }
            CameraState::Lost | CameraState::Off | CameraState::Stopped | CameraState::Frozen => {
                BadgeView { text: format!("Last frame · {}", frame.at_text), tone: Tone::Fail }
            }
        }),
    };
    let rate_badge = match (place, held) {
        (Place::Main, Some(held)) if live => Some(format!(
            "Looking {} a second · overlays {:.1} s behind",
            rate(facts.governor.rate),
            f64::from(held.frame.latency_ms) / 1000.0
        )),
        _ => None,
    };
    let (regions, trails, zones) = match (live, held) {
        (true, Some(held)) => (
            held.frame
                .overlays
                .iter()
                .map(|overlay| region(overlay, facts, held.frame.width))
                .collect(),
            trails(&camera.recent),
            zones(facts),
        ),
        _ => (Vec::new(), Vec::new(), Vec::new()),
    };
    StageView {
        width: held.map_or(facts.ingest.width, |held| held.frame.width),
        height: held.map_or(facts.ingest.height, |held| held.frame.height),
        look,
        frame,
        badge,
        rate: rate_badge,
        cover: match place {
            Place::Main | Place::Tile { .. } => cover(camera, place, ctx),
            Place::Strip => None,
        },
        regions,
        zones,
        trails,
    }
}

fn cover(camera: &Camera<'_>, place: Place, ctx: &Ctx<'_>) -> Option<CoverView> {
    let facts = camera.facts;
    let tile = matches!(place, Place::Tile { .. });
    match camera.state() {
        CameraState::Live | CameraState::SavingFrames => None,
        CameraState::Paused => Some(CoverView {
            title: format!("Paused until {}", renews(facts.governor.renews_at, ctx.offset)),
            text: format!(
                "Today's {} frames are used. Nothing on {} is being checked until the budget renews at {}.",
                thousands(facts.governor.frames_limit),
                facts.place,
                hh_mm(facts.governor.renews_at, ctx.offset)
            ),
            retrying: false,
            off: false,
        }),
        CameraState::Lost => {
            let StreamState::Lost { since, retry_at, attempt, .. } = &facts.state else {
                return None;
            };
            let wait = (*retry_at - ctx.now).num_seconds();
            let next = if wait > 0 {
                format!("trying again in {wait} s")
            } else {
                "trying again now".into()
            };
            Some(CoverView {
                title: format!("Can't reach the {} camera", facts.place),
                text: format!(
                    "Since {} · {next} · attempt {attempt}",
                    hh_mm_ss(*since, ctx.offset)
                ),
                retrying: true,
                off: false,
            })
        }
        CameraState::Off if tile => Some(CoverView {
            title: String::new(),
            text: "Nothing is captured or sent".into(),
            retrying: false,
            off: true,
        }),
        CameraState::Off => {
            let who = camera
                .gate
                .switched
                .as_ref()
                .map(|acted| {
                    let by =
                        if acted.by == ctx.user { "You".to_string() } else { acted.by.clone() };
                    format!(
                        " {by} switched {} off at {}.",
                        facts.place,
                        hh_mm(acted.at, ctx.offset)
                    )
                })
                .unwrap_or_default();
            Some(CoverView {
                title: "Camera off".into(),
                text: format!("Nothing is captured or sent.{who}"),
                retrying: false,
                off: true,
            })
        }
        CameraState::Frozen => Some(CoverView {
            title: "Paused by Stop all".into(),
            text: if tile {
                "Nothing is looked at".into()
            } else {
                "Nothing is looked at or sent until someone chooses Resume all.".into()
            },
            retrying: false,
            off: false,
        }),
        CameraState::Stopped => {
            let who = camera.gate.stopped.as_ref().map(|acted| {
                let by = if acted.by == ctx.user { "You".to_string() } else { acted.by.clone() };
                format!(" {by} stopped it at {}.", hh_mm(acted.at, ctx.offset))
            });
            Some(CoverView {
                title: "Stream stopped".into(),
                text: format!("Nothing is captured or sent.{}", who.unwrap_or_default()),
                retrying: false,
                off: true,
            })
        }
    }
}

/// Whether `label` is what a watch on the camera looks for.
fn is_finding(label: &str, facts: &CameraFacts) -> bool {
    facts.watches.iter().any(|watch| watch.label == label)
}

fn region(overlay: &Overlay, facts: &CameraFacts, width: u32) -> CameraRegion {
    let finding = is_finding(&overlay.label, facts);
    let measurement = overlay.measurement.as_ref().map(|measured| MeasurementView {
        from: measured.from,
        to: measured.to,
        text: format!("{:.1} mm", measured.millimetres),
    });
    let track = overlay.track.map(|track| format!("T{track}"));
    let (tag, short_tag) = if overlay.bbox.is_none() {
        (String::new(), String::new())
    } else if overlay.blurred {
        (format!("{} · blurred", overlay.label), format!("{} · blurred", overlay.label))
    } else if finding {
        let mut parts = vec![overlay.label.clone()];
        if let Some(score) = overlay.score {
            parts.push(format!("{score:.2}"));
        }
        if let Some(frames) = overlay.frames {
            parts.push(format!("{frames} frames"));
        }
        if let Some(measured) = &measurement {
            parts.push(measured.text.clone());
        }
        let short = measurement
            .as_ref()
            .map(|measured| measured.text.clone())
            .or_else(|| overlay.score.map(|score| format!("{score:.2}")))
            .unwrap_or_else(|| overlay.label.clone());
        (parts.join(" · "), short)
    } else if let Some(track) = &track {
        (format!("{} · {track}", overlay.label), track.clone())
    } else {
        (overlay.label.clone(), String::new())
    };
    let tag_at = match overlay.bbox {
        _ if finding => TagAt::Below,
        Some(bbox) if bbox[1] <= 1.0 || bbox[2] >= f64::from(width) - 1.0 => TagAt::In,
        _ if overlay.track.is_none() => TagAt::In,
        _ => TagAt::Above,
    };
    CameraRegion {
        label: overlay.label.clone(),
        bbox: overlay.bbox,
        tag,
        short_tag,
        tag_at,
        track: overlay.track,
        outline: overlay.outline.clone(),
        measurement,
        finding,
    }
}

/// Each track's centres over the frames held, newest first, the newest frame's tracks only.
fn trails(recent: &[HeldFrame]) -> Vec<TrailView> {
    let Some(latest) = recent.last() else { return Vec::new() };
    let centre = |overlay: &Overlay| {
        overlay.bbox.map(|[x0, y0, x1, y1]| [f64::midpoint(x0, x1), f64::midpoint(y0, y1)])
    };
    latest
        .frame
        .overlays
        .iter()
        .filter(|overlay| overlay.bbox.is_some() && !overlay.blurred)
        .filter_map(|overlay| {
            let track = overlay.track?;
            // A track's own boxes, one per frame: a finding on the same track draws no trail.
            let label = &overlay.label;
            let now = centre(overlay)?;
            let points: Vec<[f64; 2]> = recent
                .iter()
                .rev()
                .skip(1)
                .filter_map(|held| {
                    held.frame
                        .overlays
                        .iter()
                        .find(|other| other.track == Some(track) && &other.label == label)
                        .and_then(centre)
                })
                // A part that has not moved leaves no trail.
                .filter(|[x, y]| (x - now[0]).hypot(y - now[1]) > 4.0)
                .collect();
            (!points.is_empty()).then_some(TrailView { track, points })
        })
        .collect()
}

pub(crate) fn zones(facts: &CameraFacts) -> Vec<ZoneView> {
    facts
        .zones
        .iter()
        .map(|zone| ZoneView {
            id: zone.id.clone(),
            name: zone.name.clone(),
            corners: zone.corners,
            corners_text: corners_text(zone.corners),
            used_by: facts
                .watches
                .iter()
                .filter(|watch| watch.zone.as_deref() == Some(zone.id.as_str()))
                .map(|watch| format!("{} watch", watch.name))
                .collect(),
        })
        .collect()
}

/// "700, 200 → 1600, 880".
pub(crate) fn corners_text(corners: [f64; 4]) -> String {
    let [x0, y0, x1, y1] = corners.map(|value| value.round() as i64);
    format!("{x0}, {y0} → {x1}, {y1}")
}

// ---------------------------------------------------------------------------
// The banner
// ---------------------------------------------------------------------------

pub(crate) fn notice(camera: &Camera<'_>, ctx: &Ctx<'_>) -> Option<NoticeView> {
    let facts = camera.facts;
    let governor = &facts.governor;
    let used = percent(governor.used());
    match camera.state() {
        CameraState::SavingFrames => {
            let asking = governor
                .escalation
                .as_ref()
                .filter(|escalation| escalation.stopped_at.is_some())
                .map(|escalation| escalation.backend.clone());
            let lowered = governor.rate < governor.set_rate;
            let mut text = Vec::new();
            let mut short = Vec::new();
            if lowered {
                let often = if (governor.rate - 1.0).abs() < 1e-6 {
                    "once".to_string()
                } else {
                    format!("{} times", rate(governor.rate))
                };
                text.push(format!(
                    "Looking {often} a second instead of {}",
                    rate(governor.set_rate)
                ));
                short.push(format!("Looking {} a second", rate(governor.rate)));
            }
            if let Some(backend) = &asking {
                text.push(format!("no longer asking {backend} about unclear tracks"));
                short.push(format!("not asking {backend}"));
            }
            let text = capital(&text.join(", and "));
            Some(NoticeView {
                tone: Tone::Warn,
                title: "Saving frames.".into(),
                text: format!("{text}. Today's frames are {used}% used."),
                detail: None,
                short_title: "Saving frames.".into(),
                short_text: format!("{}. {used}% of today's frames used.", short.join(", ")),
                action: None,
            })
        }
        CameraState::Paused => {
            let StreamState::Paused { since } = &facts.state else { return None };
            let until = renews(governor.renews_at, ctx.offset);
            let (title, short_title) = match &facts.passing {
                Some(what) => (
                    format!("{} on {} are passing without inspection.", capital(what), facts.place),
                    format!("{} are passing without inspection", capital(what)),
                ),
                None => (
                    format!("Nothing on {} is being checked.", facts.place),
                    "Nothing is being checked".into(),
                ),
            };
            Some(NoticeView {
                tone: Tone::Warn,
                title,
                text: format!(
                    "The frame budget paused the camera at {}.",
                    hh_mm(*since, ctx.offset)
                ),
                detail: None,
                short_title,
                short_text: format!("until {until}."),
                action: Some(NoticeAction::Limit {
                    text: "Change the daily frame limit".into(),
                    short_text: "Change the limit".into(),
                    href: "/ui/memory".into(),
                }),
            })
        }
        CameraState::Lost => {
            let StreamState::Lost { error, .. } = &facts.state else { return None };
            Some(NoticeView {
                tone: Tone::Warn,
                title: format!("Nothing on {} is being checked.", facts.place),
                text: String::new(),
                detail: Some(error.clone()),
                short_title: "Nothing is being checked.".into(),
                short_text: String::new(),
                action: Some(NoticeAction::Retry {
                    text: "Try again now".into(),
                    short_text: "Try again now".into(),
                }),
            })
        }
        CameraState::Live | CameraState::Off | CameraState::Stopped | CameraState::Frozen => None,
    }
}

// ---------------------------------------------------------------------------
// Watching for, the budget, the tracks
// ---------------------------------------------------------------------------

pub(crate) fn watches(camera: &Camera<'_>, ctx: &Ctx<'_>) -> Vec<WatchView> {
    let facts = camera.facts;
    let state = camera.state();
    let active = camera.watching();
    let pill = match state {
        CameraState::Live | CameraState::SavingFrames => "Watching",
        CameraState::Paused => "Paused",
        CameraState::Lost => "No frames",
        CameraState::Off => "Camera off",
        CameraState::Stopped => "Stopped",
        CameraState::Frozen => "Paused",
    };
    facts
        .watches
        .iter()
        .map(|watch| {
            let zone = watch
                .zone
                .as_deref()
                .and_then(|id| facts.zones.iter().find(|zone| zone.id == id))
                .map(|zone| zone.name.clone());
            let rule =
                format!("Score {:.2} or more, {} frames in a row", watch.min_score, watch.frames);
            WatchView {
                id: watch.id.clone(),
                name: watch.name.clone(),
                label: watch.label.clone(),
                pill: pill.into(),
                active,
                before: if zone.is_some() { format!("{rule}, inside") } else { rule },
                after: match watch.cooldown_secs {
                    // No wait: each track wakes the agent once.
                    0 => ". It wakes once for each track.".into(),
                    secs => format!(". Then wait {secs} s before waking again for the same track."),
                },
                short_before: format!(
                    "{}, {:.2}+, {} frames{}",
                    watch.name,
                    watch.min_score,
                    watch.frames,
                    if zone.is_some() { " in" } else { "" }
                ),
                short_after: if active { String::new() } else { " · not watching".into() },
                woke: match watch.woke_today {
                    0 => format!("Has not woken the {} today", ctx.agent_name),
                    count => format!("Woke the {} {} today", ctx.agent_name, times(count)),
                },
                zone,
            }
        })
        .collect()
}

pub(crate) fn budget(camera: &Camera<'_>, ctx: &Ctx<'_>) -> BudgetView {
    let facts = camera.facts;
    let governor = &facts.governor;
    let state = camera.state();
    let offset = ctx.offset;
    let until = renews(governor.renews_at, offset);
    let lowered_at = governor.steps.iter().find_map(|step| match step.kind {
        GovernorStepKind::LookLessOften { from, .. } => Some((from, step.at)),
        GovernorStepKind::StopAsking | GovernorStepKind::Pause => None,
    });

    let mut meters = Vec::new();
    let mut rows = Vec::new();
    let row =
        |label: &str, value: String, tone: Tone| BudgetRow { label: label.into(), value, tone };
    // Looking.
    let (looking, looking_row) = match state {
        CameraState::Live | CameraState::SavingFrames => {
            let share =
                if governor.set_rate > 0.0 { governor.rate / governor.set_rate } else { 0.0 };
            match lowered_at.filter(|_| governor.rate < governor.set_rate) {
                Some((from, at)) => (
                    MeterView {
                        label: "Looking".into(),
                        value: format!("{} a second", rate(governor.rate)),
                        percent: percent(share),
                        tone: Tone::Warn,
                        note: Some(format!("Lowered from {} at {}", rate(from), hh_mm(at, offset))),
                    },
                    row(
                        "Looking",
                        format!("{} a second (was {})", rate(governor.rate), rate(from)),
                        Tone::Warn,
                    ),
                ),
                None => (
                    MeterView {
                        label: "Looking".into(),
                        value: format!("{} a second", rate(governor.rate)),
                        percent: percent(share),
                        tone: Tone::Ok,
                        note: Some("As set".into()),
                    },
                    row("Looking", format!("{} a second", rate(governor.rate)), Tone::Plain),
                ),
            }
        }
        CameraState::Paused => {
            let since = match &facts.state {
                StreamState::Paused { since } => hh_mm(*since, offset),
                StreamState::Live | StreamState::Lost { .. } => String::new(),
            };
            (
                MeterView {
                    label: "Looking".into(),
                    value: "Paused".into(),
                    percent: 0,
                    tone: Tone::Fail,
                    note: Some(format!("Since {since}")),
                },
                row("Looking", format!("Paused since {since}"), Tone::Fail),
            )
        }
        CameraState::Lost => {
            let since = match &facts.state {
                StreamState::Lost { since, .. } => *since,
                StreamState::Live | StreamState::Paused { .. } => ctx.now,
            };
            (
                MeterView {
                    label: "Looking".into(),
                    value: "No frames".into(),
                    percent: 0,
                    tone: Tone::Ok,
                    note: Some(format!("Since {}", hh_mm_ss(since, offset))),
                },
                row("Looking", format!("No frames since {}", hh_mm(since, offset)), Tone::Fail),
            )
        }
        CameraState::Off => {
            let since = camera.gate.switched.as_ref().map(|acted| hh_mm(acted.at, offset));
            (
                MeterView {
                    label: "Looking".into(),
                    value: "Off".into(),
                    percent: 0,
                    tone: Tone::Ok,
                    note: Some("No frames while the camera is off".into()),
                },
                row(
                    "Looking",
                    since.map_or_else(|| "Off".into(), |since| format!("Off since {since}")),
                    Tone::Plain,
                ),
            )
        }
        CameraState::Frozen => (
            MeterView {
                label: "Looking".into(),
                value: "Paused".into(),
                percent: 0,
                tone: Tone::Fail,
                note: Some("Stop all paused every camera".into()),
            },
            row("Looking", "Paused by Stop all".into(), Tone::Fail),
        ),
        CameraState::Stopped => (
            MeterView {
                label: "Looking".into(),
                value: "Stopped".into(),
                percent: 0,
                tone: Tone::Ok,
                note: Some("No frames since the stream stopped".into()),
            },
            row("Looking", "Stopped".into(), Tone::Plain),
        ),
    };
    meters.push(looking);
    rows.push(looking_row);

    // Asking the cloud about unclear tracks.
    if let Some(escalation) = &governor.escalation {
        let label = format!("Asking {} about unclear tracks", escalation.backend);
        let short = format!("Asking {}", escalation.backend);
        let (meter, phone) = match (state, escalation.stopped_at) {
            (CameraState::Live | CameraState::SavingFrames, Some(stopped)) => (
                MeterView {
                    label,
                    value: format!("Stopped at {}", hh_mm(stopped, offset)),
                    percent: 0,
                    tone: Tone::Warn,
                    note: Some("Resumes when the budget renews".into()),
                },
                Some(row(&short, format!("Stopped at {}", hh_mm(stopped, offset)), Tone::Warn)),
            ),
            (CameraState::Paused, Some(stopped)) => (
                MeterView {
                    label,
                    value: format!("Stopped at {}", hh_mm(stopped, offset)),
                    percent: 0,
                    tone: Tone::Fail,
                    note: None,
                },
                None,
            ),
            (CameraState::Live | CameraState::SavingFrames, None) => (
                MeterView {
                    label,
                    value: format!("{} in the last minute", escalation.last_minute),
                    percent: percent(if escalation.max_per_minute > 0 {
                        f64::from(escalation.last_minute) / f64::from(escalation.max_per_minute)
                    } else {
                        0.0
                    }),
                    tone: Tone::Ok,
                    note: Some(format!(
                        "At most {} a minute · {} today · ${:.2}",
                        escalation.max_per_minute,
                        escalation.today,
                        escalation.cost_today_micros as f64 / 1_000_000.0
                    )),
                },
                Some(row(
                    &short,
                    format!(
                        "{} a minute · at most {}",
                        escalation.last_minute, escalation.max_per_minute
                    ),
                    Tone::Plain,
                )),
            ),
            (CameraState::Lost, _) => (
                MeterView {
                    label,
                    value: "0 in the last minute".into(),
                    percent: 0,
                    tone: Tone::Ok,
                    note: None,
                },
                None,
            ),
            (CameraState::Paused, None)
            | (CameraState::Off | CameraState::Stopped | CameraState::Frozen, _) => (
                MeterView { label, value: "0".into(), percent: 0, tone: Tone::Ok, note: None },
                None,
            ),
        };
        meters.push(meter);
        rows.extend(phone);
    }

    // Frames today.
    let used = governor.used();
    let frames_tone = if used >= 1.0 {
        Tone::Fail
    } else if used >= governor.look_less_at {
        Tone::Warn
    } else {
        Tone::Ok
    };
    let frames_value =
        format!("{} of {}", thousands(governor.frames_today), thousands(governor.frames_limit));
    let frames_note = match state {
        CameraState::Live => Some(format!("Renews at {until}")),
        CameraState::SavingFrames => {
            Some(format!("{}% · at 100% the camera pauses until {until}", percent(used)))
        }
        CameraState::Paused => Some(format!("Renews at {until}")),
        CameraState::Lost | CameraState::Off | CameraState::Stopped | CameraState::Frozen => None,
    };
    meters.push(MeterView {
        label: "Frames today".into(),
        value: frames_value.clone(),
        percent: percent(used),
        tone: frames_tone,
        note: frames_note,
    });
    rows.push(row(
        "Frames today",
        frames_value,
        match (state, frames_tone) {
            (CameraState::SavingFrames | CameraState::Paused, tone) => tone,
            _ => Tone::Plain,
        },
    ));
    if state == CameraState::Paused {
        rows.push(row("Renews", format!("At {until}"), Tone::Plain));
    }

    // What the budget did.
    let paused = state == CameraState::Paused;
    let backend = governor.escalation.as_ref().map(|escalation| escalation.backend.as_str());
    let mut steps: Vec<StepView> = match state {
        CameraState::SavingFrames | CameraState::Paused => governor
            .steps
            .iter()
            .map(|step| {
                let used = step
                    .used
                    .filter(|_| !paused)
                    .map(|used| format!(" ({}% used)", percent(used)))
                    .unwrap_or_default();
                let text = match &step.kind {
                    GovernorStepKind::LookLessOften { from, to } => {
                        format!("Looked less often: {} → {} a second{used}", rate(*from), rate(*to))
                    }
                    GovernorStepKind::StopAsking => {
                        format!("Stopped asking {}{used}", backend.unwrap_or("the cloud"))
                    }
                    GovernorStepKind::Pause => format!("Paused the camera until {until}"),
                };
                StepView { at: hh_mm(step.at, offset), text, next: false }
            })
            .collect(),
        CameraState::Live
        | CameraState::Lost
        | CameraState::Off
        | CameraState::Stopped
        | CameraState::Frozen => Vec::new(),
    };
    if state == CameraState::SavingFrames {
        let asked = governor.steps.iter().any(|step| step.kind == GovernorStepKind::StopAsking);
        let next = match (governor.min_rate, backend) {
            // A governor that steps by what the frames left can last: lower to its floor, then
            // stop asking, then pause.
            (Some(floor), _) if governor.rate > floor + 1e-6 => {
                format!("Look less often, down to {} a second", rate(floor))
            }
            (Some(_), Some(backend)) if !asked => format!("Stop asking {backend}"),
            (None, Some(backend)) if !asked => {
                format!("At {}%: stop asking {backend}", percent(governor.stop_asking_at))
            }
            _ => format!("At 100%: pause the camera until {until}"),
        };
        steps.push(StepView { at: "next".into(), text: next, next: true });
    }
    let order = (state == CameraState::Live).then(|| match backend {
        Some(backend) => format!(
            "Steps down in this order: look less often, then stop asking {backend}, then pause the camera until the budget renews at {until}."
        ),
        None => format!(
            "Steps down in this order: look less often, then pause the camera until the budget renews at {until}."
        ),
    });
    BudgetView {
        scope: format!("for {}", facts.place),
        meters,
        order,
        steps,
        limit: paused.then(|| "Change the daily frame limit".into()),
        rows,
    }
}

pub(crate) fn tracks(camera: &Camera<'_>) -> (Vec<TrackRow>, String, Option<String>) {
    let facts = camera.facts;
    let note = match camera.state() {
        CameraState::Live | CameraState::SavingFrames => {
            "Tracks are anonymous and start again when the camera does."
        }
        CameraState::Paused => "No tracks while paused.",
        CameraState::Lost => "Lost with the camera. Tracks start again when it is back.",
        CameraState::Off => "None while the camera is off. Tracks start again when it is back.",
        CameraState::Stopped => "None since the stream stopped.",
        CameraState::Frozen => "None while Stop all is in force.",
    };
    let short = (camera.state() == CameraState::Off)
        .then(|| "Tracks start again when the camera is back on.".to_string());
    if !camera.watching() {
        return (Vec::new(), note.into(), short);
    }
    let mut rows: Vec<TrackRow> = facts
        .tracks
        .iter()
        .map(|track| {
            let phase = match track.phase {
                TrackPhase::Entering => "entering",
                TrackPhase::InZone => "in the zone",
                TrackPhase::Leaving => "leaving",
                TrackPhase::InView => "in view",
            };
            let finding = track.finding.as_ref();
            let found = finding.map(|finding| format!("{} {:.2}", finding.label, finding.score));
            let mut note: Vec<String> = found.iter().cloned().collect();
            note.push(phase.into());
            let short = note.join(" · ");
            if let Some(finding) = finding {
                note.push(format!("{} frames", finding.frames));
            }
            TrackRow {
                id: format!("T{}", track.id),
                label: track.label.clone(),
                note: note.join(" · "),
                short_note: short,
                swatch: finding
                    .map_or_else(|| track.label.clone(), |finding| finding.label.clone()),
                finding: finding.is_some_and(|finding| is_finding(&finding.label, facts)),
            }
        })
        .collect();
    rows.sort_by_key(|row| !row.finding);
    (rows, note.into(), short)
}

// ---------------------------------------------------------------------------
// Several cameras
// ---------------------------------------------------------------------------

pub(crate) fn source_tile(camera: &Camera<'_>, selected: bool, ctx: &Ctx<'_>) -> SourceTile {
    let state = camera.state();
    SourceTile {
        id: camera.facts.id.clone(),
        name: camera.facts.name.clone(),
        state,
        state_text: short_state(state).into(),
        tone: tone(state),
        selected,
        stage: stage(camera, Place::Strip, ctx),
    }
}

pub(crate) fn camera_tile(camera: &Camera<'_>, selected: bool, ctx: &Ctx<'_>) -> CameraTile {
    let facts = camera.facts;
    let state = camera.state();
    let watched = !facts.watches.is_empty();
    let governor = &facts.governor;
    let caption = match (&facts.from, watched) {
        (Some(from), _) => from.clone(),
        (None, true) => format!(
            "watching for {}",
            facts
                .watches
                .iter()
                .map(|watch| format!("{}s", watch.label))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        (None, false) => "shown only, nothing watched".into(),
    };
    let line = match state {
        CameraState::Live if watched => format!(
            "{} a second · {}% of today's frames",
            rate(governor.rate),
            percent(governor.used())
        ),
        CameraState::Live => format!("{} a second · nothing watched", rate(governor.rate)),
        CameraState::SavingFrames => {
            format!("{} a second · {}% used", rate(governor.rate), percent(governor.used()))
        }
        CameraState::Paused => format!("paused until {}", renews(governor.renews_at, ctx.offset)),
        CameraState::Lost => "can't reach it".into(),
        CameraState::Off => "nothing captured".into(),
        CameraState::Stopped => "stream stopped".into(),
        CameraState::Frozen => "paused by Stop all".into(),
    };
    let short_line = match state {
        CameraState::Live if watched => "watching".into(),
        CameraState::Live => "shown only".into(),
        CameraState::SavingFrames => format!("{} a second", rate(governor.rate)),
        CameraState::Paused => "paused".into(),
        CameraState::Lost => "can't reach it".into(),
        CameraState::Off | CameraState::Stopped => String::new(),
        CameraState::Frozen => "paused".into(),
    };
    CameraTile {
        id: facts.id.clone(),
        name: facts.name.clone(),
        state,
        state_text: short_state(state).into(),
        tone: tone(state),
        on: camera.gate.on && camera.gate.stopped.is_none(),
        can_switch: camera.gate.stopped.is_none(),
        stage: stage(camera, Place::Tile { selected }, ctx),
        caption,
        line,
        short_line,
    }
}

/// One camera's line in the grid's budget: "Line 3 · frames today".
pub(crate) fn tile_meter(camera: &Camera<'_>, ctx: &Ctx<'_>) -> Option<MeterView> {
    let facts = camera.facts;
    let governor: &GovernorFacts = &facts.governor;
    if governor.frames_limit == 0 {
        return None;
    }
    let used = governor.used();
    let state = camera.state();
    let note = (state == CameraState::SavingFrames).then(|| {
        let mut parts = vec![format!("Looking {} a second", rate(governor.rate))];
        if let Some(escalation) =
            governor.escalation.as_ref().filter(|escalation| escalation.stopped_at.is_some())
        {
            parts.push(format!("not asking {}", escalation.backend));
        }
        parts.join(" · ")
    });
    let _ = ctx;
    Some(MeterView {
        label: format!("{} · frames today", facts.name.trim_end_matches(" camera")),
        value: format!(
            "{} of {}",
            thousands(governor.frames_today),
            thousands(governor.frames_limit)
        ),
        percent: percent(used),
        tone: if used >= 1.0 {
            Tone::Fail
        } else if used >= governor.look_less_at {
            Tone::Warn
        } else {
            Tone::Ok
        },
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_as_the_boards_write_them() {
        assert_eq!(thousands(61_240), "61,240");
        assert_eq!(thousands(300_000), "300,000");
        assert_eq!(thousands(980), "980");
        assert_eq!(rate(5.0), "5");
        assert_eq!(rate(0.5), "0.5");
        assert_eq!(percent(0.204), 20);
        assert_eq!(percent(0.958), 96);
        assert_eq!(corners_text([700.0, 200.0, 1600.0, 880.0]), "700, 200 → 1600, 880");
        assert_eq!(capital("line 2"), "Line 2");
        assert_eq!(
            frame_href("line-2", "sha256:ab"),
            "/api/surface/cameras/line-2/frames/sha256%3Aab"
        );
    }
}
