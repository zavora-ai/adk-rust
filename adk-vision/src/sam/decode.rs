//! Turning `sam-3.1` mask payloads into rasters: the decoder trait, a user-installed
//! program, and, with feature `sam-codec`, the licensed port of Meta's codec.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::parse::SamMask;
use crate::error::{Result, VisionError};
use crate::manifest::AcceptanceStore;
use crate::mask_decoder::MaskDecoderChoice;

/// Decodes `sam-3.1` mask payloads into row-major rasters of `height × width` bits.
///
/// Meta's payload is base85 text around an arithmetic-coded body whose model is defined
/// only by Meta's own parser, `@meta-sam/parser`, published under the SAM License. The
/// Apache-2.0 crates hold no SAM-licensed code: a [`SamSegmenter`](super::SamSegmenter)
/// without a decoder answers with each object's box in source pixels; with one, with its
/// mask. [`MaskDecoderChoice`] picks [`ExternalMaskDecoder`] or, with feature `sam-codec`,
/// `LicensedMaskDecoder`; any other implementation is installed with
/// [`SamSegmenter::with_mask_decoder`](super::SamSegmenter::with_mask_decoder).
///
/// # Example
///
/// ```
/// use adk_vision::sam::{SamMask, SamMaskDecoder};
///
/// /// Every mask is solid: its box is the object.
/// struct Solid;
///
/// #[async_trait::async_trait]
/// impl SamMaskDecoder for Solid {
///     async fn decode(&self, masks: &[SamMask]) -> adk_vision::Result<Vec<Vec<bool>>> {
///         Ok(masks.iter().map(|m| vec![true; (m.width * m.height) as usize]).collect())
///     }
/// }
/// ```
#[async_trait]
pub trait SamMaskDecoder: Send + Sync {
    /// One row-major raster of `height × width` bits per mask, in order.
    ///
    /// # Errors
    ///
    /// The decoder's failure; the segmenter refuses the answer rather than guess.
    async fn decode(&self, masks: &[SamMask]) -> Result<Vec<Vec<bool>>>;
}

/// The backend name decoder errors carry.
const DECODER: &str = "sam-3.1 mask decoder";
/// The most standard error kept from a failing program.
const MAX_STDERR: u64 = 64 * 1024;

fn decoder_error(message: String) -> VisionError {
    VisionError::Backend { backend: DECODER.into(), message }
}

/// Decodes masks with a program the user installed, the way the video backends use a
/// user-installed `ffmpeg`: this crate neither bundles nor downloads it.
///
/// The program is started once per answer with its arguments. It reads a JSON array of
/// `{"encoding", "width", "height", "payload"}` on standard input (`encoding` is `lossless`
/// or `one_bit`, `payload` the mask text exactly as the API sent it) and writes a JSON array
/// with one base64 string per mask: `height × width` bytes, row-major, nonzero for a set
/// pixel. A missing program, a non-zero exit, output that is not that array, and a decode
/// that outlasts the timeout are each an error naming what went wrong; the program is
/// killed when the timeout ends it.
///
/// [`ExternalMaskDecoder::meta_parser`] runs Meta's `@meta-sam/parser` in a user-installed
/// Node.js; installing that package (`npm install @meta-sam/parser`) accepts its licence.
///
/// # Example
///
/// ```no_run
/// use std::time::Duration;
/// use adk_vision::sam::ExternalMaskDecoder;
/// use adk_vision::{MaskDecoderChoice, SamConfig, SamSegmenter};
///
/// // Through the configuration, as `.adk/vision.toml` would choose it.
/// let sam = SamSegmenter::new(
///     SamConfig::new("meta-key")
///         .with_mask_decoder(MaskDecoderChoice::external("sam-mask-decode", ["--json"])),
/// )?;
///
/// // Or directly, here Meta's parser in Node.js.
/// let decoder = ExternalMaskDecoder::meta_parser("/opt/sam/node_modules/@meta-sam/parser")
///     .with_timeout(Duration::from_secs(20));
/// # Ok::<(), adk_vision::VisionError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalMaskDecoder {
    program: PathBuf,
    args: Vec<String>,
    timeout: Duration,
}

/// Reads `[{encoding, width, height, payload}]` from standard input and writes one base64
/// raster per mask; the package path is the last argument.
const META_PARSER_SCRIPT: &str = r#"
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';
const pkg = process.argv[process.argv.length - 1];
const { decodeMaskToRaster } = await import(pathToFileURL(join(pkg, 'dist', 'index.js')).href);
let input = '';
for await (const chunk of process.stdin) input += chunk;
const masks = JSON.parse(input);
const out = masks.map((m) => Buffer.from(decodeMaskToRaster(m)).toString('base64'));
process.stdout.write(JSON.stringify(out));
"#;

impl ExternalMaskDecoder {
    /// A decoder running `program` (a path, or a name looked up on `PATH`) with no
    /// arguments and a 60-second timeout.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            timeout: Duration::from_secs(crate::mask_decoder::DEFAULT_MASK_DECODER_TIMEOUT_SECS),
        }
    }

    /// Meta's `@meta-sam/parser` at `package` (the directory holding its `package.json`),
    /// run by `node` from `PATH`; [`with_program`](Self::with_program) names another
    /// `node`.
    pub fn meta_parser(package: impl Into<PathBuf>) -> Self {
        let package: PathBuf = package.into();
        Self::new("node").with_args([
            "--input-type=module".to_string(),
            "-e".to_string(),
            META_PARSER_SCRIPT.to_string(),
            package.to_string_lossy().into_owned(),
        ])
    }

    /// Runs this program instead.
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Passes these arguments, replacing any set before.
    pub fn with_args<A: Into<String>>(mut self, args: impl IntoIterator<Item = A>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Ends a decode that takes longer than `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The program this decoder runs.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Checks that the program exists: a path names a file, a bare name is on `PATH`.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] naming the program and how to fix the configuration.
    pub fn check(&self) -> Result<()> {
        if resolve(&self.program).is_some() {
            return Ok(());
        }
        Err(VisionError::InvalidInput(format!(
            "the sam-3.1 mask decoder program `{}` was not found{}; install it, give its full \
             path, or choose `decoder = \"boxes\"` to answer with boxes",
            self.program.display(),
            if self.program.components().count() > 1 { "" } else { " on PATH" },
        )))
    }
}

/// The file `program` names: itself when it has a directory part, else the first match on
/// `PATH` (with Windows' executable extensions).
fn resolve(program: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 || program.is_absolute() {
        return program.is_file().then(|| program.to_path_buf());
    }
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(str::to_string)
            .chain([String::new()])
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        extensions.iter().find_map(|ext| {
            let mut name = program.as_os_str().to_os_string();
            name.push(ext);
            let candidate = dir.join(name);
            candidate.is_file().then_some(candidate)
        })
    })
}

#[async_trait]
impl SamMaskDecoder for ExternalMaskDecoder {
    async fn decode(&self, masks: &[SamMask]) -> Result<Vec<Vec<bool>>> {
        if masks.is_empty() {
            return Ok(Vec::new());
        }
        let program = self.program.display().to_string();
        let input: Vec<serde_json::Value> = masks
            .iter()
            .map(|m| {
                serde_json::json!({
                    "encoding": m.encoding,
                    "width": m.width,
                    "height": m.height,
                    // The wire text: Meta's decoders read the marker as part of the payload.
                    "payload": format!("{}{}", m.encoding.marker(), m.payload),
                })
            })
            .collect();
        let input = serde_json::to_vec(&input).map_err(|e| decoder_error(e.to_string()))?;
        // Base64 of every raster plus the JSON around it; more is not an answer.
        let area: u64 = masks.iter().map(|m| u64::from(m.width) * u64::from(m.height)).sum();
        let max_stdout = area.saturating_mul(4) / 3 + 8 * masks.len() as u64 + 1024;
        let mut child = tokio::process::Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => decoder_error(format!(
                    "the program `{program}` was not found; install it, give its full path, or \
                     choose `decoder = \"boxes\"`"
                )),
                _ => decoder_error(format!("could not start `{program}`: {e}")),
            })?;
        let mut stdin = child.stdin.take().ok_or_else(|| decoder_error("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| decoder_error("no stdout".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| decoder_error("no stderr".into()))?;
        // Standard error drains on its own, so a program stopped for writing too much still
        // closes it.
        let errors = tokio::spawn(async move {
            let mut err = Vec::new();
            let _ = stderr.take(MAX_STDERR).read_to_end(&mut err).await;
            err
        });
        let work = async {
            // A program that exits without reading its input closes the pipe; its exit
            // status and output say why.
            let written = async {
                let _ = stdin.write_all(&input).await;
                drop(stdin);
            };
            let mut out = Vec::new();
            let mut stdout = stdout.take(max_stdout + 1);
            let (_, read) = tokio::join!(written, stdout.read_to_end(&mut out));
            read?;
            if out.len() as u64 > max_stdout {
                child.start_kill()?;
                child.wait().await?;
                return Ok((None, out));
            }
            Ok::<_, std::io::Error>((Some(child.wait().await?), out))
        };
        let (status, out) = tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| {
                decoder_error(format!(
                    "`{program}` took longer than {:?} and was stopped; raise `timeout_secs` or \
                     check the program",
                    self.timeout
                ))
            })?
            .map_err(|e| decoder_error(format!("`{program}`: {e}")))?;
        let Some(status) = status else {
            return Err(decoder_error(format!(
                "`{program}` wrote more than {max_stdout} bytes for {} masks and was stopped; it \
                 must write one JSON array of base64 rasters",
                masks.len()
            )));
        };
        if !status.success() {
            // A child the program left behind may hold standard error open; wait briefly.
            let err = tokio::time::timeout(Duration::from_secs(1), errors)
                .await
                .ok()
                .and_then(std::result::Result::ok)
                .unwrap_or_default();
            let err = String::from_utf8_lossy(&err);
            let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
            return Err(decoder_error(format!("`{program}` exited with {status}: {first}")));
        }
        errors.abort();
        let rasters: Vec<String> = serde_json::from_slice(&out).map_err(|e| {
            decoder_error(format!(
                "`{program}` answered unreadable output ({e}); it must write one JSON array of \
                 base64 rasters"
            ))
        })?;
        if rasters.len() != masks.len() {
            return Err(decoder_error(format!(
                "`{program}` answered {} rasters for {} masks",
                rasters.len(),
                masks.len()
            )));
        }
        rasters
            .iter()
            .zip(masks)
            .enumerate()
            .map(|(i, (b64, mask))| {
                let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| {
                    decoder_error(format!("`{program}` raster {i} is not base64: {e}"))
                })?;
                if bytes.len() as u64 != u64::from(mask.width) * u64::from(mask.height) {
                    return Err(decoder_error(format!(
                        "`{program}` raster {i} has {} bytes for a {}x{} mask",
                        bytes.len(),
                        mask.width,
                        mask.height
                    )));
                }
                Ok(bytes.into_iter().map(|b| b != 0).collect())
            })
            .collect()
    }
}

/// Decodes masks in this process with `adk-vision-sam-codec`, the Rust port of the mask
/// codec in Meta's `@meta-sam/parser`, distributed under the SAM License.
///
/// Constructing one checks the licence gate: a person must have accepted the SAM License
/// for the [`SAM_MASK_CODEC_MANIFEST`](crate::SAM_MASK_CODEC_MANIFEST) entry
/// (`adk-rust vision models accept sam-mask-codec`). Each raster is bounded before it is
/// allocated, by default to 50 million pixels.
///
/// # Example
///
/// ```
/// use adk_vision::sam::LicensedMaskDecoder;
/// use adk_vision::{
///     AcceptanceStore, InMemoryAcceptanceStore, LicenceAcceptance, ManifestSet,
///     SAM_MASK_CODEC_MANIFEST,
/// };
///
/// let store = InMemoryAcceptanceStore::new();
/// let refused = LicensedMaskDecoder::new(&store).unwrap_err().to_string();
/// assert!(refused.contains("licence acceptance required"));
/// assert!(refused.contains("github.com/meta-models/meta-sam"));
///
/// let manifests = ManifestSet::builtin().unwrap();
/// let codec = manifests.get(SAM_MASK_CODEC_MANIFEST).unwrap();
/// store.record(LicenceAcceptance::of(codec, "alice@example.com")).unwrap();
/// assert!(LicensedMaskDecoder::new(&store).is_ok());
/// ```
#[cfg(feature = "sam-codec")]
#[derive(Clone, Debug)]
pub struct LicensedMaskDecoder {
    max_area: usize,
}

#[cfg(feature = "sam-codec")]
impl LicensedMaskDecoder {
    /// The decoder, once the licence gate allows the codec.
    ///
    /// # Errors
    ///
    /// [`VisionError::LicenceAcceptanceRequired`] naming the licence URL until a person has
    /// accepted it, or the acceptance store's error.
    pub fn new(acceptances: &dyn AcceptanceStore) -> Result<Self> {
        let manifests = crate::manifest::ManifestSet::builtin()?;
        let manifest =
            manifests.get(crate::mask_decoder::SAM_MASK_CODEC_MANIFEST).ok_or_else(|| {
                VisionError::BackendNotFound("the built-in manifest has no sam-mask-codec".into())
            })?;
        let policy = crate::manifest::LicencePolicy::new();
        crate::manifest::LicenceGate::new(&policy, acceptances).check(manifest)?;
        Ok(Self { max_area: 50_000_000 })
    }

    /// Refuses any raster larger than `pixels` before allocating it.
    pub fn with_max_area(mut self, pixels: usize) -> Self {
        self.max_area = pixels;
        self
    }
}

#[cfg(feature = "sam-codec")]
#[async_trait]
impl SamMaskDecoder for LicensedMaskDecoder {
    async fn decode(&self, masks: &[SamMask]) -> Result<Vec<Vec<bool>>> {
        use adk_vision_sam_codec::{EncodedMask, Encoding, decode_mask_to_raster};

        use super::parse::SamMaskEncoding;

        let masks = masks.to_vec();
        let max_area = self.max_area;
        // A large raster is milliseconds of arithmetic decoding; keep it off the runtime.
        tokio::task::spawn_blocking(move || {
            masks
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let encoding = match m.encoding {
                        SamMaskEncoding::Lossless => Encoding::Lossless,
                        SamMaskEncoding::OneBit => Encoding::OneBit,
                    };
                    let text = format!("{}{}", m.encoding.marker(), m.payload);
                    let mask =
                        EncodedMask { encoding, width: m.width, height: m.height, payload: &text };
                    decode_mask_to_raster(&mask, max_area)
                        .map(|raster| raster.into_iter().map(|b| b == 1).collect())
                        .map_err(|e| VisionError::Parse {
                            backend: DECODER.into(),
                            message: format!("mask {i}: {e}"),
                        })
                })
                .collect()
        })
        .await
        .map_err(|e| decoder_error(e.to_string()))?
    }
}

/// The decoder a configuration chooses, checked now rather than at the first answer.
pub(crate) fn decoder_for(
    choice: &MaskDecoderChoice,
    acceptances: Option<&dyn AcceptanceStore>,
) -> Result<Option<Arc<dyn SamMaskDecoder>>> {
    match choice {
        MaskDecoderChoice::None => Ok(None),
        MaskDecoderChoice::External { program, args, timeout_secs } => {
            let decoder = ExternalMaskDecoder::new(program.clone())
                .with_args(args.iter().cloned())
                .with_timeout(Duration::from_secs(*timeout_secs));
            decoder.check()?;
            Ok(Some(Arc::new(decoder)))
        }
        #[cfg(feature = "sam-codec")]
        MaskDecoderChoice::Licensed => {
            let empty = crate::manifest::InMemoryAcceptanceStore::new();
            let decoder = LicensedMaskDecoder::new(acceptances.unwrap_or(&empty))?;
            Ok(Some(Arc::new(decoder)))
        }
        #[cfg(not(feature = "sam-codec"))]
        MaskDecoderChoice::Licensed => {
            let _ = acceptances;
            Err(VisionError::Unsupported(
                "`decoder = \"licensed\"` needs adk-vision's `sam-codec` feature (adk-rust: \
                 `vision-sam-codec`), which this build lacks; rebuild with it, or choose \
                 `external` or `boxes`"
                    .into(),
            ))
        }
    }
}
