//! S1 (the Document desk, local first) and S2 (escalate the crop, not the page): one run of
//! a harness with the vision pack over the forty generated pages.
//!
//! PP-OCRv6 tiny reads every page on the device. A block it reads below the confidence
//! threshold — the three handwritten amounts and the two values under a stamp — is cropped
//! with an 8 px margin, redacted, and re-read by NVIDIA NIM's `nemotron-ocr-v1` through the
//! egress gate. NIM answers from its recorded responses on a local server.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_core::audit_log::AuditRecordDraft;
use adk_harness::HarnessMode;
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_vision::{
    BBox, Digest, EgressReceipt, EgressRecord, Escalation, NIM_READER_ID, NimCvConfig,
    NimDocumentReader, Route, Trigger, VisionConfig, VisionRuntime, VisionTask,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use wiremock::MockServer;

use crate::Checks;
use crate::fixtures::{
    DeskSpec, Factory, Page, bbox_of, covered, desk, documents, fixtures, models, normalise,
    read_json, save, tool_responses,
};
use crate::replay::{
    Recording, client_to, images_in, live_key, recording_proxy, requests, serve_in_order,
};

/// A block read below this goes to the cloud as its crop.
pub const THRESHOLD: f32 = 0.8;
/// Pixels of context kept around an escalated block.
pub const MARGIN: f32 = 8.0;
const LOCAL: &str = "pp-ocrv6-tiny";
const NIM_HOST: &str = "ai.api.nvidia.com";

/// One field as the desk read it.
#[derive(Clone, Debug)]
pub struct Extracted {
    pub page: String,
    pub kind: String,
    pub digest: Digest,
    pub field: String,
    pub expected: String,
    pub read: String,
    pub correct: bool,
    pub bbox: Option<BBox>,
    pub backend: String,
    pub escalated_from: Option<String>,
    pub hard: bool,
}

/// What one run of the desk left behind.
pub struct DeskRun {
    pages: Vec<(Page, Digest)>,
    answers: usize,
    fields: Vec<Extracted>,
    released: Vec<EgressReceipt>,
    records: Vec<EgressRecord>,
    audit_released: Vec<AuditRecordDraft>,
    calls: Vec<AuditRecordDraft>,
    ledger_total: u64,
    nim_requests: Vec<Value>,
    tables: BTreeMap<String, usize>,
    tables_dir: PathBuf,
    /// Blocks the cloud re-read, per page.
    escalated: BTreeMap<String, usize>,
}

/// The desk run S1 and S2 share.
#[derive(Default)]
pub struct Shared {
    run: OnceCell<DeskRun>,
}

impl Shared {
    async fn get(&self) -> Result<&DeskRun> {
        self.run.get_or_try_init(run_desk).await
    }
}

/// The text of each block, from the screened answer's `[n] text` lines.
fn block_texts(answer: &Value) -> BTreeMap<u64, String> {
    answer["content"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (n, text) = line.strip_prefix('[')?.split_once("] ")?;
            Some((n.parse().ok()?, text.to_string()))
        })
        .collect()
}

fn extract(page: &Page, digest: Digest, answer: &Value) -> Vec<Extracted> {
    let texts = block_texts(answer);
    let blocks = answer["blocks"].as_array().cloned().unwrap_or_default();
    page.fields
        .iter()
        .map(|field| {
            let want = field.bbox();
            // A block that covers the field, or one centred on it: a reader may split a
            // scrawled value into pieces.
            let hits: Vec<&Value> = blocks
                .iter()
                .filter(|b| {
                    bbox_of(&b["box"])
                        .is_some_and(|b| covered(&want, &b) >= 0.5 || centred(&b, &want))
                })
                .collect();
            let read = hits
                .iter()
                .filter_map(|b| texts.get(&b["line"].as_u64().unwrap_or(0)).cloned())
                .collect::<Vec<_>>()
                .join(" ");
            let escalated_from =
                hits.iter().find_map(|b| b["escalatedFrom"].as_str().map(str::to_string));
            let bbox = hits.iter().filter_map(|b| bbox_of(&b["box"])).reduce(|a, b| {
                BBox::new(a.x0.min(b.x0), a.y0.min(b.y0), a.x1.max(b.x1), a.y1.max(b.y1))
            });
            Extracted {
                page: page.file.clone(),
                kind: page.kind.clone(),
                digest,
                field: field.name.clone(),
                expected: field.value.clone(),
                correct: !read.is_empty() && normalise(&read).contains(&normalise(&field.value)),
                read,
                bbox,
                backend: if escalated_from.is_some() { NIM_READER_ID } else { LOCAL }.to_string(),
                escalated_from,
                hard: field.hard.is_some(),
            }
        })
        .collect()
}

/// Whether `inner`'s centre lies on `field`, within 10 px.
fn centred(inner: &BBox, field: &BBox) -> bool {
    let [x, y] = inner.center();
    x >= field.x0 - 10.0 && x <= field.x1 + 10.0 && y >= field.y0 - 10.0 && y <= field.y1 + 10.0
}

/// One CSV per document kind: each value with its page, region and backend.
fn write_tables(fields: &[Extracted]) -> Result<(BTreeMap<String, usize>, PathBuf)> {
    let dir = std::env::temp_dir().join("adk-vision-scenarios").join("tables");
    std::fs::create_dir_all(&dir)?;
    let mut kinds: BTreeMap<&str, BTreeMap<&str, Vec<&Extracted>>> = BTreeMap::new();
    for field in fields {
        kinds.entry(&field.kind).or_default().entry(&field.page).or_default().push(field);
    }
    let mut counts = BTreeMap::new();
    for (kind, pages) in &kinds {
        let names: Vec<&str> = pages
            .values()
            .next()
            .map(|f| f.iter().map(|f| f.field.as_str()).collect())
            .unwrap_or_default();
        let header: Vec<String> =
            names.iter().map(|n| format!("{n},{n}_box,{n}_backend")).collect();
        let mut csv = format!("page,digest,{}\n", header.join(","));
        for (page, values) in pages {
            let cells: Vec<String> = values
                .iter()
                .map(|v| {
                    let bbox = v
                        .bbox
                        .map(|b| format!("{:.0} {:.0} {:.0} {:.0}", b.x0, b.y0, b.x1, b.y1))
                        .unwrap_or_default();
                    format!("\"{}\",{bbox},{}", v.read.replace('"', "'"), v.backend)
                })
                .collect();
            csv.push_str(&format!("{page},{},{}\n", values[0].digest, cells.join(",")));
        }
        std::fs::write(dir.join(format!("{kind}.csv")), csv)?;
        counts.insert((*kind).to_string(), pages.len());
    }
    Ok((counts, dir))
}

async fn run_desk() -> Result<DeskRun> {
    let pages = documents()?;
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let mut digested = Vec::new();
    for page in &pages {
        let png = page.png()?;
        digested.push((page.clone(), Digest::of(&png)));
        save(&artifacts, &page.file, png).await?;
    }
    let local = models().await?;

    // NVIDIA NIM: the five crops' recorded answers, in the order the desk escalates them.
    let server = MockServer::start().await;
    let (address, key) = match recording_proxy("NIM") {
        Some(proxy) => (
            proxy.parse().context("VISION_RECORD_NIM is host:port")?,
            live_key("NVIDIA_API_KEY"),
        ),
        None => {
            let recorded = Recording::all(&fixtures().join("recorded").join("nim-crops"))?;
            serve_in_order(&server, &recorded).await;
            (*server.address(), "recorded".to_string())
        }
    };
    let nim = NimDocumentReader::new(
        NimCvConfig::new(format!("http://{NIM_HOST}:{}/v1/cv", address.port()))
            .with_api_key(key)
            .with_http_client(client_to(&address, NIM_HOST)?),
    )?
    .with_layout(false)
    .with_tables(false)
    .with_charts(false);

    let reader = local.reader.clone();
    let factory: Factory = Box::new(move |gate, store| {
        VisionRuntime::builder()
            .reader(reader)
            .remote_reader(Arc::new(nim))
            .route(
                VisionTask::Read,
                Route::to(LOCAL).escalate(
                    Escalation::to(NIM_READER_ID)
                        .on(Trigger::LowConfidence { threshold: THRESHOLD })
                        .with_margin(MARGIN)
                        .with_data_class("document_crop"),
                ),
            )
            .shared_egress_gate(gate)
            .media_store(store)
            .build()
    });
    // The operator's rules: crops may go to NIM after redaction; nothing else leaves.
    let rules = VisionConfig::from_toml_str(&format!(
        "[egress]\ndefault = \"deny\"\nredact = [\"faces\", \"personal_text\"]\n\n\
         [[egress.rule]]\nhost = \"{NIM_HOST}\"\nclasses = [\"document_crop\"]\naction = \"allow\"\n"
    ))?;
    let mut script = Vec::new();
    for page in &pages {
        script.push(ScriptedResponse::function_call("vision_read", json!({ "image": page.file })));
        script.push(ScriptedResponse::text(format!("Read {}.", page.file)));
    }
    let desk = desk(
        DeskSpec {
            planner: Arc::new(ScriptedLlm::new("planner", script)),
            mode: HarnessMode::Auto,
            rules,
            budget: None,
            artifacts,
            redact: true,
        },
        factory,
    )
    .await?;

    let mut fields = Vec::new();
    let mut answers = 0;
    let mut escalated = BTreeMap::new();
    for (page, digest) in &digested {
        let events = desk
            .send(&format!("Read {} and add it to the {} table.", page.file, page.kind))
            .await?;
        let answer = tool_responses(&events, "vision_read")
            .into_iter()
            .next()
            .with_context(|| format!("no vision_read answer for {}", page.file))?;
        if answer.get("error").is_some() {
            bail!("vision_read {} failed: {}", page.file, answer["error"]);
        }
        answers += 1;
        let replaced = answer["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["escalatedFrom"] == LOCAL)
            .count();
        escalated.insert(page.file.clone(), replaced);
        fields.extend(extract(page, *digest, &answer));
    }
    let (tables, tables_dir) = write_tables(&fields)?;
    let ledger_total = desk.vision_spend().await?;
    Ok(DeskRun {
        pages: digested,
        answers,
        released: desk.recorder.released(),
        records: desk.recorder.records(),
        audit_released: desk.records("vision.egress.released"),
        calls: desk.records("vision.call"),
        ledger_total,
        nim_requests: requests(&server).await,
        fields,
        tables,
        tables_dir,
        escalated,
    })
}

fn baselines() -> Result<BTreeMap<String, f64>> {
    let value = read_json(&fixtures().join("baselines.json"))?;
    Ok(value["s1FieldAccuracy"]
        .as_object()
        .context("baselines.json has s1FieldAccuracy")?
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_f64()?)))
        .collect())
}

pub async fn s1(shared: &Shared) -> Result<Checks> {
    let run = shared.get().await?;
    let mut checks = Checks::default();
    checks.note(format!(
        "{LOCAL} on the device (fetched by revision, SHA-256 verified); NIM on recorded responses"
    ));

    checks.check("the agent read all forty pages through vision_read", run.answers == 40);
    let want: BTreeMap<String, usize> =
        [("contract", 6), ("identity_card", 10), ("invoice", 12), ("receipt", 12)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
    checks.check(
        &format!("one table per document kind, a row per page ({})", run.tables_dir.display()),
        run.tables == want,
    );
    let carried = run.fields.iter().all(|f| {
        let page = run.pages.iter().find(|(p, _)| p.file == f.page);
        f.bbox.is_some_and(|b| page.is_some_and(|(p, _)| b.is_within(p.width, p.height)))
            && !f.backend.is_empty()
            && page.is_some_and(|(_, d)| *d == f.digest)
    });
    checks.check("each value carries its page digest, region in source pixels and backend", carried);

    let baselines = baselines()?;
    let mut accuracy = Vec::new();
    for backend in [LOCAL, NIM_READER_ID] {
        let mine: Vec<&Extracted> = run.fields.iter().filter(|f| f.backend == backend).collect();
        let correct = mine.iter().filter(|f| f.correct).count();
        let share = if mine.is_empty() { 0.0 } else { correct as f64 / mine.len() as f64 };
        let floor = baselines.get(backend).copied().unwrap_or(1.0);
        accuracy.push(format!(
            "{backend} {correct}/{} = {share:.3} (baseline {floor:.3})",
            mine.len()
        ));
        checks.check(
            &format!("{backend} field accuracy at or above its recorded baseline"),
            share + 1e-9 >= floor,
        );
    }
    checks.note(format!("field accuracy: {}", accuracy.join("; ")));
    for wrong in run.fields.iter().filter(|f| !f.correct) {
        checks.note(format!(
            "misread {} {}: `{}` for `{}`",
            wrong.page, wrong.field, wrong.read, wrong.expected
        ));
    }

    // The pages a block of went to the cloud: the escalation calls' source images.
    let attempted: BTreeSet<Digest> = run
        .calls
        .iter()
        .filter(|c| c.ext.get("escalation").is_some())
        .filter_map(|c| c.ext["imageDigests"][0].as_str()?.parse().ok())
        .collect();
    let local_pages: Vec<&(Page, Digest)> =
        run.pages.iter().filter(|(_, d)| !attempted.contains(d)).collect();
    let silent = local_pages.iter().all(|(_, digest)| {
        run.records.iter().all(|r| match r {
            EgressRecord::Released(r) => r.source != *digest,
            EgressRecord::Refused(r) => r.source != *digest,
            _ => true,
        })
    });
    checks.check(
        &format!(
            "the {} pages whose blocks all cleared {THRESHOLD} sent zero bytes off the machine",
            local_pages.len()
        ),
        local_pages.len() == 35 && silent,
    );

    let local_calls: Vec<&AuditRecordDraft> =
        run.calls.iter().filter(|c| c.ext["backend"] == LOCAL).collect();
    let free = local_calls.iter().all(|c| {
        c.ext.get("costMicroUsd").and_then(Value::as_u64).unwrap_or(0) == 0
            && c.ext["units"]["images"].as_u64().unwrap_or(0) >= 1
    });
    checks.check(
        "each on-device read is a vision.call record with its image units at zero cost",
        local_calls.len() == 40 && free,
    );
    let audited: u64 =
        run.calls.iter().filter_map(|c| c.ext.get("costMicroUsd").and_then(Value::as_u64)).sum();
    checks.check(
        &format!("run spend equals the sum of its ledger entries ({audited} µUSD)"),
        audited == run.ledger_total,
    );
    Ok(checks)
}

pub async fn s2(shared: &Shared) -> Result<Checks> {
    let run = shared.get().await?;
    let mut checks = Checks::default();
    checks.note("NVIDIA NIM nemotron-ocr-v1 on its recorded responses, through the real client");

    let hard: Vec<&Extracted> = run.fields.iter().filter(|f| f.hard).collect();
    let hard_pages: BTreeSet<Digest> = hard.iter().map(|f| f.digest).collect();
    let released_from: BTreeSet<Digest> = run.released.iter().map(|r| r.source).collect();
    checks.check(
        "exactly five egress records, one for each hard field's page",
        run.released.len() == 5 && released_from == hard_pages,
    );

    let crops = run.released.iter().all(|receipt| {
        let Some((page, _)) = run.pages.iter().find(|(_, d)| *d == receipt.source) else {
            return false;
        };
        let Some(field) = hard.iter().find(|f| f.page == page.file) else { return false };
        let (w, h) = receipt.chain.output_size();
        let sent = receipt.chain.bbox_to_source(&BBox::new(0.0, 0.0, w as f32, h as f32));
        let want = page.fields.iter().find(|f| f.name == field.field).map(|f| f.bbox());
        let page_area = (page.width * page.height) as f32;
        want.is_some_and(|b| covered(&b, &sent) > 0.0 || covered(&sent, &b) > 0.0)
            && sent.width() * sent.height() < page_area / 20.0
    });
    checks.check("each sent image is a crop of its field's block plus margin, never the page", crops);

    // Where the cloud read text, its answer replaced the local block; where it read none,
    // the local value stands.
    let read_something = run
        .calls
        .iter()
        .filter(|c| c.ext["backend"] == NIM_READER_ID)
        .filter(|c| c.ext["regions"].as_u64().unwrap_or(0) > 0)
        .count();
    let replaced: usize = run.escalated.values().sum();
    let on_hard_pages = run
        .escalated
        .iter()
        .filter(|(_, n)| **n > 0)
        .all(|(file, n)| *n == 1 && hard.iter().any(|f| &f.page == file));
    checks.check(
        &format!(
            "each cloud answer replaces its local block with escalatedFrom pp-ocrv6-tiny ({replaced} of 5 crops held text)"
        ),
        replaced == read_something && replaced > 0 && on_hard_pages,
    );
    let right = hard.iter().filter(|f| f.correct).count();
    checks.note(format!("NIM read {right}/5 hard fields as printed"));

    let named = run.released.iter().all(|r| {
        r.hosts == vec![NIM_HOST.to_string()]
            && r.data_class.as_str() == "document_crop"
            && r.artifact.is_some()
            && run.audit_released.iter().any(|a| {
                a.ext["sourceDigest"] == json!(r.source)
                    && a.ext["egressDigest"] == json!(r.egress)
                    && a.ext["destination"] == json!([NIM_HOST])
                    && a.ext.get("redactions").is_some()
            })
    });
    checks.check(
        "each record names host, source and egress digests, redactions, and keeps the egress copy",
        named && run.audit_released.len() == 5,
    );
    let unpriced = run.released.iter().all(|r| r.estimated_cost_micro_usd.is_none());
    checks.check(
        "each record's cost is NIM's: unpriced, so nothing is estimated or charged",
        unpriced && run.ledger_total == 0,
    );

    let sent: Vec<Digest> =
        run.nim_requests.iter().flat_map(images_in).map(|b| Digest::of(&b)).collect();
    let egress: Vec<Digest> = run.released.iter().map(|r| r.egress).collect();
    checks.check(
        "NIM received five requests, each carrying exactly the released crop",
        recording_proxy("NIM").is_some() || (run.nim_requests.len() == 5 && sent == egress),
    );
    Ok(checks)
}
