//! S7, a budget per image: a harness run capped at 20 images asks `sam-3.1` for 30
//! segmentations. Twenty run; the 21st and later are refused before any request, naming the
//! unit and the cap; the ledger holds twenty commits of 2,500 µUSD; an on-device call
//! records its image at zero cost. `sam-3.1` answers from a recorded live response.

use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_core::{RunBudget, SpendKey, SpendLedger, SpendPeriod, UsageUnit};
use adk_harness::HarnessMode;
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_vision::{SamConfig, SamSegmenter, VisionConfig, VisionRuntime};
use anyhow::Result;
use serde_json::{Value, json};
use wiremock::MockServer;

use crate::Checks;
use crate::fixtures::{
    APP, DeskSpec, Factory, crate_fixtures, desk, models, save, tool_responses,
};
use crate::replay::{Recording, requests, serve_always};

const CAP: u64 = 20;
const CALLS: usize = 30;

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    checks.note("sam-3.1 on a recorded live response through the real client; RF-DETR Nano on the device");
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let part = std::fs::read(crate_fixtures().join("cloud/shapes-960x540.png"))?;
    save(&artifacts, "part.png", part).await?;
    let server = MockServer::start().await;
    let recording = crate_fixtures().join("cloud/recorded/sam-3-1.shapes-red-rectangle.json");
    serve_always(&server, &Recording::load(&recording)?).await;
    let sam = SamSegmenter::new(
        SamConfig::new("recorded").with_base_url(format!("{}/v1", server.uri())),
    )?
    .with_hosts(["api.meta.ai"]);
    let detector = models().await?.detector.clone();
    let factory: Factory = Box::new(move |gate, store| {
        VisionRuntime::builder()
            .detector(detector)
            .remote_segmenter(Arc::new(sam))
            .shared_egress_gate(gate)
            .media_store(store)
            .build()
    });
    let rules = VisionConfig::from_toml_str(
        "[egress]\ndefault = \"deny\"\nredact = [\"faces\", \"personal_text\"]\n\n\
         [[egress.rule]]\nhost = \"api.meta.ai\"\naction = \"allow\"\n",
    )?;
    let calls: Vec<(String, Value)> = (0..CALLS)
        .map(|i| {
            ("vision_segment".to_string(), json!({ "image": "part.png", "concepts": [format!("red part {i}")] }))
        })
        .collect();
    let planner = ScriptedLlm::new(
        "planner",
        [
            ScriptedResponse::function_calls(calls),
            ScriptedResponse::text("Segmented."),
            ScriptedResponse::function_call("vision_detect", json!({ "image": "part.png", "classes": ["person"] })),
            ScriptedResponse::text("Detected."),
        ],
    );
    let desk = desk(
        DeskSpec {
            planner: Arc::new(planner),
            mode: HarnessMode::Auto,
            rules,
            budget: Some(RunBudget::new().max_units(UsageUnit::Images, CAP)),
            artifacts,
            redact: true,
        },
        factory,
    )
    .await?;

    let events = desk.send("Segment the part thirty times.").await?;
    let answers = tool_responses(&events, "vision_segment");
    let refused: Vec<&Value> = answers.iter().filter(|a| a.get("error").is_some()).collect();
    checks.check(
        &format!("{} of {CALLS} segmentations ran", answers.len() - refused.len()),
        answers.len() == CALLS && answers.len() - refused.len() == CAP as usize,
    );
    let reason = refused.first().and_then(|a| a["error"].as_str()).unwrap_or_default();
    checks.check(
        "each later call is refused naming the unit and the cap",
        !refused.is_empty()
            && refused.iter().all(|a| {
                let e = a["error"].as_str().unwrap_or_default();
                e.contains("images") && e.contains(&CAP.to_string())
            }),
    );
    checks.note(format!("refusal: {reason}"));
    let sent = requests(&server).await.len();
    checks.check(
        &format!("sam-3.1 received {sent} requests: the refusals came before any request"),
        sent == CAP as usize && desk.recorder.released().len() == CAP as usize,
    );
    let spent = desk.ledger.spent(&SpendKey::org(APP).with_vendor("meta"), SpendPeriod::Day).await?;
    let commits: Vec<u64> = desk
        .records("vision.call")
        .iter()
        .filter(|c| c.ext["backend"] == "sam-3.1")
        .filter_map(|c| c.ext.get("costMicroUsd").and_then(Value::as_u64))
        .collect();
    checks.check(
        &format!("the ledger holds {} commits of 2,500 µUSD ({spent} µUSD)", commits.len()),
        commits.len() == CAP as usize && commits.iter().all(|c| *c == 2_500) && spent == CAP * 2_500,
    );

    let before = desk.ledger.spent(&SpendKey::org(APP), SpendPeriod::Day).await?;
    desk.send("Is anyone in the picture?").await?;
    let after = desk.ledger.spent(&SpendKey::org(APP), SpendPeriod::Day).await?;
    let local = desk.records("vision.call").into_iter().filter(|c| c.ext["backend"] == "rf-detr-n").collect::<Vec<_>>();
    checks.check(
        "an on-device call records its image at zero cost",
        local.len() == 1
            && local[0].ext["units"]["images"] == 1
            && local[0].ext.get("costMicroUsd").and_then(Value::as_u64).unwrap_or(0) == 0
            && after == before,
    );
    Ok(checks)
}
