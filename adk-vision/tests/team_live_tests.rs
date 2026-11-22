//! Live S15: a perceiver, reasoner and verifier team on an ambiguous generated defect.
//!
//! The image is a printed part with layer lines and one faint diagonal mark that could be a
//! scratch or a print artefact. The perceiver, an on-device contrast detector, is unsure of
//! it; the reasoner, `claude-haiku-5-5`, sees only the redacted crop the egress gate released;
//! the verifier re-checks the claim with a stricter on-device detector and decides on NVIDIA
//! NIM `nemotron-3.5-super-vl`, another provider. Either they agree, or the team holds exactly
//! one card with both opinions and the frame.
//!
//! `#[ignore]`d; needs `ANTHROPIC_API_KEY` and `NVIDIA_API_KEY`:
//!
//! ```text
//! cargo test -p adk-vision --features team,nim --test team_live_tests -- --ignored --nocapture
//! ```

mod governed;

use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_core::egress::EgressPolicy;
use adk_core::{
    DataLocality, Event, GenerateContentConfig, InMemoryApprovalStore, Llm, LlmRequest,
    LlmResponseStream, Part, RunConfig,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_vision::cloud::nim::NimConfig;
use adk_vision::team::{DISAGREEMENT_TOOL, Opinion, Perceiver, Reasoner, Verifier, VisionTeam};
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DecodedImage, DetectQuery, Detector, Digest, EgressGate,
    InMemoryEgressRecorder, Locality, MediaStore, PersonRedactor, RawRegion, SentImage,
    VisionConfig, VisionCx, VisionRuntime,
};
use governed::{APP, RedPersonDetector, SESSION, USER, save_artifact};
use serde_json::json;
use std::sync::Mutex;

const REASONER_MODEL: &str = "claude-haiku-5-5";
const VERIFIER_MODEL: &str = "nvidia/nemotron-3.5-super-vl-120b-a12b";

/// A printed part on a dark bench: layer lines every 4 px and one faint diagonal mark.
fn part() -> Vec<u8> {
    let (w, h) = (640u32, 480u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let mut set = |x: u32, y: u32, p: [u8; 4]| {
        let i = ((y * w + x) * 4) as usize;
        rgba[i..i + 4].copy_from_slice(&p);
    };
    for y in 0..h {
        for x in 0..w {
            set(x, y, [58, 60, 64, 255]);
        }
    }
    for y in 90..390u32 {
        for x in 120..520u32 {
            let layer = if y % 4 == 0 { 182 } else { 192 };
            set(x, y, [layer, layer, layer - 4, 255]);
        }
    }
    // The mark: 2 px wide, only a little darker than the layer lines.
    for step in 0..=120u32 {
        let (x, y) = (290 + step / 2, 196 + step / 4);
        for dy in 0..2 {
            set(x, y + dy, [156, 156, 152, 255]);
        }
    }
    DecodedImage::from_rgba(w, h, rgba).unwrap().encode_png().unwrap()
}

/// An on-device detector: the box of every pixel on the part darker than `threshold`,
/// scored by how much darker it is.
struct Contrast {
    descriptor: BackendDescriptor,
    threshold: u8,
}

#[async_trait::async_trait]
impl Detector for Contrast {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        _query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        let pixels = image.pixels();
        let (w, h) = pixels.size();
        let mut bounds: Option<(u32, u32, u32, u32, u8)> = None;
        for y in 0..h {
            for x in 0..w {
                let p = pixels.pixel(x, y);
                // Only on the part, which is light; the bench is dark.
                if p[0] > 100 && p[0] < self.threshold {
                    bounds = Some(match bounds {
                        None => (x, y, x, y, p[0]),
                        Some((x0, y0, x1, y1, d)) => {
                            (x0.min(x), y0.min(y), x1.max(x), y1.max(y), d.min(p[0]))
                        }
                    });
                }
            }
        }
        Ok(bounds
            .map(|(x0, y0, x1, y1, darkest)| {
                let score = (f32::from(192 - darkest) / 80.0).clamp(0.0, 1.0);
                RawRegion::boxed(
                    BBox::new(x0 as f32, y0 as f32, (x1 + 1) as f32, (y1 + 1) as f32),
                    "scratch",
                    score,
                )
            })
            .into_iter()
            .collect())
    }
}

fn contrast(id: &str, threshold: u8) -> Arc<VisionRuntime> {
    let detector = Contrast {
        descriptor: BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels),
        threshold,
    };
    Arc::new(VisionRuntime::builder().detector(Arc::new(detector)).build().unwrap())
}

/// Records what a model was sent, then forwards it.
struct Recorded {
    inner: Arc<dyn Llm>,
    requests: Mutex<Vec<LlmRequest>>,
}

#[async_trait::async_trait]
impl Llm for Recorded {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn data_locality(&self) -> DataLocality {
        self.inner.data_locality()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.generate_content(request, stream).await
    }
}

fn key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|k| !k.is_empty())
}

#[tokio::test]
#[ignore = "live: needs ANTHROPIC_API_KEY and NVIDIA_API_KEY"]
async fn s15_live_team_on_an_ambiguous_defect() {
    let (Some(anthropic), Some(nvidia)) = (key("ANTHROPIC_API_KEY"), key("NVIDIA_API_KEY")) else {
        eprintln!("skipped: ANTHROPIC_API_KEY and NVIDIA_API_KEY are needed");
        return;
    };
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts.clone());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let gate = Arc::new(
        EgressGate::new(
            EgressPolicy::public_only(),
            VisionConfig::from_toml_str("[egress]\ndefault = \"allow\"\nredact = [\"faces\"]\n")
                .unwrap()
                .egress,
            recorder.clone(),
        )
        .with_redactor(Arc::new(PersonRedactor::new(Arc::new(RedPersonDetector::new()))))
        .with_media_store(store.clone()),
    );
    let claude: Arc<dyn Llm> = Arc::new(
        adk_model::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
            anthropic,
            REASONER_MODEL,
        ))
        .unwrap(),
    );
    let claude = Arc::new(Recorded { inner: claude, requests: Mutex::new(Vec::new()) });
    let nemotron: Arc<dyn Llm> = Arc::new(
        NimConfig::new("https://integrate.api.nvidia.com/v1", nvidia, VERIFIER_MODEL)
            .client()
            .unwrap(),
    );
    let mut thinking_off = GenerateContentConfig::default();
    thinking_off
        .extensions
        .insert("openai".into(), json!({ "chat_template_kwargs": { "enable_thinking": false } }));

    let reasoner = Reasoner::builder("reasoner")
        .model(claude.clone())
        .egress_gate(gate.clone())
        .hosts(["api.anthropic.com"])
        .max_output_tokens(400)
        .build()
        .unwrap();
    let verifier = Verifier::builder("verifier")
        .runtime(contrast("contrast-strict", 120))
        .model(nemotron)
        .provider("nvidia")
        .egress_gate(gate)
        .hosts(["integrate.api.nvidia.com"])
        .generate_config(thinking_off)
        .max_output_tokens(600)
        .build()
        .unwrap();
    let perceiver =
        Perceiver::builder("perceiver").runtime(contrast("contrast", 176)).build().unwrap();
    let team = VisionTeam::builder("inspector")
        .perceiver(perceiver)
        .reasoner(reasoner)
        .verifier(verifier)
        .question(
            "This is a crop of a 3D-printed part on an inspection line. Is the dark diagonal \
             mark a surface defect (a scratch or crack), or only a print artefact that needs no \
             action?",
        )
        .verdicts("defect", "no_defect")
        .classes(["scratch"])
        .media_store(store)
        .build()
        .unwrap();

    let png = part();
    let frame = Digest::of(&png);
    save_artifact(&artifacts, "part.png", &png).await;
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let config = RunConfig::builder().approval_store(approvals).build();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(team))
        .session_service(sessions)
        .artifact_service(artifacts)
        .run_config(config)
        .build()
        .unwrap();
    let events: Vec<Event> =
        governed::run_turn(&runner, &json!({ "image": "part.png" }).to_string()).await;

    // The reasoner saw only released crops.
    let released: Vec<Digest> = recorder.released().iter().map(|r| r.egress).collect();
    let requests = claude.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "the reasoner was asked once");
    for part in requests[0].contents.iter().flat_map(|c| c.parts.iter()) {
        if let Part::InlineData { data, .. } = part {
            let digest = Digest::of(data);
            assert!(released.contains(&digest) && digest != frame, "only released crops");
        }
    }
    let cards: Vec<_> = events.iter().filter_map(|e| e.actions.tool_confirmation.clone()).collect();
    match cards.as_slice() {
        [] => {
            let text = events
                .iter()
                .rev()
                .find_map(|e| {
                    e.llm_response
                        .content
                        .as_ref()
                        .and_then(|c| c.parts.iter().find_map(Part::text))
                })
                .unwrap()
                .to_string();
            eprintln!("agreed: {text}");
        }
        [card] => {
            assert_eq!(card.tool_name, DISAGREEMENT_TOOL);
            assert_eq!(card.args["frame"], json!(frame));
            assert_eq!(card.evidence[0].digest(), frame.to_string());
            let reasoner: Opinion = serde_json::from_value(card.args["reasoner"].clone()).unwrap();
            let verifier: Opinion = serde_json::from_value(card.args["verifier"].clone()).unwrap();
            eprintln!(
                "one card: reasoner {} {} ({:.2}): {} | verifier {} {} ({:.2}): {}",
                reasoner.provider,
                reasoner.verdict,
                reasoner.confidence,
                reasoner.rationale,
                verifier.provider,
                verifier.verdict,
                verifier.confidence,
                verifier.rationale
            );
        }
        more => panic!("{} cards; a disagreement raises one", more.len()),
    }
    eprintln!("egress records: {}", recorder.released().len());
}
