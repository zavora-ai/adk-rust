//! S3 (redact before it leaves) and S4 (approve with the image) on the generated identity
//! cards, with the real on-device redactors: RF-DETR Nano covers the portrait as a person
//! and PP-OCRv6 with the PII patterns covers the personal text. Claude answers from its
//! recorded responses, through the real Anthropic client.

use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest};
use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::egress::EgressPolicy;
use adk_core::{Evidence, Llm, Part};
use adk_harness::HarnessMode;
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_vision::{
    DescribeSpec, DetectQuery, Digest, EgressGate, InMemoryEgressRecorder, LlmDescriber,
    MediaScope, MediaStore, ReadOptions, SourceImage, VisionConfig, VisionCx, VisionRuntime,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use wiremock::MockServer;

use crate::Checks;
use crate::fixtures::{
    APP, DeskSpec, Factory, Models, SESSION, USER, covered, desk, fixtures, models, normalise,
    page, redactors, save, tool_responses,
};
use crate::replay::{Recording, images_in, live_key, recording_proxy, requests, serve_always};

const ANTHROPIC_HOST: &str = "api.anthropic.com";

/// A Claude describer behind the local server, or behind the recording proxy when
/// `VISION_RECORD_ANTHROPIC` names one.
async fn claude(model: &str, recording: &str) -> Result<(LlmDescriber, MockServer)> {
    let server = MockServer::start().await;
    let (base, key) = match recording_proxy("ANTHROPIC") {
        Some(proxy) => (format!("http://{proxy}"), live_key("ANTHROPIC_API_KEY")),
        None => {
            let file = fixtures().join("recorded").join("claude").join(recording);
            serve_always(&server, &Recording::load(&file)?).await;
            (server.uri(), "recorded".to_string())
        }
    };
    let config = adk_model::anthropic::AnthropicConfig::new(key, model)
        .allow_insecure_http()
        .with_base_url(base);
    let client: Arc<dyn Llm> = Arc::new(adk_model::AnthropicClient::new(config)?);
    let describer = LlmDescriber::remote_with_hosts(client, [ANTHROPIC_HOST])?.with_max_output_tokens(400);
    Ok((describer, server))
}

/// What the on-device models still find in an image: people, and the text they read.
async fn seen(models: &Models, image: &SourceImage) -> Result<(Vec<adk_vision::BBox>, String)> {
    let runtime = VisionRuntime::builder()
        .detector(models.detector.clone())
        .reader(models.reader.clone())
        .build()?;
    let cx = VisionCx::new();
    let people = runtime
        .detect(&cx, image, &DetectQuery::Classes(vec!["person".into()]))
        .await?
        .value
        .into_iter()
        .filter(|r| r.label.as_deref() == Some("person"))
        .filter_map(|r| r.shape.bbox())
        .collect();
    let page = runtime.read(&cx, image, &ReadOptions::default()).await?.value;
    let text = page.blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join(" ");
    Ok((people, text))
}

/// The personal strings of a card that `text` still shows.
fn leaked<'a>(personal: &'a [String], text: &str) -> Vec<&'a str> {
    let text = normalise(text);
    personal.iter().filter(|s| text.contains(&normalise(s))).map(String::as_str).collect()
}

pub async fn s3() -> Result<Checks> {
    let mut checks = Checks::default();
    checks.note("RF-DETR Nano and PP-OCRv6 tiny redact on the device; Claude Haiku 5.5 on a recorded response");
    let card = page("id-card-01.png")?;
    let source = card.source()?;
    let local = models().await?;

    // The detector and the reader see the face and the personal text on the card itself.
    let (people, text) = seen(local, &source).await?;
    let portrait = card.portrait().context("an identity card has a portrait")?;
    checks.check(
        "on the card, RF-DETR finds the generated portrait as a person and PP-OCRv6 reads every seeded personal string",
        people.iter().any(|p| covered(&portrait, p) >= 0.5)
            && leaked(&card.personal, &text).len() == card.personal.len(),
    );

    let (describer, server) = claude("claude-haiku-5-5", "s3-haiku-id-card.json").await?;
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let store = MediaStore::new(artifacts.clone());
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let (people_redactor, text_redactor) = redactors(local);
    let rules = VisionConfig::from_toml_str(&format!(
        "[egress]\ndefault = \"deny\"\nredact = [\"faces\", \"personal_text\"]\n\n\
         [[egress.rule]]\nhost = \"{ANTHROPIC_HOST}\"\naction = \"allow\"\n"
    ))?;
    let gate = EgressGate::new(EgressPolicy::public_only(), rules.egress, recorder.clone())
        .with_redactor(people_redactor)
        .with_redactor(text_redactor)
        .with_media_store(store.clone());
    let runtime = VisionRuntime::builder()
        .remote_describer(Arc::new(describer))
        .egress_gate(gate)
        .media_store(store)
        .build()?;
    let audit = InMemoryAuditRecorder::new();
    let cx = VisionCx::new()
        .with_media_scope(MediaScope::new(APP, USER, SESSION))
        .with_audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default());
    let answer = runtime
        .describe(
            &cx,
            &[&source],
            &DescribeSpec::new("What kind of document is this?").with_max_output_tokens(300),
        )
        .await?;

    let released = recorder.released();
    let receipt = released.first().context("the card was released")?;
    let bodies = requests(&server).await;
    let received: Vec<Vec<u8>> = bodies.iter().flat_map(images_in).collect();
    let live = recording_proxy("ANTHROPIC").is_some();
    checks.check(
        "the provider received one image, exactly the released egress digest",
        live || (received.len() == 1 && Digest::of(&received[0]) == receipt.egress),
    );
    let stored = artifacts
        .load(LoadRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: SESSION.into(),
            file_name: receipt
                .artifact
                .as_ref()
                .context("the egress copy is stored")?
                .file_name
                .clone(),
            version: receipt.artifact.as_ref().map(|a| a.version),
        })
        .await?;
    let Part::InlineData { data: egress_bytes, .. } = stored.part else {
        anyhow::bail!("the egress copy is an image artifact");
    };
    checks.check(
        "the egress copy is an artifact with the egress digest",
        Digest::of(&egress_bytes) == receipt.egress,
    );
    let sent = SourceImage::decode(&egress_bytes, &Default::default())?;
    let (people_after, text_after) = seen(local, &sent).await?;
    checks.check("RF-DETR finds no face in the bytes that left", people_after.is_empty());
    let still = leaked(&card.personal, &text_after);
    checks.check(
        &format!(
            "PP-OCRv6 reads none of the {} seeded personal strings in them",
            card.personal.len()
        ),
        still.is_empty(),
    );
    let audited = audit.records().into_iter().any(|r| {
        r.action == "vision.egress.released"
            && r.ext["sourceDigest"] == json!(source.digest())
            && r.ext["egressDigest"] == json!(receipt.egress)
            && r.ext["redactions"]["faces"].as_u64().unwrap_or(0) >= 1
            && r.ext["redactions"]["personal_text"].as_u64().unwrap_or(0) >= 4
    });
    checks.check(
        "the vision.egress.released record links both digests and counts the redactions by class",
        audited,
    );
    let text = answer.value.text.unwrap_or_default();
    checks.check(
        "Claude's answer repeats none of the personal strings",
        leaked(&card.personal, &text).is_empty(),
    );
    checks.note(format!("Claude answered: {}", text.lines().next().unwrap_or_default()));
    Ok(checks)
}

fn describe_args() -> Value {
    json!({
        "image": "id-card.png",
        "backend": "claude-sonnet-5-5",
        "region": [20, 70, 620, 390],
        "question": "What kind of document is this?",
    })
}

pub async fn s4() -> Result<Checks> {
    let mut checks = Checks::default();
    checks.note(
        "a harness in Ask mode; Claude Sonnet 5.5 on a recorded response; on-device redactors",
    );
    let card = page("id-card-02.png")?;
    let other = page("id-card-03.png")?;
    let artifacts = Arc::new(InMemoryArtifactService::new());
    save(&artifacts, "id-card.png", card.png()?).await?;
    let (describer, server) = claude("claude-sonnet-5-5", "s4-sonnet-id-card.json").await?;
    let factory: Factory = Box::new(move |gate, store| {
        VisionRuntime::builder()
            .remote_describer(Arc::new(describer))
            .shared_egress_gate(gate)
            .media_store(store)
            .build()
    });
    let rules = VisionConfig::from_toml_str(&format!(
        "[egress]\ndefault = \"ask\"\nredact = [\"faces\", \"personal_text\"]\n\n\
         [[egress.rule]]\nhost = \"{ANTHROPIC_HOST}\"\nclasses = [\"document_crop\"]\naction = \"ask\"\n"
    ))?;
    let planner = ScriptedLlm::new(
        "planner",
        [
            ScriptedResponse::function_call("vision_describe", describe_args()),
            ScriptedResponse::text("It is an identity card."),
            ScriptedResponse::function_call("vision_describe", describe_args()),
            ScriptedResponse::text("It is an identity card."),
        ],
    );
    let desk = desk(
        DeskSpec {
            planner: Arc::new(planner),
            mode: HarnessMode::Ask,
            rules,
            budget: None,
            artifacts: artifacts.clone(),
            redact: true,
        },
        factory,
    )
    .await?;

    let events = desk.send("What kind of document is id-card.png?").await?;
    let prompts = desk.approver.prompts();
    let held: Vec<_> =
        prompts.iter().filter(|p| p.request.tool_name == "vision_describe").collect();
    let first = held.first().context("the remote call was held")?;
    let Some(Evidence::Image(evidence)) = first.request.evidence.first() else {
        anyhow::bail!("the held call carries image evidence");
    };
    let egress: Digest = evidence.digest.parse()?;
    checks.check(
        "the call is held, and the card carries the exact egress image, its regions and digest",
        held.len() == 1 && evidence.regions.is_some() && evidence.width > 0,
    );
    checks.note(format!(
        "card: {} {}x{}; {}",
        evidence.digest,
        evidence.width,
        evidence.height,
        evidence.caption.clone().unwrap_or_default()
    ));
    let released = desk.recorder.released();
    let received: Vec<Digest> =
        requests(&server).await.iter().flat_map(images_in).map(|b| Digest::of(&b)).collect();
    let live = recording_proxy("ANTHROPIC").is_some();
    checks.check(
        "approval released that digest, and the provider received exactly it",
        released.len() == 1 && released[0].egress == egress && (live || received == vec![egress]),
    );
    let answered = tool_responses(&events, "vision_describe")
        .first()
        .is_some_and(|a| a.get("error").is_none());
    checks.check("the approved call answered", answered);

    // The image is swapped before the next call: a different egress image, held again.
    save(&artifacts, "id-card.png", other.png()?).await?;
    desk.send("And now?").await?;
    let prompts = desk.approver.prompts();
    let again: Vec<_> =
        prompts.iter().filter(|p| p.request.tool_name == "vision_describe").collect();
    let swapped = again.get(1).and_then(|p| match p.request.evidence.first() {
        Some(Evidence::Image(e)) => Some((e.digest.clone(), p.request.fingerprint())),
        _ => None,
    });
    checks.check(
        "a swapped image changes the digest and the fingerprint, and the call is held again",
        swapped.as_ref().is_some_and(|(digest, fingerprint)| {
            *digest != evidence.digest && *fingerprint != first.request.fingerprint()
        }),
    );

    let decided: Vec<Value> = desk
        .records("vision_describe")
        .into_iter()
        .filter_map(|r| r.ext.get("evidenceDigests").cloned())
        .collect();
    let releases = desk.records("vision.egress.released");
    checks.check(
        "the audit records link each decision to its image digest",
        decided.iter().any(|d| d == &json!([egress]))
            && releases.iter().any(|r| {
                r.ext["egressDigest"] == json!(egress)
                    && r.ext["decisionKind"]["kind"] == "approved"
            }),
    );
    Ok(checks)
}
