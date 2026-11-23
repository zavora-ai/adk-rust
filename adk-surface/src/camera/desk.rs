//! The live camera surface on the engine (spec 3.7 §8.2): reads authorised as the inbox is —
//! a camera belongs to its agent's session, and only that session's owner sees or acts on it
//! (spec 3.3 §11 S-I8) — and every act recorded.

use chrono::Duration;
use serde_json::json;

use super::facts::{CameraFacts, ZoneFacts};
use super::hub::Cameras;
use super::hub::{DropReason, Offered};
use super::project::{self, Camera, Ctx, Place};
use super::view::{
    CameraAgent, CameraView, CamerasView, CardRaised, FrameDropped, FrameTaken, RaiseCard,
    StageView, ZoneChange, ZoneSaved,
};
use super::{
    SURFACE_AUDIT_CAMERA_CARD, SURFACE_AUDIT_CAMERA_RETRY, SURFACE_AUDIT_CAMERA_STOP,
    SURFACE_AUDIT_CAMERA_SWITCH, SURFACE_AUDIT_CAMERA_ZONE,
};
use crate::engine::SurfaceEngine;
use crate::error::{Result, SurfaceError};
use crate::evidence::{EVIDENCE_RETENTION_DAYS, EvidenceBytes};
use crate::origin::{Caller, Origin};

/// The words "Nothing is waiting for you." under Decisions.
const NOTHING_WAITING: &str = "Nothing is waiting for you.";

/// The largest frame a screen may send a camera: a 1280 × 720 JPEG is about a tenth of it.
pub const MAX_SENT_FRAME_BYTES: usize = 2 * 1024 * 1024;

impl SurfaceEngine {
    fn hub(&self) -> Result<&Cameras> {
        self.camera_hub().ok_or_else(|| SurfaceError::unsupported("the live camera surface"))
    }

    /// The cameras `caller` may see: those of their own sessions.
    async fn visible_cameras(&self, caller: &Caller) -> Result<Vec<CameraFacts>> {
        let mut visible = Vec::new();
        for facts in self.hub()?.facts().await? {
            if self.owns_session(caller, &facts.session).await.is_ok() {
                visible.push(facts);
            }
        }
        Ok(visible)
    }

    /// One camera of `caller`'s, `surface.not_found` for any other.
    async fn owned_camera(&self, caller: &Caller, camera: &str) -> Result<CameraFacts> {
        self.visible_cameras(caller)
            .await?
            .into_iter()
            .find(|facts| facts.id == camera)
            .ok_or_else(|| SurfaceError::not_found(format!("camera '{camera}'")))
    }

    fn camera_ctx<'a>(&self, caller: &'a Caller, agent: &str) -> Ctx<'a> {
        Ctx {
            offset: self.projector().offset,
            now: self.clock_now(),
            user: caller.user_id(),
            agent_name: self.projector().agent(agent).display_name,
        }
    }

    async fn with_gates<'a>(&self, facts: &'a [CameraFacts]) -> Result<Vec<Camera<'a>>> {
        let hub = self.hub()?;
        let mut cameras = Vec::with_capacity(facts.len());
        for camera in facts {
            let gate = hub.gate(&camera.id).await.unwrap_or(super::hub::Gate {
                on: camera.on,
                switched: None,
                stopped: None,
                frozen: hub.is_frozen(),
            });
            cameras.push(Camera {
                facts: camera,
                gate,
                recent: hub.recent(&camera.id).await,
                takes_frames: hub.takes_frames(&camera.id),
            });
        }
        Ok(cameras)
    }

    fn camera_agent(&self, camera: &Camera<'_>) -> CameraAgent {
        let (line, tone) = project::agent_line(camera);
        CameraAgent {
            agent: self.projector().agent(&camera.facts.agent),
            line,
            tone,
            session_id: camera.facts.session.clone(),
        }
    }

    /// Every camera `caller` may see, as the grid shows them (spec 3.7 §8.2, board choice C8).
    ///
    /// # Errors
    ///
    /// `surface.unsupported` when the engine serves no cameras; the source's error.
    pub async fn cameras_view(&self, caller: &Caller) -> Result<CamerasView> {
        let facts = self.visible_cameras(caller).await?;
        let cameras = self.with_gates(&facts).await?;
        let first = cameras.first();
        let agent = first.map(|camera| {
            let mut agent = self.camera_agent(camera);
            let watched = cameras
                .iter()
                .filter(|other| {
                    other.facts.agent == camera.facts.agent && !other.facts.watches.is_empty()
                })
                .count();
            if watched > 1 {
                agent.line = format!("{} · {watched} cameras", agent.line);
            }
            agent
        });
        let agent_name = first.map_or_else(String::new, |camera| camera.facts.agent.clone());
        let ctx = self.camera_ctx(caller, &agent_name);
        let on =
            cameras.iter().filter(|camera| camera.gate.on && camera.gate.stopped.is_none()).count();
        let mut decisions = Vec::new();
        let mut sessions: Vec<&str> = facts.iter().map(|facts| facts.session.as_str()).collect();
        sessions.dedup();
        for session in sessions {
            decisions.extend(self.session_decisions(caller, session).await?);
        }
        let selected_line = first.map(|camera| {
            let state = camera.state();
            let mut line = format!(
                "{} · {}",
                camera.facts.name,
                project::state_text(state, camera.facts, ctx.offset)
            );
            if matches!(
                state,
                super::view::CameraState::Live | super::view::CameraState::SavingFrames
            ) {
                line.push_str(&format!(" · {} a second", rate_text(camera.facts.governor.rate)));
            }
            line
        });
        Ok(CamerasView {
            title: "Cameras".into(),
            count_text: format!("{} sources · {on} on", cameras.len()),
            count_short: format!("{} · {on} on", cameras.len()),
            agent,
            selected: first.map(|camera| camera.facts.id.clone()),
            selected_line,
            tiles: cameras
                .iter()
                .enumerate()
                .map(|(index, camera)| project::camera_tile(camera, index == 0, &ctx))
                .collect(),
            note: "Each source has its own frame budget. Stop all pauses every one of them.".into(),
            short_note: "Each camera has its own frame budget. People are blurred before any frame is shown or kept.".into(),
            footer: "Select a camera to watch it large. People are blurred before any frame is shown or kept.".into(),
            decisions,
            nothing_waiting: NOTHING_WAITING.into(),
            budget: cameras.iter().filter_map(|camera| project::tile_meter(camera, &ctx)).collect(),
        })
    }

    /// Camera `camera`, watched large (spec 3.7 §8.2, board choice C1): the picture with its
    /// overlays, the agent's decisions, what it watches for, the frame budget, the tracks and
    /// every source.
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the camera is not one of `caller`'s; `surface.unsupported` when
    /// the engine serves no cameras; the source's error.
    pub async fn camera_view(&self, caller: &Caller, camera: &str) -> Result<CameraView> {
        let facts = self.visible_cameras(caller).await?;
        if !facts.iter().any(|facts| facts.id == camera) {
            return Err(SurfaceError::not_found(format!("camera '{camera}'")));
        }
        let cameras = self.with_gates(&facts).await?;
        let Some(shown) = cameras.iter().find(|candidate| candidate.facts.id == camera) else {
            return Err(SurfaceError::not_found(format!("camera '{camera}'")));
        };
        let ctx = self.camera_ctx(caller, &shown.facts.agent);
        let state = shown.state();
        let (tracks, tracks_note, tracks_short_note) = project::tracks(shown);
        Ok(CameraView {
            id: shown.facts.id.clone(),
            name: shown.facts.name.clone(),
            ingest: project::ingest(shown.facts),
            state,
            state_text: project::state_text(state, shown.facts, ctx.offset),
            tone: project::tone(state),
            // The person's switch: Stop all pauses a camera without switching it off.
            on: shown.gate.on && shown.gate.stopped.is_none(),
            agent: self.camera_agent(shown),
            stage: project::stage(shown, Place::Main, &ctx),
            notice: project::notice(shown, &ctx),
            actions: project::actions(shown),
            decisions: self.session_decisions(caller, &shown.facts.session).await?,
            nothing_waiting: NOTHING_WAITING.into(),
            watches: project::watches(shown, &ctx),
            budget: project::budget(shown, &ctx),
            tracks,
            tracks_note,
            tracks_short_note,
            zones: project::zones(shown.facts),
            sources: cameras
                .iter()
                .map(|other| project::source_tile(other, other.facts.id == camera, &ctx))
                .collect(),
        })
    }

    /// Camera `camera`'s picture alone, as its feed sends it after each frame.
    ///
    /// # Errors
    ///
    /// As [`camera_view`](Self::camera_view).
    pub async fn camera_stage(&self, caller: &Caller, camera: &str) -> Result<StageView> {
        let facts = self.owned_camera(caller, camera).await?;
        self.stage_of(caller, facts).await
    }

    /// Camera `camera`'s picture for a feed that already checked the caller may see it: the
    /// sessions are not read again for every frame.
    pub(crate) async fn camera_stage_after_check(
        &self,
        caller: &Caller,
        camera: &str,
    ) -> Result<StageView> {
        let facts = self
            .hub()?
            .facts()
            .await?
            .into_iter()
            .find(|facts| facts.id == camera)
            .ok_or_else(|| SurfaceError::not_found(format!("camera '{camera}'")))?;
        self.stage_of(caller, facts).await
    }

    async fn stage_of(&self, caller: &Caller, facts: CameraFacts) -> Result<StageView> {
        let shown = self.with_gates(std::slice::from_ref(&facts)).await?;
        let ctx = self.camera_ctx(caller, &facts.agent);
        Ok(project::stage(&shown[0], Place::Main, &ctx))
    }

    /// Freezes frame `frame` of `camera` for a card (board choice C11): the hub holds it in
    /// memory while the person writes what the agent should look at. Returns the picture of that
    /// frame with what was found on it. Nothing is written.
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the camera is not `caller`'s or the frame is no longer held;
    /// `surface.not_here` from anything but a person at a screen.
    pub async fn freeze_camera_frame(
        &self,
        caller: &Caller,
        camera: &str,
        frame: &str,
    ) -> Result<StageView> {
        at_a_screen(caller)?;
        let facts = self.owned_camera(caller, camera).await?;
        let recent = self.hub()?.freeze(camera, frame, self.clock_now()).await.ok_or_else(|| {
            SurfaceError::not_found(format!(
                "frame '{frame}' of camera '{camera}': it is no longer held; freeze the picture again"
            ))
        })?;
        let mut shown = self.with_gates(std::slice::from_ref(&facts)).await?;
        shown[0].recent = recent;
        let ctx = self.camera_ctx(caller, &facts.agent);
        Ok(project::stage(&shown[0], Place::Main, &ctx))
    }

    /// The frame of `camera` with `digest`, while the hub holds it in memory, its bytes checked
    /// against the digest (spec 3.7 §8.3). A camera that is off holds none.
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the camera is not `caller`'s or the frame is no longer held.
    pub async fn camera_frame(
        &self,
        caller: &Caller,
        camera: &str,
        digest: &str,
    ) -> Result<EvidenceBytes> {
        self.owned_camera(caller, camera).await?;
        let held = self
            .hub()?
            .held(camera, digest)
            .await
            .filter(|held| crate::evidence::digest_of(&held.frame.bytes) == digest)
            .ok_or_else(|| {
                SurfaceError::not_found(format!("frame '{digest}' of camera '{camera}'"))
            })?;
        Ok(EvidenceBytes { mime_type: held.frame.mime_type, bytes: held.frame.bytes })
    }

    /// Switches `camera` on or off for `caller` (board choice C2). Off closes the hub's gate
    /// before anything else, so the server drops every frame from then on, and forgets the
    /// frames it held; then it is recorded and the source told. On is recorded first.
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the camera is not `caller`'s; `surface.not_here` from anything
    /// but a person at a screen; the source's error.
    pub async fn switch_camera(
        &self,
        caller: &Caller,
        camera: &str,
        on: bool,
    ) -> Result<CameraView> {
        at_a_screen(caller)?;
        let facts = self.owned_camera(caller, camera).await?;
        let hub = self.hub()?;
        let fields = json!({ "camera": camera, "sessionId": facts.session, "on": on });
        if on {
            self.record(caller, SURFACE_AUDIT_CAMERA_SWITCH, fields).await?;
            hub.switch(camera, true, caller.user_id(), self.clock_now()).await?;
        } else {
            let switched = hub.switch(camera, false, caller.user_id(), self.clock_now()).await;
            self.record(caller, SURFACE_AUDIT_CAMERA_SWITCH, fields).await?;
            switched?;
        }
        self.camera_view(caller, camera).await
    }

    /// Ends `camera`'s stream for `caller` (board choice C2): the gate closes for good, then it
    /// is recorded and the source told.
    ///
    /// # Errors
    ///
    /// As [`switch_camera`](Self::switch_camera).
    pub async fn stop_camera(&self, caller: &Caller, camera: &str) -> Result<CameraView> {
        at_a_screen(caller)?;
        let facts = self.owned_camera(caller, camera).await?;
        let stopped = self.hub()?.stop(camera, caller.user_id(), self.clock_now()).await;
        self.record(
            caller,
            SURFACE_AUDIT_CAMERA_STOP,
            json!({ "camera": camera, "sessionId": facts.session }),
        )
        .await?;
        stopped?;
        self.camera_view(caller, camera).await
    }

    /// Takes one frame `caller`'s screen captured for `camera` — the web home's or the phone's
    /// own camera — and hands it to the camera's frame pipeline through the hub's gate
    /// ([`Cameras::ingest`]): while the camera is off, stopped or paused by Stop all the frame is
    /// dropped and the answer says why, so the screen stops capturing. A camera switched off
    /// sends nothing. Nothing is held or written here; the pipeline shows what it makes of the
    /// frame. Frames are not recorded one by one: switching the camera is.
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the camera is not `caller`'s; `surface.not_here` from anything
    /// but a person at a screen; `surface.invalid` for an empty frame, one over
    /// [`MAX_SENT_FRAME_BYTES`], or one that is not the JPEG, PNG or WebP `mime_type` says;
    /// `surface.unsupported` for a camera that reads its own source; `surface.rate_limited`
    /// for a frame sooner than [`MAX_SCREEN_FPS`](super::MAX_SCREEN_FPS) allows.
    pub async fn send_camera_frame(
        &self,
        caller: &Caller,
        camera: &str,
        mime_type: &str,
        bytes: Vec<u8>,
    ) -> Result<FrameTaken> {
        at_a_screen(caller)?;
        if bytes.is_empty() || bytes.len() > MAX_SENT_FRAME_BYTES {
            return Err(SurfaceError::Invalid(format!(
                "a camera frame is 1 byte to {} MiB; this one is {} bytes",
                MAX_SENT_FRAME_BYTES / (1024 * 1024),
                bytes.len()
            )));
        }
        let mime = mime_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
        let matches = match mime.as_str() {
            "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
            "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
            _ => {
                return Err(SurfaceError::Invalid(format!(
                    "a camera frame is image/jpeg, image/png or image/webp, not '{mime_type}'"
                )));
            }
        };
        if !matches {
            return Err(SurfaceError::Invalid(format!(
                "the frame's bytes are not the {mime} its content type says"
            )));
        }
        self.owned_camera(caller, camera).await?;
        let hub = self.hub()?;
        if !hub.takes_frames(camera) {
            return Err(SurfaceError::unsupported(format!(
                "sending frames to camera '{camera}', which reads its own source"
            )));
        }
        let dropped = match hub.ingest(camera, bytes, self.clock_now()).await {
            Offered::Forwarded | Offered::Shown { .. } => None,
            Offered::Dropped(DropReason::Off) => Some(FrameDropped::Off),
            Offered::Dropped(DropReason::Stopped) => Some(FrameDropped::Stopped),
            Offered::Dropped(DropReason::Frozen) => Some(FrameDropped::Frozen),
            Offered::Dropped(DropReason::Unknown) => Some(FrameDropped::NotReading),
            Offered::Dropped(DropReason::TooSoon) => {
                return Err(SurfaceError::Source(adk_core::AdkError::new(
                    adk_core::ErrorComponent::Server,
                    adk_core::ErrorCategory::RateLimited,
                    "surface.rate_limited",
                    format!(
                        "camera '{camera}' takes at most {} frames a second from a screen; send them further apart",
                        super::MAX_SCREEN_FPS
                    ),
                )));
            }
        };
        Ok(FrameTaken { taken: dropped.is_none(), dropped })
    }

    /// Tries a lost `camera` again now (board choice C6).
    ///
    /// # Errors
    ///
    /// As [`switch_camera`](Self::switch_camera); `surface.unsupported` when the source cannot.
    pub async fn retry_camera(&self, caller: &Caller, camera: &str) -> Result<CameraView> {
        at_a_screen(caller)?;
        let facts = self.owned_camera(caller, camera).await?;
        self.record(
            caller,
            SURFACE_AUDIT_CAMERA_RETRY,
            json!({ "camera": camera, "sessionId": facts.session }),
        )
        .await?;
        self.hub()?.source().retry(camera).await?;
        self.hub()?.changed(camera);
        self.camera_view(caller, camera).await
    }

    /// Changes zone `zone` of `camera` (board choice C12): every watch that uses it reads the
    /// new corners from its next frame. A zone changed since the person started is refused,
    /// never overwritten. Recorded before it is saved.
    ///
    /// # Errors
    ///
    /// `surface.not_found` for another's camera or an unknown zone; `surface.invalid` for corners
    /// outside the frame, an empty name, or a zone that changed; `surface.not_here` from a
    /// paired device or anything but a person at a screen; the source's error.
    pub async fn save_camera_zone(
        &self,
        caller: &Caller,
        camera: &str,
        zone: &str,
        change: &ZoneChange,
    ) -> Result<ZoneSaved> {
        at_a_screen(caller)?;
        if caller.device.is_some() {
            return Err(SurfaceError::NotHere(
                "edit a zone in the web home; a phone shows the zone and opens it there".into(),
            ));
        }
        let facts = self.owned_camera(caller, camera).await?;
        let current =
            facts.zones.iter().find(|candidate| candidate.id == zone).ok_or_else(|| {
                SurfaceError::not_found(format!("zone '{zone}' of camera '{camera}'"))
            })?;
        let name = change.name.trim();
        if name.is_empty() || name.chars().count() > 60 {
            return Err(SurfaceError::Invalid("a zone needs a name of 1 to 60 characters".into()));
        }
        let [x0, y0, x1, y1] = change.corners;
        let (width, height) = (f64::from(facts.ingest.width), f64::from(facts.ingest.height));
        let inside = change.corners.iter().all(|value| value.is_finite())
            && x0 >= 0.0
            && y0 >= 0.0
            && x1 <= width
            && y1 <= height
            && x1 - x0 >= 8.0
            && y1 - y0 >= 8.0;
        if !inside {
            return Err(SurfaceError::Invalid(format!(
                "a zone's corners must lie inside the camera's {} × {} pixels, top left before bottom right",
                facts.ingest.width, facts.ingest.height
            )));
        }
        if change.was.iter().zip(current.corners).any(|(was, now)| (was - now).abs() > 0.5) {
            return Err(SurfaceError::Invalid(
                "the zone changed since you started editing it; open it again to see it as it is now"
                    .into(),
            ));
        }
        let corners = change.corners.map(f64::round);
        self.record(
            caller,
            SURFACE_AUDIT_CAMERA_ZONE,
            json!({
                "camera": camera,
                "sessionId": facts.session,
                "zone": zone,
                "name": name,
                "corners": corners,
                "was": current.corners,
            }),
        )
        .await?;
        let saved = ZoneFacts { id: zone.to_string(), name: name.to_string(), corners };
        self.hub()?.source().save_zone(camera, &saved).await?;
        self.hub()?.changed(camera);
        let mut facts = facts;
        if let Some(entry) = facts.zones.iter_mut().find(|candidate| candidate.id == zone) {
            *entry = saved;
        }
        let view = project::zones(&facts).into_iter().find(|candidate| candidate.id == zone);
        let view = view.ok_or_else(|| SurfaceError::not_found(format!("zone '{zone}'")))?;
        let text = match view.used_by.as_slice() {
            [] => "Saved.".to_string(),
            [one] => format!("Saved. The {} uses it from the next frame.", one.to_lowercase()),
            several => format!("Saved. {} use it from the next frame.", several.join(", ")),
        };
        Ok(ZoneSaved { zone: view, text })
    }

    /// Raises a card from a frozen frame of `camera` (board choice C11): that one frame is kept
    /// as evidence — blurred first by the source, kept
    /// [`EVIDENCE_RETENTION_DAYS`] days — with the regions on it, and the card goes to the
    /// watching agent as `caller`'s message, naming the evidence by digest and artifact, never
    /// carrying pixels (spec 3.7 V-I15). Nothing else is stored. Recorded before the frame is
    /// kept.
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the camera is not `caller`'s or the hub no longer holds the
    /// frame; `surface.invalid` for an empty note; `surface.not_here` from anything but a person
    /// at a screen; the source's error, and nothing is kept then.
    pub async fn raise_camera_card(
        &self,
        caller: &Caller,
        camera: &str,
        card: &RaiseCard,
    ) -> Result<CardRaised> {
        at_a_screen(caller)?;
        let note = card.note.trim();
        if note.is_empty() {
            return Err(SurfaceError::Invalid(
                "say what the agent should look at on the frame".into(),
            ));
        }
        let facts = self.owned_camera(caller, camera).await?;
        let held = self.hub()?.held(camera, &card.frame).await.ok_or_else(|| {
            SurfaceError::not_found(format!(
                "frame '{}' of camera '{camera}': it is no longer held; freeze the picture again",
                card.frame
            ))
        })?;
        let regions: Vec<_> =
            held.frame.overlays.iter().filter(|overlay| overlay.bbox.is_some()).collect();
        self.record(
            caller,
            SURFACE_AUDIT_CAMERA_CARD,
            json!({
                "camera": camera,
                "sessionId": facts.session,
                "frame": held.digest,
                "regions": regions.len(),
                "noteDigest": crate::evidence::digest_of(note.as_bytes()),
            }),
        )
        .await?;
        let evidence = self.hub()?.source().keep_frame(&facts, &held).await?;
        let offset = self.projector().offset;
        let kept_until = (self.clock_now() + Duration::days(i64::from(EVIDENCE_RETENTION_DAYS)))
            .with_timezone(&offset)
            .format("%-d %b")
            .to_string();
        let lines: Vec<String> = regions
            .iter()
            .filter_map(|overlay| {
                let [x0, y0, x1, y1] = overlay.bbox?.map(|value| value.round() as i64);
                let mut name = overlay.label.clone();
                if let Some(track) = overlay.track {
                    name.push_str(&format!(" T{track}"));
                }
                if let Some(score) = overlay.score {
                    name.push_str(&format!(" {score:.2}"));
                }
                Some(format!("- {name}: {x0}, {y0} → {x1}, {y1}"))
            })
            .collect();
        let at = held.frame.at.with_timezone(&offset).format("%H:%M:%S");
        let text = format!(
            "Card from the {place} camera, frame of {at}: {note}\n\nThe frame is kept as evidence until {kept_until}: {digest} (artifact {file} v{version}, {width} × {height}, anyone in it blurred). {count} regions on it, in the camera's pixels:\n{lines}",
            place = facts.place,
            digest = evidence.digest,
            file = evidence.artifact.file_name,
            version = evidence.artifact.version,
            width = evidence.width,
            height = evidence.height,
            count = lines.len(),
            lines = lines.join("\n"),
        );
        let outcome = self.send_message(caller, &facts.session, text).await?;
        let agent = self.projector().agent(&facts.agent).display_name;
        // Behind an open decision or a running turn the card waits its turn, as a typed message
        // does (spec 3.3 S-I4).
        let when = match &outcome {
            crate::engine::CommandOutcome::Queued { .. } => {
                format!(
                    "Raised to the {agent}, who reads it once the open decision or turn is done."
                )
            }
            _ => format!("Raised to the {agent}."),
        };
        Ok(CardRaised {
            short_digest: crate::evidence::short_digest(&evidence.digest),
            href: crate::viewer::viewer_path(&facts.session, &evidence.artifact.file_name),
            digest: evidence.digest,
            regions: lines.len(),
            text: format!("{when} This one frame is kept as evidence until {kept_until}."),
            kept_until: format!("Kept as evidence until {kept_until}"),
            outcome,
        })
    }
}

/// Cameras are switched, stopped, edited and carded by a person at a screen; voice and chat
/// have no camera controls (spec 3.7 §8.2: web home and phone).
fn at_a_screen(caller: &Caller) -> Result<()> {
    match caller.origin {
        Origin::Keyboard | Origin::Pointer | Origin::Touch => Ok(()),
        Origin::Voice | Origin::Chat { .. } | Origin::Agent { .. } => Err(SurfaceError::NotHere(
            "cameras are switched and edited from the web home or the phone".into(),
        )),
    }
}

fn rate_text(rate: f64) -> String {
    if (rate - rate.round()).abs() < 1e-6 {
        format!("{}", rate.round() as i64)
    } else {
        format!("{rate:.1}")
    }
}
