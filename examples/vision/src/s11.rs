//! S11, a budget per frame: thirty minutes of two conveyor cameras, replayed at the capture
//! times cameras started at 23:45 UTC would give them, so the stream crosses midnight and the
//! daily quotas renew halfway.
//!
//! | Camera | Budget | What it shows |
//! |---|---|---|
//! | Line 2 | 5 frames a second on the device; 4 escalations a minute, 300 escalated frames a day | Parts whose crack is uncertain for three frames are escalated to `sam-3.1` video, each request through the frame governor first; escalation stops at the quota and resumes at midnight |
//! | Line 3 | 5 frames a second, 600 frames a day | The governor lowers the rate, stops escalating, then pauses the source, and resumes at midnight |
//!
//! `sam-3.1` answers from live responses recorded for each part's clip, served to the real
//! client from a local server. The clips — three seconds of each defective part — are cut
//! from the conveyor clip once by `--write-s11-clips` with a user-installed ffmpeg and
//! committed under `fixtures/conveyor/`, so the scenario needs no encoder; a granted
//! escalation sends the clip of the part its track is on.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::egress::EgressPolicy;
use adk_core::{
    BudgetTracker, InMemorySpendLedger, ModelCallScope, RunBudget, SpendKey, SpendLedger,
    SpendLimits, SpendPeriod,
};
use adk_vision::sam::{SamConfig, SamVideoTracker};
use adk_vision::stream::fixtures::{ConveyorClip, ConveyorSpec};
use adk_vision::stream::{
    EscalationVerdict, FrameBudget, GovernorChange, GovernorHandle, Observation, ObservationSink,
    SourceEvent, StopReason, StreamPipeline,
};
use adk_vision::{
    DetectQuery, EgressAction, EgressConfig, EgressGate, GreedyIouTracker, InMemoryEgressRecorder,
    Sampling, SegmentPrompt, SourceClip, VideoRequest, VideoSource, VisionCx, VisionRuntime,
};
use anyhow::{Context, Result};
use base64::Engine as _;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

use crate::Checks;
use crate::conveyor::{BeltDetector, ClipSource, StreamLog, defect_in};
use crate::fixtures::{fixtures, read_json};
use crate::replay::{live_key, recording_proxy};

const ORG: &str = "line";
/// A crack scored below this is uncertain: the detector's score grows with its length.
const UNCERTAIN: f32 = 0.8;
/// Frames in a row a part's crack stays uncertain before its track is escalated.
const STREAK: u32 = 3;
/// Clip frames in each escalation clip: three seconds at 10 a second.
const CLIP_FRAMES: u64 = 30;
/// The 5-minute clip played six times over.
const LOOPS: u64 = 6;

fn clips_dir() -> PathBuf {
    fixtures().join("conveyor")
}

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 23, 23, 45, 0).single().unwrap_or_else(Utc::now)
}

fn midnight() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 24, 0, 0, 0).single().unwrap_or_else(Utc::now)
}

/// The first clip frame of part `part`'s escalation clip: the part centred in view at its
/// middle frame.
fn clip_start(clip: &ConveyorClip, part: usize) -> u64 {
    let p = &clip.truth().parts[part];
    let middle = (p.enters_ms + p.exits_ms) / 2;
    (middle * u64::from(clip.truth().spec.fps) / 1000).saturating_sub(CLIP_FRAMES / 2)
}

fn clip_file(part: usize) -> PathBuf {
    clips_dir().join(format!("part-{part:03}.mp4"))
}

/// Cuts three seconds of each defective part from the conveyor clip into
/// `fixtures/conveyor/part-NNN.mp4`, H.264 through the user's ffmpeg (`ADK_VISION_FFMPEG`, else
/// `ffmpeg` on `PATH`).
pub fn write_clips() -> Result<()> {
    let clip = ConveyorClip::new(ConveyorSpec::default());
    let ffmpeg = adk_vision::stream::FfmpegBinary::from_env().path().to_path_buf();
    let (w, h) = (clip.truth().spec.width, clip.truth().spec.height);
    std::fs::create_dir_all(clips_dir())?;
    for part in clip.truth().parts.iter().filter(|p| p.defect.is_some()) {
        let file = clip_file(part.index);
        let mut child = Command::new(&ffmpeg)
            .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "rawvideo"])
            .args(["-pix_fmt", "rgba", "-s", &format!("{w}x{h}"), "-r", "10", "-i", "pipe:0"])
            .args(["-c:v", "libx264", "-preset", "veryslow", "-crf", "28", "-pix_fmt", "yuv420p"])
            .args(["-movflags", "+faststart", "-an"])
            .arg(&file)
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| format!("running {}", ffmpeg.display()))?;
        let mut stdin = child.stdin.take().context("ffmpeg's stdin")?;
        let from = clip_start(&clip, part.index);
        for index in from..from + CLIP_FRAMES {
            std::io::Write::write_all(&mut stdin, clip.render(index).rgba())?;
        }
        drop(stdin);
        anyhow::ensure!(child.wait()?.success(), "ffmpeg failed on part {}", part.index);
        println!("wrote {}", file.display());
    }
    Ok(())
}

/// The recorded `sam-3.1` answer for each clip, by the clip's SHA-256.
struct ByClip(BTreeMap<String, (u16, String)>);

impl ByClip {
    fn load(dir: &Path) -> Result<Self> {
        let mut answers = BTreeMap::new();
        if !dir.exists() {
            return Ok(Self(answers));
        }
        for entry in std::fs::read_dir(dir)? {
            let file = entry?.path();
            if file.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let record = read_json(&file)?;
            let video = record["request"]["input"][0]["content"][1]["video_url"]
                .as_str()
                .context("a recording of a video request")?;
            let digest = video.split(":sha256:").nth(1).and_then(|r| r.split(':').next());
            let response = record["response"].as_str().context("a streamed response")?;
            answers.insert(
                digest.context("a recorded clip digest")?.to_string(),
                (record["status"].as_u64().unwrap_or(200) as u16, response.to_string()),
            );
        }
        Ok(Self(answers))
    }
}

impl Respond for ByClip {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let digest = body["input"][0]["content"][1]["video_url"]
            .as_str()
            .and_then(|url| url.split_once(";base64,"))
            .and_then(|(_, data)| base64::engine::general_purpose::STANDARD.decode(data).ok())
            .map(|bytes| adk_vision::Digest::of(&bytes).hex().to_string());
        match digest.and_then(|d| self.0.get(&d)) {
            Some((status, text)) => ResponseTemplate::new(*status)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(text.clone()),
            None => ResponseTemplate::new(404).set_body_string("no recording for this clip"),
        }
    }
}

/// One escalation the line asked for: when, for which part, and the governor's answer.
#[derive(Clone, Debug)]
struct Asked {
    at: DateTime<Utc>,
    part: usize,
    verdict: EscalationVerdict,
    sent: bool,
    /// The request's metered cost and the frames it sent, from its call record.
    cost: Option<u64>,
    frames: u64,
}

/// Line 2's escalation stage: a part whose crack stays uncertain for three frames has its
/// track escalated once — the governor first, then `sam-3.1` video on the part's clip.
struct Escalator {
    clip: Arc<ConveyorClip>,
    governor: OnceLock<GovernorHandle>,
    sam: VisionRuntime,
    log: Arc<StreamLog>,
    streaks: Mutex<BTreeMap<u64, u32>>,
    done: Mutex<BTreeSet<u64>>,
    asked: Mutex<Vec<Asked>>,
    errors: Mutex<Vec<String>>,
}

impl Escalator {
    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait::async_trait]
impl ObservationSink for Escalator {
    async fn observe(&self, cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        self.log.record(&observation);
        let per_loop = self.clip.frame_count();
        let index = observation.frame.index % per_loop;
        let mut due = Vec::new();
        {
            let mut streaks = self.streaks.lock().unwrap_or_else(|e| e.into_inner());
            let done = self.done.lock().unwrap_or_else(|e| e.into_inner());
            for part in observation.regions.iter().filter(|r| r.label.as_deref() == Some("part")) {
                let (Some(track), Some(bbox)) = (part.track, part.shape.bbox()) else { continue };
                let uncertain = observation.regions.iter().any(|r| {
                    r.label.as_deref() == Some("crack")
                        && r.score.is_some_and(|s| s < UNCERTAIN)
                        && r.shape.bbox().is_some_and(|c| c.x0 >= bbox.x0 && c.x1 <= bbox.x1)
                });
                let streak = streaks.entry(track.0).or_default();
                *streak = if uncertain { *streak + 1 } else { 0 };
                if *streak == STREAK && !done.contains(&track.0) {
                    let crack = observation
                        .regions
                        .iter()
                        .filter(|r| r.label.as_deref() == Some("crack"))
                        .filter_map(|r| r.shape.bbox())
                        .find(|c| c.x0 >= bbox.x0 && c.x1 <= bbox.x1);
                    if let Some(part) = crack.and_then(|c| defect_in(&self.clip, index, &c)) {
                        due.push((track.0, part));
                    }
                }
            }
        }
        for (track, part) in due {
            self.done.lock().unwrap_or_else(|e| e.into_inner()).insert(track);
            let governor = self.governor.get().ok_or_else(|| {
                adk_vision::VisionError::InvalidInput("the governor is not set".into())
            })?;
            let at = observation.frame.captured_at;
            let verdict = governor.request_escalation_at(cx, at, CLIP_FRAMES).await?;
            let (mut sent, mut cost, mut frames) = (false, None, 0);
            if matches!(verdict, EscalationVerdict::Granted { .. }) {
                let bytes = std::fs::read(clip_file(part)).map_err(|e| {
                    adk_vision::VisionError::InvalidInput(format!(
                        "part {part}'s clip is missing ({e}); run `cargo run -- --write-s11-clips`"
                    ))
                })?;
                let clip = SourceClip::decode(bytes)?;
                let request = VideoRequest::Track {
                    prompts: vec![SegmentPrompt::Concept { text: "crack".into() }],
                };
                match self
                    .sam
                    .analyze_video(cx, VideoSource::clip(&clip, Sampling::at(10.0)), &request)
                    .await
                {
                    Ok(out) => {
                        sent = true;
                        cost =
                            out.calls.iter().filter_map(|c| c.cost_micro_usd).reduce(|a, b| a + b);
                        frames = out
                            .calls
                            .iter()
                            .map(|c| c.units.get(adk_core::UsageUnit::Frames))
                            .sum();
                    }
                    Err(error) => self
                        .errors
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(format!("part {part}: {error}")),
                }
            }
            self.asked.lock().unwrap_or_else(|e| e.into_inner()).push(Asked {
                at,
                part,
                verdict,
                sent,
                cost,
                frames,
            });
        }
        Ok(())
    }

    async fn source_event(&self, cx: &VisionCx, event: &SourceEvent) -> adk_vision::Result<()> {
        self.log.source_event(cx, event).await
    }
}

fn steps(log: &StreamLog) -> Vec<(DateTime<Utc>, GovernorChange)> {
    log.events()
        .into_iter()
        .filter_map(|event| match event {
            SourceEvent::Governor { step } => Some((step.at, step.change)),
            _ => None,
        })
        .collect()
}

/// A source's `vision.governor.step` records: when, and the change.
fn recorded(audit: &InMemoryAuditRecorder, source: &str) -> Vec<(DateTime<Utc>, GovernorChange)> {
    audit
        .records()
        .into_iter()
        .filter(|r| r.action == "vision.governor.step" && r.ext["source"] == source)
        .filter_map(|r| {
            let at = serde_json::from_value(r.ext["at"].clone()).ok()?;
            let change = serde_json::from_value(r.ext["change"].clone()).ok()?;
            Some((at, change))
        })
        .collect()
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    let live = recording_proxy("META");
    checks.note(format!(
        "thirty minutes of two cameras from 23:45 UTC, replayed at their capture times; \
         BeltDetector on the device; sam-3.1 video {}",
        if live.is_some() {
            "live through the recording proxy"
        } else {
            "on recorded live answers"
        }
    ));
    let audit = InMemoryAuditRecorder::new();
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let cx = VisionCx::new()
        .with_scope(
            ModelCallScope::new()
                .with_budget(budget.clone())
                .with_spend_ledger(ledger.clone(), ORG),
        )
        .with_audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default());

    // sam-3.1 video, through the egress gate, on the recorded answers or live.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ByClip::load(&clips_dir().join("recorded"))?)
        .mount(&server)
        .await;
    let (base, key) = match &live {
        Some(proxy) => (format!("http://{proxy}/v1"), live_key("META_API_KEY")),
        None => (format!("{}/v1", server.uri()), "recorded".to_string()),
    };
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let sam = VisionRuntime::builder()
        .remote_video_analyzer(Arc::new(
            SamVideoTracker::new(SamConfig::new(key).with_base_url(base))?
                .with_hosts(["api.meta.ai"]),
        ))
        .egress_gate(EgressGate::new(
            EgressPolicy::public_only(),
            EgressConfig::default().with_default(EgressAction::Allow),
            recorder.clone(),
        ))
        .build()?;

    let clip = Arc::new(ConveyorClip::new(ConveyorSpec::default()));
    let log2 = Arc::new(StreamLog::default());
    let escalator = Arc::new(Escalator {
        clip: clip.clone(),
        governor: OnceLock::new(),
        sam,
        log: log2.clone(),
        streaks: Mutex::default(),
        done: Mutex::default(),
        asked: Mutex::default(),
        errors: Mutex::default(),
    });
    let detector =
        Arc::new(VisionRuntime::builder().detector(Arc::new(BeltDetector::new())).build()?);
    let line_2 =
        StreamPipeline::builder(Arc::new(ClipSource::new("line-2", clip.clone(), LOOPS, start())))
            .sampling(Sampling::at(5.0))
            .budget(
                FrameBudget::new(5.0)
                    .with_min_local_fps(1.0)
                    .with_local_quota(1_000_000)
                    .with_escalations_per_minute(4)
                    .with_escalation_quota(300),
            )
            .detector(detector, DetectQuery::Classes(vec!["part".into(), "crack".into()]))
            .tracker(Box::new(GreedyIouTracker::new(0.3, 3)))
            .sink(escalator.clone())
            .build()?;
    let governor = line_2.governor().context("line 2 has a frame governor")?;
    escalator.governor.set(governor.clone()).ok();

    // Line 3: the same belt, drawn small, held to 600 frames a day.
    let small = Arc::new(ConveyorClip::new(ConveyorSpec {
        width: 320,
        height: 176,
        px_per_mm: 1.25,
        ..ConveyorSpec::default()
    }));
    let log3 = Arc::new(StreamLog::default());
    let line_3 =
        StreamPipeline::builder(Arc::new(ClipSource::new("line-3", small, LOOPS, start())))
            .sampling(Sampling::at(5.0))
            .budget(FrameBudget::new(5.0).with_min_local_fps(1.0).with_local_quota(600))
            .sink(log3.clone())
            .build()?;

    let started = std::time::Instant::now();
    let (two, three) = tokio::join!(line_2.run(&cx), line_3.run(&cx));
    let (two, three) = (two?, three?);
    checks.note(format!(
        "line 2: {} frames observed; line 3: {} observed, {} paused; in {:.0?}",
        two.observations,
        three.observations,
        three.paused,
        started.elapsed()
    ));
    if let Some(error) = escalator.errors.lock().unwrap_or_else(|e| e.into_inner()).first() {
        checks.note(format!("escalation error: {error}"));
    }

    // Line 2: escalation stops at its quota and resumes at midnight.
    let asked = escalator.asked();
    let granted = |a: &Asked| matches!(a.verdict, EscalationVerdict::Granted { .. });
    let before: Vec<&Asked> = asked.iter().filter(|a| a.at < midnight()).collect();
    let after: Vec<&Asked> = asked.iter().filter(|a| a.at >= midnight()).collect();
    // Escalation steps are taken when a request asks, so the records hold them; frame steps
    // reach the sink too.
    let line_2_steps = recorded(&audit, "line-2");
    let stopped_at = line_2_steps.iter().find_map(|(at, change)| {
        matches!(
            change,
            GovernorChange::EscalationStopped { reason: StopReason::EscalationQuotaReached }
        )
        .then_some(*at)
    });
    let resumed_at = line_2_steps.iter().find_map(|(at, change)| {
        matches!(change, GovernorChange::EscalationResumed).then_some(*at)
    });
    let refused_before = before.iter().filter(|a| !granted(a)).count();
    checks.check(
        &format!(
            "escalation stops at the daily quota: {} granted before midnight, then {refused_before} refused before any request",
            before.iter().filter(|a| granted(a)).count()
        ),
        before.iter().filter(|a| granted(a)).count() == 6
            && refused_before > 0
            && stopped_at.is_some_and(|at| at < midnight())
            && before.iter().filter(|a| a.sent).count() == 6,
    );
    checks.check(
        &format!(
            "and resumes in the next period: {} granted after midnight",
            after.iter().filter(|a| granted(a)).count()
        ),
        resumed_at.is_some_and(|at| at >= midnight())
            && after.iter().any(|a| granted(a))
            && after.iter().filter(|a| granted(a)).all(|a| a.sent),
    );

    // Every sam-3.1 request is billed at least 50 frames.
    let sent = asked.iter().filter(|a| a.sent).count();
    let costs: Vec<u64> = asked.iter().filter(|a| a.sent).filter_map(|a| a.cost).collect();
    // Lifetime, not today's: a run near midnight UTC spends on two days.
    let spent =
        ledger.spent(&SpendKey::org(ORG).with_vendor("muse"), SpendPeriod::Lifetime).await?;
    let billed: u64 = asked
        .iter()
        .filter_map(|a| match a.verdict {
            EscalationVerdict::Granted { billed_frames } => Some(billed_frames),
            EscalationVerdict::Refused { .. } => None,
        })
        .sum();
    checks.check(
        &format!(
            "each of the {sent} sam-3.1 requests sent {CLIP_FRAMES} frames and is billed 50: {} µUSD in the ledger",
            costs.iter().sum::<u64>()
        ),
        sent > 6
            && costs.len() == sent
            && costs.iter().all(|c| *c == 10_000)
            && asked.iter().filter(|a| a.sent).all(|a| a.frames == CLIP_FRAMES)
            && billed == 50 * sent as u64
            && (live.is_some() || spent == 10_000 * sent as u64)
            && recorder.released().len() == sent,
    );
    let parts: BTreeSet<usize> = asked.iter().filter(|a| a.sent).map(|a| a.part).collect();
    checks.note(format!(
        "ledger for vendor muse: {spent} µUSD; governor billed {billed} frames; parts escalated {parts:?}"
    ));

    // Line 3: the rate goes down before the source pauses, and each change is on record.
    let line_3_steps = steps(&log3);
    let first_pause =
        line_3_steps.iter().position(|(_, c)| matches!(c, GovernorChange::Paused { .. }));
    let ordered = first_pause.is_some_and(|pause| {
        let lowered = line_3_steps[..pause].iter().any(
            |(_, c)| matches!(c, GovernorChange::RateLowered { to_fps, .. } if *to_fps == 1.0),
        );
        let stopped = line_3_steps[..pause]
            .iter()
            .position(|(_, c)| matches!(c, GovernorChange::EscalationStopped { .. }));
        let last_lowered = line_3_steps[..pause]
            .iter()
            .rposition(|(_, c)| matches!(c, GovernorChange::RateLowered { .. }));
        lowered && stopped.zip(last_lowered).is_some_and(|(s, l)| l < s)
    });
    let resumed = line_3_steps
        .iter()
        .any(|(at, c)| matches!(c, GovernorChange::Resumed) && *at >= midnight())
        && line_3_steps.iter().any(|(_, c)| matches!(c, GovernorChange::RateRestored { .. }));
    let kinds: Vec<String> = line_3_steps
        .iter()
        .map(|(at, c)| {
            format!(
                "{} {}",
                at.format("%H:%M:%S"),
                serde_json::to_value(c).map(|v| v["kind"].to_string()).unwrap_or_default()
            )
        })
        .collect();
    checks.note(format!("line 3's steps: {}", kinds.join(", ")));
    checks.check(
        "line 3 lowers its rate to the floor and stops escalating before it pauses, and resumes at midnight",
        ordered && resumed,
    );
    let line_3_records = recorded(&audit, "line-3");
    let line_2_seen = steps(&log2);
    checks.check(
        &format!(
            "every governor change is a `vision.governor.step` record: line 3's {} steps, line 2's {} frame steps and its escalation steps",
            line_3_steps.len(),
            line_2_seen.len()
        ),
        !line_3_steps.is_empty()
            && line_3_records == line_3_steps
            && line_2_seen.iter().all(|step| line_2_steps.contains(step))
            && stopped_at.is_some(),
    );
    Ok(checks)
}
