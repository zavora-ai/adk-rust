//! Decoding through a user-installed `ffmpeg` binary (spec 3.7 D-11, Q9): [`FileSource`],
//! and the H.264 and H.265 decoder `RtspSource` uses.
//!
//! `ffmpeg` runs as a child process through `ffmpeg-sidecar`, its download feature off:
//! nothing is vendored, linked or downloaded.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use ffmpeg_sidecar::child::FfmpegChild;
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::{FfmpegEvent, LogLevel};
use futures::StreamExt;
use tokio::sync::mpsc;

use super::queue::{Pushed, spawn_frames};
use super::{FrameSource, FrameStream, SOURCE_QUEUE_FRAMES, Sampler};
use crate::error::{Result, VisionError};
use crate::media::{DecodedImage, Frame, FramePixels, FrameSourceId};
use crate::traits::Sampling;

/// The environment variable naming the `ffmpeg` binary to run; `ffmpeg` on the `PATH` when
/// unset.
pub const FFMPEG_ENV: &str = "ADK_VISION_FFMPEG";

/// How to install `ffmpeg`, for the error a missing binary raises.
const INSTALL_HINT: &str = "install it with `brew install ffmpeg` (macOS), `sudo apt install ffmpeg` (Debian, Ubuntu) or `winget install ffmpeg` (Windows), or set ADK_VISION_FFMPEG to its path; adk-vision never downloads or bundles ffmpeg";

/// The user-installed `ffmpeg` binary.
///
/// # Example
///
/// ```
/// use adk_vision::stream::FfmpegBinary;
///
/// let missing = FfmpegBinary::at("/nonexistent/ffmpeg");
/// let err = missing.check().unwrap_err();
/// assert!(err.to_string().contains("brew install ffmpeg"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegBinary {
    path: PathBuf,
}

impl Default for FfmpegBinary {
    fn default() -> Self {
        Self::from_env()
    }
}

impl FfmpegBinary {
    /// The binary [`FFMPEG_ENV`] names, or `ffmpeg` on the `PATH`.
    pub fn from_env() -> Self {
        let path = std::env::var_os(FFMPEG_ENV)
            .filter(|p| !p.is_empty())
            .map_or_else(|| PathBuf::from("ffmpeg"), PathBuf::from);
        Self { path }
    }

    /// The binary at `path`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The binary's path, as given.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `ffmpeg -version` and returns its first line.
    ///
    /// # Errors
    ///
    /// [`VisionError::Unavailable`] naming how to install `ffmpeg` when the binary does not
    /// run.
    pub fn check(&self) -> Result<String> {
        let output = std::process::Command::new(&self.path)
            .arg("-version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        match output {
            Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("ffmpeg")
                .to_string()),
            Ok(out) => Err(self.missing(&format!("`-version` exited with {}", out.status))),
            Err(err) => Err(self.missing(&err.to_string())),
        }
    }

    pub(super) fn missing(&self, why: &str) -> VisionError {
        VisionError::Unavailable {
            backend: "ffmpeg".into(),
            message: format!(
                "no working ffmpeg at `{}` ({why}); {INSTALL_HINT}",
                self.path.display()
            ),
        }
    }

    pub(super) fn command(&self) -> FfmpegCommand {
        let mut command = FfmpegCommand::new_with_path(&self.path);
        command.hide_banner();
        command
    }
}

/// The video filter that samples at `fps` and scales down to `max_long_edge`.
pub(super) fn filter(fps: Option<f32>, max_long_edge: Option<u32>) -> Option<String> {
    let mut filters = Vec::new();
    if let Some(fps) = fps {
        filters.push(format!("fps={fps}"));
    }
    if let Some(max) = max_long_edge {
        filters.push(format!(
            "scale=w='min(iw,{max})':h='min(ih,{max})':force_original_aspect_ratio=decrease"
        ));
    }
    (!filters.is_empty()).then(|| filters.join(","))
}

/// One decoded frame from ffmpeg.
pub(super) struct Raw {
    pub(super) pixels: DecodedImage,
    /// ffmpeg's output timestamp.
    pub(super) pts: Duration,
    /// The frame's number in ffmpeg's output, from 0.
    #[cfg_attr(not(feature = "device"), allow(dead_code, reason = "only DeviceSource reads it"))]
    pub(super) number: u32,
}

/// A running ffmpeg whose decoded RGBA frames arrive on a channel; killed and reaped when
/// dropped, so a dropped stream leaves no process behind.
pub(crate) struct Decoder {
    child: FfmpegChild,
    frames: mpsc::Receiver<Result<Raw>>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // The process may already have exited; both calls are then harmless.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Decoder {
    /// Spawns `command` (which must write `-f rawvideo -pix_fmt rgba -`) and forwards its
    /// frames, `buffer` at most in flight, from a reader thread.
    pub(super) fn spawn(
        binary: &FfmpegBinary,
        mut command: FfmpegCommand,
        buffer: usize,
    ) -> Result<Self> {
        command.args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
        let mut child = command.spawn().map_err(|err| binary.missing(&err.to_string()))?;
        let events = child.iter().map_err(|err| VisionError::Backend {
            backend: "ffmpeg".into(),
            message: err.to_string(),
        })?;
        let (tx, frames) = mpsc::channel(buffer.max(1));
        std::thread::Builder::new()
            .name("adk-vision-ffmpeg".into())
            .spawn(move || forward(events, &tx))
            .map_err(|err| VisionError::io("start the ffmpeg reader for", binary.path(), err))?;
        Ok(Self { child, frames })
    }

    pub(super) async fn next(&mut self) -> Option<Result<Raw>> {
        self.frames.recv().await
    }
}

/// Forwards decoded frames until ffmpeg ends or the receiver goes; ends with ffmpeg's first
/// and last errors when it produced none.
fn forward(events: ffmpeg_sidecar::iter::FfmpegIterator, tx: &mpsc::Sender<Result<Raw>>) {
    let mut first_error: Option<String> = None;
    let mut last_error = None;
    let mut produced = false;
    for event in events {
        match event {
            FfmpegEvent::OutputFrame(frame) => {
                produced = true;
                let pts = Duration::from_secs_f64(f64::from(frame.timestamp.max(0.0)));
                let raw = DecodedImage::from_rgba(frame.width, frame.height, frame.data)
                    .map(|pixels| Raw { pixels, pts, number: frame.frame_num });
                if tx.blocking_send(raw).is_err() {
                    return;
                }
            }
            FfmpegEvent::Log(LogLevel::Error | LogLevel::Fatal, message)
            | FfmpegEvent::Error(message) => {
                if first_error.is_none() {
                    first_error = Some(message);
                } else {
                    last_error = Some(message);
                }
            }
            FfmpegEvent::Done | FfmpegEvent::LogEOF => {}
            _ => {}
        }
    }
    if !produced && let Some(first) = first_error {
        // The first error names the cause, such as a camera that refused to open; the last,
        // what ffmpeg gave up on.
        let message = match last_error {
            Some(last) => format!("{first}; {last}"),
            None => first,
        };
        let _ = tx.blocking_send(Err(VisionError::Backend { backend: "ffmpeg".into(), message }));
    }
}

/// A decoder fed an H.264 or H.265 elementary stream, Annex B, on its standard input.
#[cfg(feature = "rtsp")]
pub(crate) struct StreamDecoder {
    pub(crate) decoder: Decoder,
    input: Option<mpsc::Sender<Vec<u8>>>,
}

#[cfg(feature = "rtsp")]
impl StreamDecoder {
    /// Spawns ffmpeg decoding `format` (`h264` or `hevc`), scaling to `max_long_edge`.
    pub(crate) fn spawn(
        binary: &FfmpegBinary,
        format: &str,
        max_long_edge: Option<u32>,
    ) -> Result<Self> {
        let mut command = binary.command();
        // One decoder thread: frame threading holds frames back, and a camera needs each
        // frame as soon as its access unit is in. `-fflags nobuffer` is left out: on a raw
        // elementary stream, whose timestamps ffmpeg invents, it drops every frame.
        command.args([
            "-flags",
            "low_delay",
            "-threads",
            "1",
            "-probesize",
            "65536",
            "-analyzeduration",
            "0",
        ]);
        command.format(format).input("pipe:0").no_audio();
        if let Some(filter) = filter(None, max_long_edge) {
            command.filter(filter);
        }
        // One output frame per access unit: no frame duplicated or dropped to fit a rate
        // ffmpeg guessed, so frames still match their RTP timestamps in order.
        command.fps_mode("passthrough");
        let mut decoder = Decoder::spawn(binary, command, SOURCE_QUEUE_FRAMES)?;
        let stdin = decoder.child.take_stdin().ok_or_else(|| VisionError::Backend {
            backend: "ffmpeg".into(),
            message: "the decoder has no standard input".into(),
        })?;
        let (input, mut units) = mpsc::channel::<Vec<u8>>(64);
        std::thread::Builder::new()
            .name("adk-vision-ffmpeg-in".into())
            .spawn(move || {
                use std::io::Write;
                let mut stdin = stdin;
                while let Some(unit) = units.blocking_recv() {
                    if stdin.write_all(&unit).is_err() {
                        return;
                    }
                }
                // Dropping stdin ends the input, so ffmpeg flushes and exits.
            })
            .map_err(|err| VisionError::io("start the ffmpeg writer for", binary.path(), err))?;
        Ok(Self { decoder, input: Some(input) })
    }

    /// Feeds one access unit, waiting while ffmpeg is behind.
    pub(crate) async fn feed(&self, unit: Vec<u8>) -> Result<()> {
        let input = self.input.as_ref().ok_or_else(|| VisionError::Backend {
            backend: "ffmpeg".into(),
            message: "the decoder's input is closed".into(),
        })?;
        input.send(unit).await.map_err(|_| VisionError::Backend {
            backend: "ffmpeg".into(),
            message: "the decoder exited".into(),
        })
    }

    /// Ends the input; the remaining frames still arrive.
    pub(crate) fn finish(&mut self) {
        self.input = None;
    }

    /// The next decoded frame's pixels.
    pub(crate) async fn next(&mut self) -> Option<Result<DecodedImage>> {
        self.decoder.next().await.map(|raw| raw.map(|raw| raw.pixels))
    }
}

/// An MP4 or MOV file decoded by a user-installed `ffmpeg` (spec 3.7 §7.13, Q9).
///
/// ffmpeg samples at the requested rate and scales down to `max_long_edge` itself, so only
/// the frames kept cross the pipe. A file is analysed as fast as it decodes, every sampled
/// frame delivered: the stream waits for its consumer rather than dropping. Frames are
/// stamped `captured_at` = the moment the file was opened plus their presentation time, so
/// the frame governor meters media time. [`paced`](Self::paced) plays the file at its own
/// rate instead, like a camera, dropping what the consumer cannot keep up with.
///
/// # Example
///
/// ```no_run
/// use adk_vision::stream::{FileSource, FrameSource};
/// use adk_vision::{FrameSourceId, Sampling};
/// use futures::StreamExt;
///
/// # async fn demo() -> adk_vision::Result<()> {
/// let clip = FileSource::new(FrameSourceId("dock-cam".into()), "dock.mp4");
/// let mut frames = clip.frames(Sampling { fps: 1.0, max_long_edge: Some(640) }).await?;
/// while let Some(frame) = frames.next().await {
///     let frame = frame?;
///     println!("{} at {:?}", frame.index, frame.pts);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct FileSource {
    id: FrameSourceId,
    path: PathBuf,
    binary: FfmpegBinary,
    paced: bool,
}

impl FileSource {
    /// The video at `path`, decoded by the [`FfmpegBinary::from_env`] binary.
    pub fn new(id: FrameSourceId, path: impl Into<PathBuf>) -> Self {
        Self { id, path: path.into(), binary: FfmpegBinary::from_env(), paced: false }
    }

    /// Decodes with `binary`.
    pub fn with_ffmpeg(mut self, binary: FfmpegBinary) -> Self {
        self.binary = binary;
        self
    }

    /// Plays the file at its own rate, like a live camera, dropping what the consumer cannot
    /// keep up with.
    pub fn paced(mut self, paced: bool) -> Self {
        self.paced = paced;
        self
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl FrameSource for FileSource {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> Result<FrameStream> {
        let mut sampler = Sampler::new(sampling)?;
        let extension = self
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !matches!(extension.as_str(), "mp4" | "m4v" | "mov") {
            return Err(VisionError::InvalidInput(format!(
                "{} is not an MP4 or MOV file; FileSource reads .mp4, .m4v and .mov",
                self.path.display()
            )));
        }
        if !self.path.is_file() {
            return Err(VisionError::io(
                "open the video",
                &self.path,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        let binary = self.binary.clone();
        let check = binary.clone();
        tokio::task::spawn_blocking(move || check.check()).await.map_err(|err| {
            VisionError::Backend { backend: "ffmpeg".into(), message: err.to_string() }
        })??;

        let mut command = binary.command();
        // The file is the input; stdin carries nothing, so ffmpeg must not read commands from it.
        command.arg("-nostdin");
        if self.paced {
            command.realtime();
        }
        command.input(&self.path).no_audio().args(["-sn", "-dn"]);
        if let Some(filter) = filter(Some(sampling.fps), sampling.max_long_edge) {
            command.filter(filter);
        }
        let buffer = if self.paced { SOURCE_QUEUE_FRAMES } else { 2 };
        let mut decoder = Decoder::spawn(&binary, command, buffer)?;
        let id = self.id.clone();
        let opened_at = Utc::now();
        tracing::debug!(frame.source = %id.0, video.path = %self.path.display(), "video opened");

        let mut index = 0u64;
        let mut next_frame = move |raw: Raw| {
            let frame_index = index;
            index += 1;
            // ffmpeg already sampled; the sampler only guards the rate if its filter did not.
            sampler.admit(raw.pts).then(|| Frame {
                source: id.clone(),
                index: frame_index,
                pts: raw.pts,
                captured_at: opened_at + chrono::TimeDelta::from_std(raw.pts).unwrap_or_default(),
                pixels: FramePixels::Ephemeral(Arc::new(raw.pixels)),
            })
        };
        if self.paced {
            return Ok(spawn_frames(SOURCE_QUEUE_FRAMES, move |tx| async move {
                while let Some(raw) = decoder.next().await {
                    let frame = match raw {
                        Ok(raw) => next_frame(raw),
                        Err(err) => return tx.fail(err),
                    };
                    if let Some(frame) = frame
                        && tx.push(Ok(frame)) == Pushed::Closed
                    {
                        return;
                    }
                }
            }));
        }
        let frames = futures::stream::unfold(
            (decoder, next_frame),
            |(mut decoder, mut next_frame)| async move {
                loop {
                    match decoder.next().await? {
                        Ok(raw) => {
                            if let Some(frame) = next_frame(raw) {
                                return Some((Ok(frame), (decoder, next_frame)));
                            }
                        }
                        Err(err) => return Some((Err(err), (decoder, next_frame))),
                    }
                }
            },
        );
        Ok(frames.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_samples_then_scales() {
        assert_eq!(filter(None, None), None);
        assert_eq!(filter(Some(5.0), None).as_deref(), Some("fps=5"));
        assert_eq!(
            filter(Some(2.5), Some(640)).as_deref(),
            Some(
                "fps=2.5,scale=w='min(iw,640)':h='min(ih,640)':force_original_aspect_ratio=decrease"
            )
        );
    }

    #[test]
    fn a_missing_binary_names_how_to_install_it() {
        let err = FfmpegBinary::at("/nonexistent/adk-vision-ffmpeg").check().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("brew install ffmpeg"), "{text}");
        assert!(text.contains(FFMPEG_ENV), "{text}");
        assert!(text.contains("never downloads"), "{text}");
    }
}
