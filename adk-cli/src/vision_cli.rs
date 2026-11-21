//! `adk-rust vision`: model manifests and licences, one perception call through the egress
//! gate, and the egress log (spec 3.7 §8.1).
//!
//! | Command | Library |
//! |---|---|
//! | `vision models list` | `ManifestSet::builtin`, the user's manifests, `ModelFetcher::cached`, the project's `FileAcceptanceStore` |
//! | `vision models fetch <id>…` | `ModelFetcher::fetch`: by revision, SHA-256 checked, the licence gate first |
//! | `vision models verify [<id>…]` | `ModelFetcher::fetch` offline: every cached file's SHA-256 again, no socket |
//! | `vision models accept <id>` | `LicenceAcceptance` in `.adk/vision/licences.json`, from a terminal only (V-I11) |
//! | `vision run detect\|segment\|read\|describe <image>` | `VisionRuntime` with one backend, the `EgressGate` of `.adk/vision.toml` for a remote one |
//! | `vision run … --backend sam-3.1 --mask-decoder boxes\|external\|licensed` | `SamConfig::with_mask_decoder`: `[sam.masks]` of `.adk/vision.toml` unless a flag chooses; `licensed` needs feature `vision-sam-codec` and `vision models accept sam-mask-codec` |
//! | `vision egress log` | The project's `.adk/vision/egress.jsonl`, written by the gate before each release (V-I3) |
//!
//! Everything a project keeps is beside its `.adk/vision.toml`, in `.adk/vision/`.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_vision::{
    AcceptanceStore, BBox, CallOptions, CallRecord, DataClass, DecodeLimits, DescribeSpec,
    DetectQuery, Digest, DocumentPage, EditOp, EgressGate, EgressRecord, EgressRecorder,
    FetchConfig, FileAcceptanceStore, LicenceAcceptance, LicenceClass, Locality, ManifestSet,
    MaskDecoderChoice, MediaScope, MediaStore, ModelFetcher, ModelManifest, ModelSource, Overlay,
    ReadOptions, RedactionClass, Region, RegionSet, ReleaseRequest, Rgba, SegmentPrompt, Shape,
    SourceImage, TransformChain, VisionConfig, VisionCx, VisionError, VisionRuntime,
    VisionRuntimeBuilder, VisionTask, apply_edit,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// The rules file, relative to the project.
const RULES_FILE: &str = ".adk/vision.toml";
/// What the CLI keeps, relative to the project.
const STATE_DIR: &str = ".adk/vision";
/// The default model of each describing backend.
const CLAUDE_MODEL: &str = "claude-haiku-5-5";
const GEMINI_MODEL: &str = "gemini-3.7-flash";
const NIM_VLM_MODEL: &str = "nvidia/nemotron-3.5-super-vl-120b-a12b";
/// The detector the face redactor runs (spec 3.7 Q3: a person's whole box stands in for a
/// face until a face detector is chosen).
#[cfg(feature = "vision-local")]
const PERSON_DETECTOR: &str = "rf-detr-n";

/// `adk-rust vision` (spec 3.7 §8.1).
#[derive(clap::Args, Debug, Clone)]
pub struct VisionArgs {
    /// The project whose `.adk/vision.toml` and `.adk/vision/` to use (default: the current
    /// directory)
    #[arg(long, global = true, value_name = "DIR")]
    pub project: Option<PathBuf>,

    #[command(subcommand)]
    pub command: VisionCommands,
}

/// `adk-rust vision` subcommands.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum VisionCommands {
    /// The model manifest: licences, fetches by revision, SHA-256 checks
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },

    /// One perception call: on the device first, through the egress gate when remote
    ///
    /// Example: `adk-rust vision run read invoice.png --annotate invoice.read.png`.
    Run {
        #[command(subcommand)]
        task: Box<RunTask>,
    },

    /// What left the machine, and what was held or refused
    Egress {
        #[command(subcommand)]
        command: EgressCommand,
    },
}

/// `adk-rust vision models`.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum ModelsCommand {
    /// Every model in the manifest with its licence, size and cache state
    List {
        #[command(flatten)]
        manifests: ManifestArgs,
        /// table or json
        #[arg(long, value_enum, default_value_t = ListFormat::Table)]
        output: ListFormat,
    },

    /// Fetch models by revision into the Hugging Face cache, each file SHA-256 checked
    ///
    /// `HF_HUB_OFFLINE=1` opens no socket; `HF_HUB_CACHE`, `HF_HOME`, `HF_ENDPOINT` and
    /// `HF_TOKEN` are read as `huggingface_hub` reads them.
    Fetch {
        /// Manifest ids, such as `rf-detr-n pp-ocrv6-tiny-det pp-ocrv6-tiny-rec`
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,
        #[command(flatten)]
        manifests: ManifestArgs,
    },

    /// Check every cached file's SHA-256 against the manifest again, without the network
    Verify {
        /// Manifest ids (default: every model with all its files cached)
        #[arg(value_name = "ID")]
        ids: Vec<String>,
        #[command(flatten)]
        manifests: ManifestArgs,
    },

    /// Show a use-restricted model's licence and record your acceptance of it
    ///
    /// Asks on a terminal; no agent tool records an acceptance.
    Accept {
        /// The manifest id
        #[arg(value_name = "ID")]
        id: String,
        #[command(flatten)]
        manifests: ManifestArgs,
    },
}

/// Manifests of your own models beside the built-in one.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ManifestArgs {
    /// A model manifest JSON file, or a directory of them, such as an RF-DETR fine-tune you
    /// exported
    #[arg(long, value_name = "PATH")]
    pub manifest: Vec<PathBuf>,
}

/// `models list` and `egress log` output.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFormat {
    /// Aligned columns
    Table,
    /// JSON
    Json,
}

/// `vision run` output.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFormat {
    /// Aligned columns, then the backend calls
    Table,
    /// The result and its backend calls as JSON
    Json,
    /// The `RegionSet` JSON (adk-vision `schema/region-set.json`)
    Regions,
}

/// `adk-rust vision run`.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum RunTask {
    /// Find objects: RF-DETR on the device (default `rf-detr-n`), or `sam-3.1` by concept
    Detect {
        #[command(flatten)]
        run: RunArgs,
        /// Classes to keep (comma-separated); concepts for `sam-3.1`
        #[arg(long, value_delimiter = ',', value_name = "NAMES")]
        classes: Vec<String>,
        /// The lowest score an RF-DETR backend keeps
        #[arg(long)]
        threshold: Option<f32>,
    },

    /// Segment objects: EfficientSAM on the device (default `efficientsam-ti`), SAM 2.1, or
    /// `sam-3.1` by concept
    Segment {
        #[command(flatten)]
        run: RunArgs,
        /// A box prompt in source pixels, `x0,y0,x1,y1`
        #[arg(long = "box", value_name = "X0,Y0,X1,Y1")]
        boxes: Vec<String>,
        /// A point on the object in source pixels, `x,y`
        #[arg(long = "point", value_name = "X,Y")]
        points: Vec<String>,
        /// A noun phrase naming one concept (`sam-3.1`)
        #[arg(long = "concept", value_name = "TEXT")]
        concepts: Vec<String>,
    },

    /// Read a document page: PP-OCRv6 on the device (default `pp-ocrv6-tiny`),
    /// PaddleOCR-VL-1.6, or NVIDIA NIM
    Read {
        #[command(flatten)]
        run: RunArgs,
        /// Also render the page as Markdown, when the backend can
        #[arg(long)]
        markdown: bool,
    },

    /// Ask a vision-language model about the image: `claude` (default), `gemini` or `nim`
    Describe {
        #[command(flatten)]
        run: RunArgs,
        /// The question
        #[arg(long, default_value = "Describe this image.")]
        question: String,
        /// The model (default: `claude-haiku-5-5`, `gemini-3.7-flash`, or
        /// `nvidia/nemotron-3.5-super-vl-120b-a12b`)
        #[arg(long)]
        model: Option<String>,
        /// Also ask for regions, mapped to source pixels
        #[arg(long)]
        regions: bool,
    },
}

/// What every `vision run` takes.
#[derive(clap::Args, Debug, Clone)]
pub struct RunArgs {
    /// The image: PNG, JPEG, WebP or GIF
    #[arg(value_name = "IMAGE")]
    pub image: PathBuf,
    /// The backend id (default: the on-device one)
    #[arg(long)]
    pub backend: Option<String>,
    /// table, json or regions
    #[arg(long, value_enum, default_value_t = RunFormat::Table)]
    pub output: RunFormat,
    /// Write a copy of the image with the regions drawn on it
    #[arg(long, value_name = "PNG")]
    pub annotate: Option<PathBuf>,
    /// Only this region of the image, `x0,y0,x1,y1` in source pixels; a remote backend is
    /// sent nothing else
    #[arg(long, value_name = "X0,Y0,X1,Y1")]
    pub crop: Option<String>,
    /// The data class the egress rules are written per (default: `image`, or `image_crop`
    /// with --crop)
    #[arg(long, value_name = "CLASS")]
    pub data_class: Option<String>,
    /// Release this egress digest, shown when the call was held, without asking again
    #[arg(long, value_name = "DIGEST")]
    pub approve: Vec<String>,
    /// The egress rules (default: <project>/.adk/vision.toml)
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// A remote backend's base URL: NVIDIA NIM (also `NVIDIA_NIM_URL` for describing,
    /// `NVIDIA_NIM_CV_URL` for reading), or a proxy
    #[arg(long, value_name = "URL")]
    pub base_url: Option<String>,
    #[command(flatten)]
    pub manifests: ManifestArgs,
    #[command(flatten)]
    pub masks: MaskDecoderArgs,
}

/// How `sam-3.1` masks are decoded; the flags override `[sam.masks]` of the rules file.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct MaskDecoderArgs {
    /// `sam-3.1` masks: boxes (no decoder), external (a program you installed) or licensed
    /// (the port of Meta's codec under the SAM License: feature `vision-sam-codec`, after
    /// `adk-rust vision models accept sam-mask-codec`). Default: `[sam.masks]` of the rules
    /// file, else boxes
    #[arg(long, value_enum, value_name = "DECODER")]
    pub mask_decoder: Option<MaskDecoderKind>,
    /// The external decoder: reads mask JSON on standard input, writes base64 rasters
    #[arg(long, value_name = "PROGRAM")]
    pub mask_decoder_program: Option<PathBuf>,
    /// An argument for the external decoder; repeat it for each
    #[arg(long = "mask-decoder-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub mask_decoder_args: Vec<String>,
    /// Seconds the external decoder may take for one answer (default 60)
    #[arg(long, value_name = "SECS")]
    pub mask_decoder_timeout: Option<u64>,
}

/// `--mask-decoder`.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskDecoderKind {
    /// Each object is its box
    Boxes,
    /// A program you installed decodes each answer's masks
    External,
    /// The port of Meta's codec, in this process (SAM License)
    Licensed,
}

/// `adk-rust vision egress`.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum EgressCommand {
    /// Every release and refusal: digests, destination, data class, redactions, decision
    Log {
        /// Only entries since this instant (RFC 3339) or day (YYYY-MM-DD)
        #[arg(long)]
        since: Option<String>,
        /// table or json
        #[arg(long, value_enum, default_value_t = ListFormat::Table)]
        output: ListFormat,
    },
}

/// Runs `adk-rust vision`.
pub async fn run(args: VisionArgs) -> Result<()> {
    let project = match args.project {
        Some(dir) => dir,
        None => std::env::current_dir().context("the current directory is unreadable")?,
    };
    match args.command {
        VisionCommands::Models { command } => models(&project, command).await,
        VisionCommands::Run { task } => run_task(&project, *task).await,
        VisionCommands::Egress { command: EgressCommand::Log { since, output } } => {
            egress_log(&project, since.as_deref(), output)
        }
    }
}

// --- models ----------------------------------------------------------------------------

fn manifest_set(args: &ManifestArgs) -> Result<ManifestSet> {
    let mut set = ManifestSet::builtin()?;
    for path in &args.manifest {
        set = if path.is_dir() {
            set.with_dir(path)?
        } else {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("could not read {}", path.display()))?;
            set.with(ModelManifest::from_json(&text)?)?
        };
    }
    Ok(set)
}

fn acceptances(project: &Path) -> Arc<FileAcceptanceStore> {
    Arc::new(FileAcceptanceStore::in_project(project))
}

fn fetcher(project: &Path, offline: Option<bool>) -> ModelFetcher {
    let mut config = FetchConfig::from_env();
    if let Some(offline) = offline {
        config = config.with_offline(offline);
    }
    ModelFetcher::new(config, acceptances(project))
}

fn lookup<'a>(set: &'a ManifestSet, id: &str) -> Result<&'a ModelManifest> {
    set.get(id).ok_or_else(|| {
        anyhow!(
            "no model `{id}` in the manifest; `adk-rust vision models list` shows the ids, and \
             --manifest adds your own"
        )
    })
}

/// Whether every file of `manifest` is in the cache, without opening a socket.
fn cached(fetcher: &ModelFetcher, manifest: &ModelManifest) -> bool {
    match &manifest.source {
        ModelSource::HuggingFace { repo, revision } => {
            manifest.files.iter().all(|file| fetcher.cached(repo, revision, &file.path).is_some())
        }
        ModelSource::Local { dir } => {
            manifest.files.iter().all(|file| dir.join(&file.path).exists())
        }
        _ => false,
    }
}

fn accepted(store: &dyn AcceptanceStore, manifest: &ModelManifest) -> bool {
    store.acceptances().unwrap_or_default().iter().any(|a| {
        a.model == manifest.id
            && a.licence_id == manifest.licence.id
            && a.licence_url == manifest.licence.url
    })
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

fn acceptance_state(store: &dyn AcceptanceStore, manifest: &ModelManifest) -> &'static str {
    match manifest.licence.class {
        LicenceClass::UseRestricted if accepted(store, manifest) => "accepted",
        LicenceClass::UseRestricted => "required",
        _ => "-",
    }
}

async fn models(project: &Path, command: ModelsCommand) -> Result<()> {
    match command {
        ModelsCommand::List { manifests, output } => {
            let set = manifest_set(&manifests)?;
            let fetcher = fetcher(project, Some(true));
            let store = acceptances(project);
            if output == ListFormat::Json {
                let rows: Vec<Value> = set
                    .iter()
                    .map(|m| {
                        json!({
                            "id": m.id,
                            "task": m.task,
                            "licenceClass": m.licence.class,
                            "licence": m.licence.id,
                            "licenceUrl": m.licence.url,
                            "bytes": m.files.iter().map(|f| f.size).sum::<u64>(),
                            "cached": cached(&fetcher, m),
                            "acceptance": acceptance_state(store.as_ref(), m),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
                return Ok(());
            }
            println!(
                "{:<38} {:<8} {:<15} {:<12} {:>10}  {:<7} acceptance",
                "id", "task", "licence class", "licence", "size", "cached"
            );
            for m in set.iter() {
                println!(
                    "{:<38} {:<8} {:<15} {:<12} {:>10}  {:<7} {}",
                    m.id,
                    task_name(m.task),
                    m.licence.class.to_string(),
                    m.licence.id,
                    megabytes(m.files.iter().map(|f| f.size).sum()),
                    if cached(&fetcher, m) { "yes" } else { "no" },
                    acceptance_state(store.as_ref(), m),
                );
            }
            Ok(())
        }
        ModelsCommand::Fetch { ids, manifests } => {
            let set = manifest_set(&manifests)?;
            let fetcher = fetcher(project, None);
            for id in &ids {
                let manifest = lookup(&set, id)?;
                let size: u64 = manifest.files.iter().map(|f| f.size).sum();
                eprintln!("fetching {id} ({}, {})", megabytes(size), manifest.licence.id);
                let verified = fetcher.fetch(manifest).await?;
                for file in verified.files() {
                    println!("{id}  {}  {}", file.digest(), file.path().display());
                }
            }
            Ok(())
        }
        ModelsCommand::Verify { ids, manifests } => {
            let set = manifest_set(&manifests)?;
            let fetcher = fetcher(project, Some(true));
            let targets: Vec<&ModelManifest> = if ids.is_empty() {
                set.iter().filter(|m| cached(&fetcher, m)).collect()
            } else {
                ids.iter().map(|id| lookup(&set, id)).collect::<Result<_>>()?
            };
            if targets.is_empty() {
                println!("no model is cached; fetch one with `adk-rust vision models fetch <id>`");
                return Ok(());
            }
            let mut failed = 0;
            for manifest in targets {
                match fetcher.fetch(manifest).await {
                    Ok(verified) => {
                        println!("ok    {} ({} files)", manifest.id, verified.files().count());
                    }
                    Err(err) => {
                        failed += 1;
                        println!("FAIL  {}: {err}", manifest.id);
                    }
                }
            }
            if failed > 0 {
                bail!("{failed} model(s) failed verification");
            }
            Ok(())
        }
        ModelsCommand::Accept { id, manifests } => {
            let set = manifest_set(&manifests)?;
            accept(project, lookup(&set, &id)?)
        }
    }
}

fn task_name(task: VisionTask) -> &'static str {
    match task {
        VisionTask::Detect => "detect",
        VisionTask::Segment => "segment",
        VisionTask::Read => "read",
        VisionTask::Embed => "embed",
        VisionTask::Describe => "describe",
        VisionTask::AnalyzeVideo => "video",
        VisionTask::Edit => "edit",
        _ => "other",
    }
}

fn person() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "user".into())
}

fn accept(project: &Path, manifest: &ModelManifest) -> Result<()> {
    let licence = &manifest.licence;
    match licence.class {
        LicenceClass::Permissive => {
            println!(
                "{} is {} ({}): no acceptance is needed",
                manifest.id, licence.id, licence.class
            );
            return Ok(());
        }
        LicenceClass::UseRestricted => {}
        _ => bail!(
            "{} is {} ({}): acceptance does not cover non-commercial or copyleft weights; name \
             the model in your `LicencePolicy` if your use is allowed",
            manifest.id,
            licence.id,
            licence.class
        ),
    }
    let store = acceptances(project);
    if accepted(store.as_ref(), manifest) {
        println!("{}: your acceptance of {} is already recorded", manifest.id, licence.id);
        return Ok(());
    }
    println!("{} is under {} ({}).", manifest.id, licence.id, licence.class);
    println!("Read the licence before you accept it: {}", licence.url);
    if !std::io::stdin().is_terminal() {
        bail!(
            "licence acceptance is recorded only from a terminal, by a person (spec 3.7 V-I11); \
             run this command interactively"
        );
    }
    print!("Type the model id to accept its licence as {}: ", person());
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if answer.trim() != manifest.id {
        bail!("not accepted");
    }
    store.record(LicenceAcceptance::of(manifest, person()))?;
    println!("accepted; recorded in {}", project.join(STATE_DIR).join("licences.json").display());
    Ok(())
}

// --- run -------------------------------------------------------------------------------

/// Appends each egress record to the project's `.adk/vision/egress.jsonl`.
struct JsonlEgressRecorder {
    path: PathBuf,
}

#[async_trait::async_trait]
impl EgressRecorder for JsonlEgressRecorder {
    async fn record(&self, record: EgressRecord) -> adk_vision::Result<()> {
        let line = serde_json::to_string(&record)
            .map_err(|e| VisionError::InvalidInput(format!("egress record: {e}")))?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            writeln!(file, "{line}")?;
            file.sync_data()
        })
        .await
        .map_err(|e| VisionError::InvalidInput(format!("egress log: {e}")))?
        .map_err(|source| VisionError::Io {
            action: "could not append to",
            path: self.path.clone(),
            source,
        })
    }
}

/// The backend a call names, or its task's on-device default.
fn backend_id(task: &RunTask) -> String {
    let default = match task {
        RunTask::Detect { .. } => "rf-detr-n",
        RunTask::Segment { .. } => "efficientsam-ti",
        RunTask::Read { .. } => "pp-ocrv6-tiny",
        RunTask::Describe { .. } => "claude",
    };
    run_args(task).backend.clone().unwrap_or_else(|| default.to_string())
}

fn run_args(task: &RunTask) -> &RunArgs {
    match task {
        RunTask::Detect { run, .. }
        | RunTask::Segment { run, .. }
        | RunTask::Read { run, .. }
        | RunTask::Describe { run, .. } => run,
    }
}

fn parse_numbers<const N: usize>(text: &str, what: &str) -> Result<[f32; N]> {
    let values: Vec<f32> = text
        .split(',')
        .map(|v| v.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| anyhow!("{what} `{text}` is not {N} comma-separated numbers"))?;
    values.try_into().map_err(|_| anyhow!("{what} `{text}` is not {N} comma-separated numbers"))
}

fn parse_box(text: &str) -> Result<BBox> {
    let [x0, y0, x1, y1] = parse_numbers::<4>(text, "box")?;
    Ok(BBox::new(x0, y0, x1, y1))
}

/// The egress rules: the file when it exists, else every release asks. Faces and personal
/// text are always redacted, as the harness vision pack redacts them.
fn rules(project: &Path, config: Option<&Path>) -> Result<VisionConfig> {
    let path = config.map_or_else(|| project.join(RULES_FILE), Path::to_path_buf);
    let mut rules = match std::fs::read_to_string(&path) {
        Ok(text) => VisionConfig::from_toml_str(&text)
            .with_context(|| format!("{} is invalid", path.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && config.is_none() => {
            VisionConfig::default()
        }
        Err(err) => return Err(err).with_context(|| format!("could not read {}", path.display())),
    };
    rules.egress =
        rules.egress.redacting(RedactionClass::faces()).redacting(RedactionClass::personal_text());
    Ok(rules)
}

fn key(names: &[&str]) -> Result<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .ok_or_else(|| anyhow!("set {} to call this backend", names.join(" or ")))
}

/// Registers the one backend the call names.
async fn register(
    builder: VisionRuntimeBuilder,
    project: &Path,
    task: &RunTask,
    id: &str,
    set: &ManifestSet,
    fetcher: &ModelFetcher,
) -> Result<VisionRuntimeBuilder> {
    let base_url = run_args(task).base_url.clone();
    match (task, id) {
        (RunTask::Detect { .. }, "sam-3.1") => {
            Ok(builder.remote_detector(Arc::new(sam(project, run_args(task))?)))
        }
        (RunTask::Segment { .. }, "sam-3.1") => {
            Ok(builder.remote_segmenter(Arc::new(sam(project, run_args(task))?)))
        }
        (RunTask::Read { .. }, "nim") => {
            let url =
                base_url.or_else(|| std::env::var("NVIDIA_NIM_CV_URL").ok()).ok_or_else(|| {
                    anyhow!(
                        "NVIDIA NIM has no default URL: pass --base-url or set NVIDIA_NIM_CV_URL \
                         (hosted: https://ai.api.nvidia.com/v1/cv, under NVIDIA's API Trial \
                         Terms; or your own NIM)"
                    )
                })?;
            let config = adk_vision::NimCvConfig::new(url).with_api_key_from_env();
            Ok(builder.remote_reader(Arc::new(adk_vision::NimDocumentReader::new(config)?)))
        }
        (RunTask::Describe { model, .. }, backend) => {
            Ok(builder.remote_describer(Arc::new(describer(backend, model.as_deref(), base_url)?)))
        }
        _ => local(builder, task, id, set, fetcher).await,
    }
}

/// The mask decoder a call uses: the flags when they choose, else `[sam.masks]`.
fn mask_choice(args: &MaskDecoderArgs, file: MaskDecoderChoice) -> Result<MaskDecoderChoice> {
    let external = |program: PathBuf, args_from_file: Vec<String>, timeout_from_file: u64| {
        MaskDecoderChoice::External {
            program,
            args: if args.mask_decoder_args.is_empty() {
                args_from_file
            } else {
                args.mask_decoder_args.clone()
            },
            timeout_secs: args.mask_decoder_timeout.unwrap_or(timeout_from_file),
        }
    };
    let (file_program, file_args, file_timeout) = match &file {
        MaskDecoderChoice::External { program, args, timeout_secs } => {
            (Some(program.clone()), args.clone(), *timeout_secs)
        }
        _ => (None, Vec::new(), adk_vision::DEFAULT_MASK_DECODER_TIMEOUT_SECS),
    };
    let kind = match args.mask_decoder {
        Some(kind) => kind,
        None if args.mask_decoder_program.is_some() => MaskDecoderKind::External,
        None if matches!(file, MaskDecoderChoice::External { .. }) => MaskDecoderKind::External,
        None => return Ok(file),
    };
    Ok(match kind {
        MaskDecoderKind::Boxes => MaskDecoderChoice::None,
        MaskDecoderKind::Licensed => MaskDecoderChoice::Licensed,
        MaskDecoderKind::External => {
            let program = args.mask_decoder_program.clone().or(file_program).ok_or_else(|| {
                anyhow!(
                    "--mask-decoder external needs --mask-decoder-program, or `program` in the \
                     `[sam.masks]` table of {RULES_FILE}"
                )
            })?;
            external(program, file_args, file_timeout)
        }
    })
}

fn sam(project: &Path, run: &RunArgs) -> Result<adk_vision::SamSegmenter> {
    let mut config = adk_vision::SamConfig::from_env()?;
    if let Some(url) = &run.base_url {
        config = config.with_base_url(url.clone());
    }
    let masks = mask_choice(&run.masks, rules(project, run.config.as_deref())?.sam.masks)?;
    let config = config.with_mask_decoder(masks).with_licence_acceptances(acceptances(project));
    Ok(adk_vision::SamSegmenter::new(config)?)
}

fn describer(
    backend: &str,
    model: Option<&str>,
    base_url: Option<String>,
) -> Result<adk_vision::LlmDescriber> {
    use adk_core::Llm;
    let (llm, host): (Arc<dyn Llm>, &str) = match backend {
        "claude" => {
            let mut config = adk_model::anthropic::AnthropicConfig::new(
                key(&["ANTHROPIC_API_KEY"])?,
                model.unwrap_or(CLAUDE_MODEL),
            );
            if let Some(url) = base_url {
                config = config.with_base_url(url);
            }
            (Arc::new(adk_model::AnthropicClient::new(config)?), "api.anthropic.com")
        }
        "gemini" => {
            let api_key = key(&["GEMINI_API_KEY", "GOOGLE_API_KEY"])?;
            let model = model.unwrap_or(GEMINI_MODEL);
            let client = match base_url {
                Some(url) => adk_model::GeminiModel::new_with_base_url(api_key, model, url)?,
                None => adk_model::GeminiModel::new(api_key, model)?,
            };
            (Arc::new(client), "generativelanguage.googleapis.com")
        }
        "nim" => {
            let url =
                base_url.or_else(|| std::env::var("NVIDIA_NIM_URL").ok()).ok_or_else(|| {
                    anyhow!(
                        "NVIDIA NIM has no default URL: pass --base-url or set NVIDIA_NIM_URL \
                     (hosted: https://integrate.api.nvidia.com/v1, under NVIDIA's API Trial \
                     Terms; or your own NIM)"
                    )
                })?;
            let config = adk_vision::cloud::nim::NimConfig::new(
                url,
                key(&["NVIDIA_API_KEY"])?,
                model.unwrap_or(NIM_VLM_MODEL),
            );
            return Ok(config.describer()?);
        }
        other => bail!(
            "no describing backend `{other}`: use claude, gemini or nim. The CLI has no \
             on-device describer; in a program, `LlmDescriber::mistralrs` (adk-vision feature \
             `mistralrs`) runs Qwen3-VL in the process"
        ),
    };
    Ok(adk_vision::LlmDescriber::remote(llm, [host])?)
}

#[cfg(feature = "vision-local")]
async fn local(
    builder: VisionRuntimeBuilder,
    task: &RunTask,
    id: &str,
    set: &ManifestSet,
    fetcher: &ModelFetcher,
) -> Result<VisionRuntimeBuilder> {
    use adk_vision::onnx::{EfficientSam, OnnxOptions, RfDetr, Sam2};
    use adk_vision::{PpOcrReader, PpOcrV6Size};

    let options = OnnxOptions::default();
    match task {
        RunTask::Read { .. } => {
            let reader: Arc<dyn adk_vision::DocumentReader> = match id {
                "pp-ocrv6-tiny" => Arc::new(PpOcrReader::fetch(fetcher, PpOcrV6Size::Tiny).await?),
                "pp-ocrv6-small" => {
                    Arc::new(PpOcrReader::fetch(fetcher, PpOcrV6Size::Small).await?)
                }
                "pp-ocrv6-medium" => {
                    Arc::new(PpOcrReader::fetch(fetcher, PpOcrV6Size::Medium).await?)
                }
                "paddleocr-vl-1.6" => Arc::new(
                    adk_vision::PaddleOcrVlReader::fetch(fetcher, adk_vision::VlDevice::Cpu)
                        .await?,
                ),
                other => bail!(
                    "no reading backend `{other}`: use pp-ocrv6-tiny, pp-ocrv6-small, \
                     pp-ocrv6-medium, paddleocr-vl-1.6 or nim"
                ),
            };
            Ok(builder.reader(reader))
        }
        RunTask::Detect { threshold, .. } => {
            let manifest = model_for(set, id, VisionTask::Detect)?;
            let mut detector = RfDetr::new(&fetcher.fetch(manifest).await?, &options)?;
            if let Some(threshold) = threshold {
                detector = detector.with_threshold(*threshold);
            }
            Ok(builder.detector(Arc::new(detector)))
        }
        RunTask::Segment { .. } => {
            let manifest = model_for(set, id, VisionTask::Segment)?;
            let model = fetcher.fetch(manifest).await?;
            let segmenter: Arc<dyn adk_vision::Segmenter> = if id.starts_with("sam2") {
                Arc::new(Sam2::new(&model, &options)?)
            } else {
                Arc::new(EfficientSam::new(&model, &options)?)
            };
            Ok(builder.segmenter(segmenter))
        }
        RunTask::Describe { .. } => bail!("describing backends are remote"),
    }
}

#[cfg(feature = "vision-local")]
fn model_for<'a>(set: &'a ManifestSet, id: &str, task: VisionTask) -> Result<&'a ModelManifest> {
    let manifest = lookup(set, id)?;
    if manifest.task != task {
        bail!(
            "model `{id}` is a {} model, not a {} one",
            task_name(manifest.task),
            task_name(task)
        );
    }
    Ok(manifest)
}

#[cfg(not(feature = "vision-local"))]
async fn local(
    _builder: VisionRuntimeBuilder,
    _task: &RunTask,
    id: &str,
    _set: &ManifestSet,
    _fetcher: &ModelFetcher,
) -> Result<VisionRuntimeBuilder> {
    bail!(
        "`{id}` runs on the device, and this adk-rust was built without its on-device backends: \
         install it with feature `vision-local` (umbrella `adk-rust/vision-local`), or name a \
         remote backend with --backend"
    )
}

/// The egress gate of the project's rules, with the on-device redactors the rules need.
async fn gate(
    project: &Path,
    config: Option<&Path>,
    fetcher: &ModelFetcher,
    set: &ManifestSet,
) -> Result<EgressGate> {
    let rules = rules(project, config)?;
    let state = project.join(STATE_DIR);
    let artifacts = adk_artifact::FileArtifactService::new(state.join("media"))?;
    let recorder = Arc::new(JsonlEgressRecorder { path: state.join("egress.jsonl") });
    let gate = EgressGate::new(EgressPolicy::public_only(), rules.egress, recorder)
        .with_media_store(MediaStore::new(Arc::new(artifacts)));
    redactors(gate, fetcher, set).await
}

#[cfg(feature = "vision-local")]
async fn redactors(
    mut gate: EgressGate,
    fetcher: &ModelFetcher,
    set: &ManifestSet,
) -> Result<EgressGate> {
    use adk_vision::onnx::{OnnxOptions, RfDetr};
    use adk_vision::{PersonRedactor, PersonalTextRedactor, PpOcrReader, PpOcrV6Size};

    let classes = gate.redaction_classes();
    if classes.contains(&RedactionClass::faces()) {
        let model = fetcher.fetch(lookup(set, PERSON_DETECTOR)?).await.with_context(|| {
            format!(
                "the face redactor needs `{PERSON_DETECTOR}`; fetch it with `adk-rust vision \
                 models fetch {PERSON_DETECTOR}`"
            )
        })?;
        // The redactor covers every person scoring 0.25 or more; RF-DETR reports from 0.5 by
        // default, so the detector reports down to the redactor's floor.
        let detector = RfDetr::new(&model, &OnnxOptions::default())?.with_threshold(0.25);
        gate = gate.with_redactor(Arc::new(PersonRedactor::new(Arc::new(detector))));
    }
    if classes.contains(&RedactionClass::personal_text()) {
        let reader = PpOcrReader::fetch(fetcher, PpOcrV6Size::Tiny).await.context(
            "the personal-text redactor needs PP-OCRv6 tiny; fetch it with `adk-rust vision \
             models fetch pp-ocrv6-tiny-det pp-ocrv6-tiny-rec`",
        )?;
        gate = gate.with_redactor(Arc::new(PersonalTextRedactor::new(Arc::new(reader))));
    }
    Ok(gate)
}

/// Without the on-device backends there is no redactor: a release the rules require one for
/// is refused at the gate's redact step.
#[cfg(not(feature = "vision-local"))]
async fn redactors(
    gate: EgressGate,
    _fetcher: &ModelFetcher,
    _set: &ManifestSet,
) -> Result<EgressGate> {
    Ok(gate)
}

/// One call's result, ready to print.
struct Outcome {
    source: SourceImage,
    regions: Vec<Region>,
    value: Value,
    calls: Vec<CallRecord>,
    lines: Vec<String>,
}

async fn run_task(project: &Path, task: RunTask) -> Result<()> {
    let args = run_args(&task).clone();
    let bytes = std::fs::read(&args.image)
        .with_context(|| format!("could not read {}", args.image.display()))?;
    let source = SourceImage::decode(&bytes, &DecodeLimits::default())?;
    let set = manifest_set(&args.manifests)?;
    let fetcher = fetcher(project, None);
    let id = backend_id(&task);
    let remote =
        matches!(task, RunTask::Describe { .. }) || matches!(id.as_str(), "sam-3.1" | "nim");
    let crop = args.crop.as_deref().map(parse_box).transpose()?;
    let class = args.data_class.clone().unwrap_or_else(|| {
        if crop.is_some() { "image_crop".to_string() } else { "image".to_string() }
    });
    let mut options = CallOptions::new().with_data_class(&class);
    if let Some(crop) = crop {
        options = options.with_region(crop);
    }
    for digest in &args.approve {
        options = options
            .approving(digest.parse::<Digest>().map_err(|e| anyhow!("--approve `{digest}`: {e}"))?);
    }
    // A remote call is released only through the gate; a local one never reaches it.
    let mut builder =
        register(VisionRuntime::builder(), project, &task, &id, &set, &fetcher).await?;
    let gate = if remote {
        let gate = Arc::new(gate(project, args.config.as_deref(), &fetcher, &set).await?);
        builder = builder.shared_egress_gate(gate.clone());
        Some(gate)
    } else {
        None
    };
    let runtime = builder.build()?;
    let vision_task = match &task {
        RunTask::Detect { .. } => VisionTask::Detect,
        RunTask::Segment { .. } => VisionTask::Segment,
        RunTask::Read { .. } => VisionTask::Read,
        RunTask::Describe { .. } => VisionTask::Describe,
    };
    let backend = runtime.backends().into_iter().next().ok_or_else(|| anyhow!("no backend"))?;
    let descriptor = runtime
        .descriptor(vision_task, &backend)
        .cloned()
        .ok_or_else(|| anyhow!("backend `{backend}` is not registered for this task"))?;
    let cx = VisionCx::new().with_media_scope(MediaScope::new("adk-rust-vision", person(), "cli"));
    let mut asked = false;
    let outcome = loop {
        match call(&runtime, &cx, &task, &source, &options).await {
            Err(VisionError::EgressHeld { host, data_class, egress }) => {
                let Some(gate) = &gate else { bail!("an on-device call was held") };
                let mut request = ReleaseRequest::new(&source, &descriptor, DataClass::new(&class));
                if let Some(crop) = crop {
                    request = request.with_crop(crop);
                }
                let preview = gate.preview(&cx, &request).await?;
                let held = project.join(STATE_DIR).join("held");
                std::fs::create_dir_all(&held)?;
                let path = held.join(format!("{}.png", egress.to_string().replace(':', "-")));
                std::fs::write(&path, preview.bytes())?;
                eprintln!("held: this image would be sent to {host} as {data_class}");
                eprintln!("  redacted: {}", summarise(preview.redactions()));
                eprintln!("  egress digest: {egress}");
                eprintln!("  the exact image: {}", path.display());
                if asked || !std::io::stdin().is_terminal() {
                    eprintln!(
                        "not sent; to release exactly this image, run again with --approve {egress}"
                    );
                    std::process::exit(2);
                }
                asked = true;
                eprint!("send this image to {host}? [y/N] ");
                std::io::stderr().flush()?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim(), "y" | "Y" | "yes") {
                    eprintln!("not sent");
                    std::process::exit(2);
                }
                options = options.approving(egress);
            }
            Err(err) => return Err(err.into()),
            Ok(outcome) => break outcome,
        }
    };
    print_outcome(&outcome, &args, gate.is_some())?;
    if let Some(path) = &args.annotate {
        annotate(&outcome, path)?;
        eprintln!("annotated copy: {}", path.display());
    }
    Ok(())
}

async fn call(
    runtime: &VisionRuntime,
    cx: &VisionCx,
    task: &RunTask,
    source: &SourceImage,
    options: &CallOptions,
) -> adk_vision::Result<Outcome> {
    let finish = |regions: Vec<Region>, value: Value, calls, lines| Outcome {
        source: source.clone(),
        regions,
        value,
        calls,
        lines,
    };
    let invalid = |e: anyhow::Error| VisionError::InvalidInput(e.to_string());
    match task {
        RunTask::Detect { classes, .. } => {
            let query = if run_args(task).backend.as_deref() == Some("sam-3.1") {
                DetectQuery::Concepts(classes.clone())
            } else {
                DetectQuery::Classes(classes.clone())
            };
            let out = runtime.detect_with(cx, source, &query, options).await?;
            let value = serde_json::to_value(&out.value).unwrap_or_default();
            Ok(finish(out.value, value, out.calls, Vec::new()))
        }
        RunTask::Segment { boxes, points, concepts, .. } => {
            let mut prompts = Vec::new();
            for b in boxes {
                prompts.push(SegmentPrompt::Box { bbox: parse_box(b).map_err(invalid)? });
            }
            for p in points {
                let at = parse_numbers::<2>(p, "point").map_err(invalid)?;
                prompts.push(SegmentPrompt::Point { at, positive: true });
            }
            for text in concepts {
                prompts.push(SegmentPrompt::Concept { text: text.clone() });
            }
            if prompts.is_empty() {
                return Err(VisionError::InvalidInput(
                    "segment needs a prompt: --box, --point or --concept".into(),
                ));
            }
            let out = runtime.segment_with(cx, source, &prompts, options).await?;
            let value = serde_json::to_value(&out.value).unwrap_or_default();
            Ok(finish(out.value, value, out.calls, Vec::new()))
        }
        RunTask::Read { markdown, .. } => {
            let mut read = ReadOptions::default();
            read.markdown = *markdown;
            let out = runtime.read_with(cx, source, &read, options).await?;
            let value = serde_json::to_value(&out.value).unwrap_or_default();
            let lines = page_lines(&out.value);
            let regions = out.value.blocks.iter().map(|b| b.region.clone()).collect();
            Ok(finish(regions, value, out.calls, lines))
        }
        RunTask::Describe { question, regions, .. } => {
            let mut spec = DescribeSpec::new(question.clone());
            if *regions {
                spec = spec.with_regions();
            }
            let out = runtime.describe_with(cx, &[source], &spec, options).await?;
            let value = serde_json::to_value(&out.value).unwrap_or_default();
            let mut lines: Vec<String> =
                out.value.text.iter().flat_map(|t| t.lines().map(str::to_string)).collect();
            if let Some(v) = &out.value.value {
                lines.push(v.to_string());
            }
            Ok(finish(out.value.regions, value, out.calls, lines))
        }
    }
}

fn page_lines(page: &DocumentPage) -> Vec<String> {
    let order: Vec<usize> = if page.reading_order.is_empty() {
        (0..page.blocks.len()).collect()
    } else {
        page.reading_order.clone()
    };
    order
        .into_iter()
        .filter_map(|i| page.blocks.get(i))
        .map(|block| {
            let bbox = block.region.shape.bbox().map(fmt_box).unwrap_or_default();
            let confidence =
                block.confidence.map_or_else(|| "    ".to_string(), |c| format!("{c:.2}"));
            let escalated = block
                .region
                .origin
                .escalated_from
                .as_ref()
                .map(|e| format!("  (escalated from {})", e.backend))
                .unwrap_or_default();
            format!("{confidence}  {bbox:<22}  {}{escalated}", block.text)
        })
        .collect()
}

fn fmt_box(b: BBox) -> String {
    format!("{:.0},{:.0},{:.0},{:.0}", b.x0, b.y0, b.x1, b.y1)
}

fn print_outcome(outcome: &Outcome, args: &RunArgs, remote: bool) -> Result<()> {
    match args.output {
        RunFormat::Json => {
            let value = json!({ "value": outcome.value, "calls": outcome.calls });
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        RunFormat::Regions => {
            let (width, height) = outcome.source.size();
            let set =
                RegionSet::new(outcome.source.digest(), width, height, outcome.regions.clone());
            println!("{}", serde_json::to_string_pretty(&set)?);
        }
        RunFormat::Table => {
            if outcome.lines.is_empty() {
                println!("{:<4} {:<20} {:>6}  {:<22}  backend", "#", "label", "score", "box");
                for (i, region) in outcome.regions.iter().enumerate() {
                    println!(
                        "{:<4} {:<20} {:>6}  {:<22}  {}",
                        i + 1,
                        region.label.as_deref().unwrap_or("-"),
                        region.score.map_or_else(|| "-".to_string(), |s| format!("{s:.2}")),
                        region.shape.bbox().map(fmt_box).unwrap_or_default(),
                        region.origin.backend,
                    );
                }
            } else {
                for line in &outcome.lines {
                    println!("{line}");
                }
            }
            for call in &outcome.calls {
                let locality = match &call.locality {
                    Locality::Remote { hosts } => format!("remote: {}", hosts.join(", ")),
                    Locality::Loopback => "loopback".to_string(),
                    _ => "on the device".to_string(),
                };
                let cost = match call.cost_micro_usd {
                    Some(micro) => format!("${:.4}", micro as f64 / 1_000_000.0),
                    None => "billed by tokens".to_string(),
                };
                let sent = if call.egress.is_empty() {
                    "nothing left the machine".to_string()
                } else {
                    let digests: Vec<String> = call.egress.iter().map(Digest::to_string).collect();
                    format!("released {}", digests.join(", "))
                };
                eprintln!(
                    "{} {} ({locality}) · {} ms · {} region(s) · {cost} · {sent}",
                    task_name(call.task),
                    call.backend,
                    call.duration_ms,
                    call.regions,
                );
            }
            if remote {
                eprintln!("what left the machine: adk-rust vision egress log");
            }
        }
    }
    Ok(())
}

fn annotate(outcome: &Outcome, path: &Path) -> Result<()> {
    let colours = [Rgba::RED, Rgba::GREEN, Rgba::BLUE];
    let overlays: Vec<Overlay> = outcome
        .regions
        .iter()
        .enumerate()
        .flat_map(|(i, region)| {
            let color = colours[i % colours.len()];
            let mut overlays = Vec::new();
            if let Shape::Mask(mask) = &region.shape {
                let [r, g, b, _] = color.0;
                overlays.push(Overlay::Mask { mask: mask.clone(), color: Rgba([r, g, b, 96]) });
            }
            if let Some(bbox) = region.shape.bbox() {
                overlays.push(Overlay::Box { bbox, color, thickness: 3 });
            }
            overlays
        })
        .collect();
    let pixels = outcome.source.pixels();
    let chain = TransformChain::new(pixels.width(), pixels.height());
    let drawn = apply_edit(pixels, &chain, &EditOp::Annotate { overlays })?;
    std::fs::write(path, drawn.encode_png()?)
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

// --- egress log ------------------------------------------------------------------------

fn since(text: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    if let Ok(instant) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(instant.with_timezone(&chrono::Utc));
    }
    let day = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| anyhow!("--since `{text}` is neither RFC 3339 nor YYYY-MM-DD"))?;
    Ok(day.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc())
}

fn egress_log(project: &Path, since_text: Option<&str>, output: ListFormat) -> Result<()> {
    let path = project.join(STATE_DIR).join("egress.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("could not read {}", path.display())),
    };
    let from = since_text.map(since).transpose()?;
    let records: Vec<EgressRecord> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<Vec<EgressRecord>, _>>()
        .with_context(|| format!("{} holds a line that is not an egress record", path.display()))?
        .into_iter()
        .filter(|record| {
            let at = match record {
                EgressRecord::Released(r) => r.released_at,
                EgressRecord::Refused(r) => r.refused_at,
                _ => return true,
            };
            from.is_none_or(|from| at >= from)
        })
        .collect();
    if output == ListFormat::Json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(());
    }
    if records.is_empty() {
        println!("nothing has left the machine from {}", project.display());
        return Ok(());
    }
    println!(
        "{:<20} {:<9} {:<18} {:<34} {:<12} {:<25} outcome",
        "when", "entry", "backend", "destination", "class", "egress digest"
    );
    for record in &records {
        match record {
            EgressRecord::Released(r) => {
                let decision = match r.decision {
                    adk_vision::EgressDecision::Approved { .. } => "approved by a person",
                    _ => "allowed by rule",
                };
                let cost = r
                    .estimated_cost_micro_usd
                    .map(|c| format!(", ~${:.4}", c as f64 / 1_000_000.0))
                    .unwrap_or_default();
                println!(
                    "{:<20} {:<9} {:<18} {:<34} {:<12} {:<25} {decision}; redacted {}{cost}",
                    r.released_at.format("%Y-%m-%d %H:%M:%S"),
                    "released",
                    r.backend.as_str(),
                    r.hosts.join(", "),
                    r.data_class.as_str(),
                    short(&r.egress),
                    summarise(&r.redactions),
                );
            }
            EgressRecord::Refused(r) => {
                println!(
                    "{:<20} {:<9} {:<18} {:<34} {:<12} {:<25} {} at {}",
                    r.refused_at.format("%Y-%m-%d %H:%M:%S"),
                    "refused",
                    r.backend.as_str(),
                    r.hosts.join(", "),
                    r.data_class.as_str(),
                    r.egress.as_ref().map_or_else(|| "-".to_string(), short),
                    r.reason,
                    r.step,
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn summarise(redactions: &BTreeMap<RedactionClass, u32>) -> String {
    if redactions.is_empty() {
        return "nothing".to_string();
    }
    redactions.iter().map(|(class, n)| format!("{n} {class}")).collect::<Vec<_>>().join(", ")
}

fn short(digest: &Digest) -> String {
    digest.to_string().chars().take(25).collect()
}

#[cfg(test)]
mod tests {
    use adk_vision::EgressAction;

    use super::*;

    #[test]
    fn rules_always_redact_faces_and_personal_text_and_default_to_ask() {
        let dir = tempfile::tempdir().unwrap();
        let config = rules(dir.path(), None).unwrap();
        assert!(config.egress.redact.contains(&RedactionClass::faces()));
        assert!(config.egress.redact.contains(&RedactionClass::personal_text()));
        assert_eq!(
            config.egress.decide("api.anthropic.com", &DataClass::new("image")).0,
            EgressAction::Ask
        );

        std::fs::create_dir_all(dir.path().join(".adk")).unwrap();
        std::fs::write(
            dir.path().join(RULES_FILE),
            "[egress]\ndefault = \"deny\"\nredact = []\n\n[[egress.rule]]\nhost = \
             \"api.anthropic.com\"\nclasses = [\"image_crop\"]\naction = \"allow\"\n",
        )
        .unwrap();
        let config = rules(dir.path(), None).unwrap();
        assert_eq!(
            config.egress.decide("api.anthropic.com", &DataClass::new("image_crop")).0,
            EgressAction::Allow
        );
        assert_eq!(
            config.egress.decide("api.anthropic.com", &DataClass::new("image")).0,
            EgressAction::Deny
        );
        assert!(config.egress.redact.contains(&RedactionClass::faces()));
    }

    #[test]
    fn an_unknown_describer_names_the_backends_and_the_in_process_one() {
        let error = describer("qwen3-vl", None, None).expect_err("an unknown backend").to_string();
        assert_eq!(
            error,
            "no describing backend `qwen3-vl`: use claude, gemini or nim. The CLI has no \
             on-device describer; in a program, `LlmDescriber::mistralrs` (adk-vision feature \
             `mistralrs`) runs Qwen3-VL in the process"
        );
    }

    #[test]
    fn a_missing_named_rules_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(rules(dir.path(), Some(&dir.path().join("nope.toml"))).is_err());
    }

    #[test]
    fn boxes_and_points_parse_from_commas() {
        assert_eq!(parse_box("1,2,30,40").unwrap(), BBox::new(1.0, 2.0, 30.0, 40.0));
        assert!(parse_box("1,2,3").is_err());
        assert_eq!(parse_numbers::<2>(" 5 , 6", "point").unwrap(), [5.0, 6.0]);
    }

    #[test]
    fn since_takes_a_day_or_an_instant() {
        assert_eq!(since("2026-11-19").unwrap().to_rfc3339(), "2026-11-19T00:00:00+00:00");
        assert_eq!(
            since("2026-11-19T10:00:00+02:00").unwrap().to_rfc3339(),
            "2026-11-19T08:00:00+00:00"
        );
        assert!(since("yesterday").is_err());
    }

    #[tokio::test]
    async fn the_egress_log_reads_what_the_recorder_appended() {
        let dir = tempfile::tempdir().unwrap();
        let recorder =
            JsonlEgressRecorder { path: dir.path().join(STATE_DIR).join("egress.jsonl") };
        let refusal = adk_vision::EgressRefusal {
            source: Digest::of(b"page"),
            egress: Some(Digest::of(b"crop")),
            backend: adk_vision::BackendId::new("claude-haiku-5-5"),
            hosts: vec!["api.anthropic.com".into()],
            data_class: DataClass::new("image_crop"),
            step: "decide".into(),
            reason: "held for a person's approval".into(),
            refused_at: chrono::Utc::now(),
        };
        recorder.record(EgressRecord::Refused(refusal.clone())).await.unwrap();
        recorder.record(EgressRecord::Refused(refusal)).await.unwrap();
        let text = std::fs::read_to_string(&recorder.path).unwrap();
        assert_eq!(text.lines().count(), 2);
        egress_log(dir.path(), Some("2000-01-01"), ListFormat::Table).unwrap();
        egress_log(dir.path(), None, ListFormat::Json).unwrap();
    }

    #[test]
    fn mask_decoder_flags_override_the_rules_file() {
        let file = MaskDecoderChoice::External {
            program: "/opt/sam/decode".into(),
            args: vec!["--json".into()],
            timeout_secs: 30,
        };
        let none = MaskDecoderArgs::default();
        assert_eq!(mask_choice(&none, MaskDecoderChoice::None).unwrap(), MaskDecoderChoice::None);
        assert_eq!(mask_choice(&none, file.clone()).unwrap(), file);
        let boxes =
            MaskDecoderArgs { mask_decoder: Some(MaskDecoderKind::Boxes), ..Default::default() };
        assert_eq!(mask_choice(&boxes, file.clone()).unwrap(), MaskDecoderChoice::None);
        let licensed =
            MaskDecoderArgs { mask_decoder: Some(MaskDecoderKind::Licensed), ..Default::default() };
        assert_eq!(mask_choice(&licensed, file.clone()).unwrap(), MaskDecoderChoice::Licensed);
        // A timeout alone keeps the file's program and arguments.
        let slower = MaskDecoderArgs { mask_decoder_timeout: Some(90), ..Default::default() };
        assert_eq!(
            mask_choice(&slower, file.clone()).unwrap(),
            MaskDecoderChoice::External {
                program: "/opt/sam/decode".into(),
                args: vec!["--json".into()],
                timeout_secs: 90,
            }
        );
        // A program alone chooses an external decoder with its own arguments.
        let program = MaskDecoderArgs {
            mask_decoder_program: Some("sam-decode".into()),
            mask_decoder_args: vec!["-q".into()],
            ..Default::default()
        };
        assert_eq!(
            mask_choice(&program, MaskDecoderChoice::None).unwrap(),
            MaskDecoderChoice::external("sam-decode", ["-q"])
        );
        let external =
            MaskDecoderArgs { mask_decoder: Some(MaskDecoderKind::External), ..Default::default() };
        let err = mask_choice(&external, MaskDecoderChoice::None).unwrap_err().to_string();
        assert!(err.contains("--mask-decoder-program"), "{err}");
    }

    #[test]
    fn the_rules_file_chooses_the_mask_decoder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".adk")).unwrap();
        std::fs::write(dir.path().join(RULES_FILE), "[sam.masks]\ndecoder = \"licensed\"\n")
            .unwrap();
        assert_eq!(rules(dir.path(), None).unwrap().sam.masks, MaskDecoderChoice::Licensed);
    }

    #[test]
    fn each_task_defaults_to_its_on_device_backend() {
        let run = RunArgs {
            image: "page.png".into(),
            backend: None,
            output: RunFormat::Table,
            annotate: None,
            crop: None,
            data_class: None,
            approve: Vec::new(),
            config: None,
            base_url: None,
            manifests: ManifestArgs::default(),
            masks: MaskDecoderArgs::default(),
        };
        let read = RunTask::Read { run: run.clone(), markdown: false };
        assert_eq!(backend_id(&read), "pp-ocrv6-tiny");
        let detect = RunTask::Detect { run: run.clone(), classes: Vec::new(), threshold: None };
        assert_eq!(backend_id(&detect), "rf-detr-n");
        let describe = RunTask::Describe { run, question: "?".into(), model: None, regions: false };
        assert_eq!(backend_id(&describe), "claude");
    }
}
