//! Turning `sam-3.1` mask payloads into rasters.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::parse::SamMask;
use crate::error::{Result, VisionError};

/// Decodes `sam-3.1` mask payloads into row-major rasters of `height × width` bits.
///
/// Meta's payload is base85 text around an arithmetic-coded body whose model is defined
/// only by Meta's own parser, `@meta-sam/parser`, published under the SAM License. This
/// crate does not ship that codec. A [`SamSegmenter`](super::SamSegmenter) without a
/// decoder answers with each object's box in source pixels; with one, with its mask.
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

/// Decodes masks with Meta's `@meta-sam/parser` in a user-installed Node.js, the way the
/// `ffmpeg` feature uses a user-installed `ffmpeg`: this crate neither bundles nor
/// downloads it, and installing it (`npm install @meta-sam/parser`) accepts its licence.
///
/// One `node` process decodes every mask of an answer; it reads them as JSON on standard
/// input and writes each raster as base64 on standard output.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::sam::{NodeMaskDecoder, SamConfig, SamSegmenter};
///
/// let decoder = NodeMaskDecoder::new("/opt/sam/node_modules/@meta-sam/parser");
/// let sam = SamSegmenter::new(SamConfig::new("meta-key"))
///     .unwrap()
///     .with_mask_decoder(Arc::new(decoder));
/// ```
#[derive(Clone, Debug)]
pub struct NodeMaskDecoder {
    node: PathBuf,
    package: PathBuf,
    timeout: Duration,
}

/// Reads `[{encoding, width, height, payload}]` from standard input and writes one base64
/// raster per mask; the package path is the first argument.
const SCRIPT: &str = r#"
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

impl NodeMaskDecoder {
    /// A decoder using `node` from `PATH` and the `@meta-sam/parser` package at `package`
    /// (the directory holding its `package.json`).
    pub fn new(package: impl Into<PathBuf>) -> Self {
        Self {
            node: PathBuf::from("node"),
            package: package.into(),
            timeout: Duration::from_secs(60),
        }
    }

    /// Runs this `node` binary.
    pub fn with_node(mut self, node: impl Into<PathBuf>) -> Self {
        self.node = node.into();
        self
    }

    /// Ends a decode that takes longer than `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl SamMaskDecoder for NodeMaskDecoder {
    async fn decode(&self, masks: &[SamMask]) -> Result<Vec<Vec<bool>>> {
        if masks.is_empty() {
            return Ok(Vec::new());
        }
        let backend = |message: String| VisionError::Backend {
            backend: "sam-3.1 mask decoder".into(),
            message,
        };
        let input: Vec<serde_json::Value> = masks
            .iter()
            .map(|m| {
                serde_json::json!({
                    "encoding": m.encoding,
                    "width": m.width,
                    "height": m.height,
                    "payload": m.payload,
                })
            })
            .collect();
        let input = serde_json::to_vec(&input).map_err(|e| backend(e.to_string()))?;
        let mut child = tokio::process::Command::new(&self.node)
            .arg("--input-type=module")
            .arg("-e")
            .arg(SCRIPT)
            .arg(&self.package)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                backend(format!("could not start `{}`: {e}; install Node.js", self.node.display()))
            })?;
        let mut stdin = child.stdin.take().ok_or_else(|| backend("no stdin".into()))?;
        let mut stdout = child.stdout.take().ok_or_else(|| backend("no stdout".into()))?;
        let mut stderr = child.stderr.take().ok_or_else(|| backend("no stderr".into()))?;
        let work = async {
            stdin.write_all(&input).await?;
            drop(stdin);
            let mut out = Vec::new();
            let mut err = Vec::new();
            let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
            a?;
            b?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((status, out, err))
        };
        let (status, out, err) = tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| backend(format!("decoding took longer than {:?}", self.timeout)))?
            .map_err(|e| backend(e.to_string()))?;
        if !status.success() {
            let err = String::from_utf8_lossy(&err);
            let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
            return Err(backend(format!(
                "node exited with {status}: {first}; is @meta-sam/parser installed at {}?",
                self.package.display()
            )));
        }
        let rasters: Vec<String> =
            serde_json::from_slice(&out).map_err(|e| backend(format!("unreadable output: {e}")))?;
        if rasters.len() != masks.len() {
            return Err(backend(format!("{} rasters for {} masks", rasters.len(), masks.len())));
        }
        rasters
            .iter()
            .zip(masks)
            .map(|(b64, mask)| {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| backend(e.to_string()))?;
                if bytes.len() != (mask.width as usize) * (mask.height as usize) {
                    return Err(backend(format!(
                        "a raster of {} bytes for a {}x{} mask",
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
