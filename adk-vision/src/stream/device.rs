//! [`DeviceSource`]: a camera on this machine, captured through a user-installed `ffmpeg`
//! (spec 3.7 §7.13, Q9).
//!
//! | Platform | ffmpeg input | A device is | Listed by |
//! |----------|--------------|-------------|-----------|
//! | macOS | `-f avfoundation` | an index or a name | `-list_devices true` |
//! | Linux | `-f v4l2` | `/dev/videoN`, its index `N`, or its name | `-sources v4l2` |
//! | Windows | `-f dshow` | a name, or an index among the video devices | `-list_devices true` |
//!
//! The v4l2 input has no `-list_devices` option; ffmpeg's `-sources v4l2` lists the capture
//! nodes instead.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;

use super::ffmpeg::{Decoder, filter};
use super::queue::{Pushed, spawn_frames};
use super::{FfmpegBinary, FrameSource, FrameStream, SOURCE_QUEUE_FRAMES, Sampler};
use crate::error::{Result, VisionError};
use crate::media::{Frame, FramePixels, FrameSourceId};
use crate::traits::Sampling;

/// The capture rate a [`DeviceSource`] asks the camera for unless told otherwise. AVFoundation's
/// own default, 29.97, is a rate few webcams offer exactly, and it refuses the camera then.
pub const DEFAULT_CAPTURE_RATE: f32 = 30.0;

/// The size the test pattern is drawn at unless a size is asked for.
const TEST_PATTERN_SIZE: (u32, u32) = (640, 480);

/// The platform capture interface ffmpeg opens a camera through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceApi {
    /// macOS: `-f avfoundation`.
    AvFoundation,
    /// Linux: `-f v4l2`.
    V4l2,
    /// Windows: `-f dshow`.
    DirectShow,
}

impl DeviceApi {
    /// The interface of the platform this was built for; `None` on a platform ffmpeg has no
    /// camera input for here.
    pub fn native() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::AvFoundation)
        } else if cfg!(target_os = "linux") {
            Some(Self::V4l2)
        } else if cfg!(target_os = "windows") {
            Some(Self::DirectShow)
        } else {
            None
        }
    }

    /// The ffmpeg input format: `avfoundation`, `v4l2` or `dshow`.
    pub fn ffmpeg_format(self) -> &'static str {
        match self {
            Self::AvFoundation => "avfoundation",
            Self::V4l2 => "v4l2",
            Self::DirectShow => "dshow",
        }
    }

    /// The ffmpeg arguments that list this interface's devices.
    fn list_args(self) -> &'static [&'static str] {
        match self {
            Self::AvFoundation => {
                &["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""]
            }
            Self::V4l2 => &["-hide_banner", "-sources", "v4l2"],
            Self::DirectShow => {
                &["-hide_banner", "-f", "dshow", "-list_devices", "true", "-i", "dummy"]
            }
        }
    }

    /// The video devices in what ffmpeg printed when asked to list this interface's devices
    /// (standard output and standard error together); audio devices are left out.
    ///
    /// Reads the AVFoundation listing with and without the `[uid:…]` suffix newer ffmpeg
    /// releases print, the `-sources v4l2` listing with and without media types, and the
    /// DirectShow listing in both the media-type form of ffmpeg 5 and later and the headed
    /// form before it.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::stream::{DeviceApi, DeviceKind, DeviceSelector};
    ///
    /// let printed = "\
    /// [AVFoundation indev @ 0x7f1404140] AVFoundation video devices:
    /// [AVFoundation indev @ 0x7f1404140] [0] FaceTime HD Camera
    /// [AVFoundation indev @ 0x7f1404140] [1] Capture screen 0
    /// [AVFoundation indev @ 0x7f1404140] AVFoundation audio devices:
    /// [AVFoundation indev @ 0x7f1404140] [0] MacBook Pro Microphone
    /// ";
    /// let devices = DeviceApi::AvFoundation.parse_device_list(printed);
    /// assert_eq!(devices.len(), 2);
    /// assert_eq!(devices[0].name, "FaceTime HD Camera");
    /// assert_eq!(devices[0].selector, DeviceSelector::Index(0));
    /// assert_eq!(devices[1].kind, DeviceKind::Screen);
    /// ```
    pub fn parse_device_list(self, output: &str) -> Vec<CaptureDevice> {
        match self {
            Self::AvFoundation => parse_avfoundation(output),
            Self::V4l2 => parse_v4l2(output),
            Self::DirectShow => parse_dshow(output),
        }
    }
}

impl fmt::Display for DeviceApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.ffmpeg_format())
    }
}

/// What a listed device captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    /// A camera, including a virtual one such as OBS Virtual Camera.
    Camera,
    /// A screen, which AVFoundation lists as `Capture screen N`; capturing it needs the
    /// screen recording permission instead of the camera one.
    Screen,
}

impl DeviceKind {
    /// `camera` or `screen`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::Screen => "screen",
        }
    }
}

/// Which device a [`DeviceSource`] opens.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DeviceSelector {
    /// The index `adk-rust vision sources list` shows: AVFoundation's on macOS, `N` of
    /// `/dev/videoN` on Linux, the position among the video devices on Windows.
    Index(u32),
    /// The name the platform gives the device, or DirectShow's alternative name
    /// (`@device_pnp_…`).
    Name(String),
    /// A device node, such as `/dev/video0` (Linux).
    Path(PathBuf),
}

impl DeviceSelector {
    /// Reads what a person typed: digits are an index, `/dev/…` a path, anything else a
    /// name.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::stream::DeviceSelector;
    ///
    /// assert_eq!(DeviceSelector::parse("0"), DeviceSelector::Index(0));
    /// assert_eq!(DeviceSelector::parse("/dev/video2"), DeviceSelector::Path("/dev/video2".into()));
    /// assert_eq!(DeviceSelector::parse("Huddly GO"), DeviceSelector::Name("Huddly GO".into()));
    /// ```
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        if !text.is_empty()
            && text.bytes().all(|b| b.is_ascii_digit())
            && let Ok(index) = text.parse()
        {
            return Self::Index(index);
        }
        if text.starts_with("/dev/") {
            return Self::Path(PathBuf::from(text));
        }
        Self::Name(text.to_string())
    }
}

impl fmt::Display for DeviceSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(index) => write!(f, "{index}"),
            Self::Name(name) => f.write_str(name),
            Self::Path(path) => write!(f, "{}", path.display()),
        }
    }
}

/// A video device ffmpeg listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureDevice {
    /// The index a [`DeviceSelector::Index`] names it by.
    pub index: u32,
    /// The name the platform gives it: AVFoundation's and DirectShow's device name, the v4l2
    /// card name.
    pub name: String,
    /// A camera or a screen.
    pub kind: DeviceKind,
    /// What opens it: the index on macOS, the device node on Linux, the name on Windows.
    pub selector: DeviceSelector,
}

/// What a [`DeviceSource`] captures.
#[derive(Clone, Debug)]
enum Input {
    /// A device through the platform's interface; `None` where ffmpeg has none here.
    Device { api: Option<DeviceApi>, selector: DeviceSelector },
    /// ffmpeg's `testsrc2` pattern, paced like a camera.
    TestPattern,
}

/// A camera on this machine, captured through a user-installed `ffmpeg` (spec 3.7 §7.13,
/// Q9): AVFoundation on macOS, v4l2 on Linux, DirectShow on Windows.
///
/// The camera is opened at the capture rate and size asked for ([`with_frame_rate`], 30 a
/// second by default, and [`with_video_size`]), which must be a mode the camera offers.
/// ffmpeg samples at the stream's rate when that is lower and scales down to
/// `max_long_edge`, so only the frames kept cross the pipe; [`Sampler`] guards the rate on
/// ffmpeg's timestamps. Presentation times count from the start of capture; `captured_at`
/// is the moment each frame arrived. The source holds four frames for a consumer that falls
/// behind and drops the oldest, so the consumer always sees the newest. Dropping the stream
/// kills the ffmpeg child.
///
/// Nothing is vendored or downloaded: the binary is the one [`FfmpegBinary`] finds, and a
/// missing one fails with the commands that install it. [`DeviceSource::list`] names the
/// devices; [`DeviceSource::test_pattern`] stands in for a camera in tests and demos.
///
/// On macOS the first capture asks for camera permission for the application that runs the
/// process, usually the terminal; until it is allowed in System Settings → Privacy &
/// Security → Camera, the camera refuses to open and the error says so. Listing needs no
/// permission.
///
/// [`with_frame_rate`]: Self::with_frame_rate
/// [`with_video_size`]: Self::with_video_size
///
/// # Example
///
/// ```no_run
/// use adk_vision::stream::{DeviceSelector, DeviceSource, FrameSource};
/// use adk_vision::{FrameSourceId, Sampling};
/// use futures::StreamExt;
///
/// # async fn demo() -> adk_vision::Result<()> {
/// for device in DeviceSource::list()? {
///     println!("{} {} ({})", device.index, device.name, device.kind.as_str());
/// }
/// let camera = DeviceSource::new(FrameSourceId("desk".into()), DeviceSelector::Index(0))
///     .with_video_size(1280, 720);
/// let mut frames = camera.frames(Sampling { fps: 2.0, max_long_edge: Some(640) }).await?;
/// if let Some(frame) = frames.next().await {
///     let frame = frame?;
///     println!("frame {} at {:?}", frame.index, frame.pts);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct DeviceSource {
    id: FrameSourceId,
    input: Input,
    binary: FfmpegBinary,
    frame_rate: f32,
    video_size: Option<(u32, u32)>,
}

impl DeviceSource {
    /// The device `selector` names, through this platform's interface and the
    /// [`FfmpegBinary::from_env`] binary.
    pub fn new(id: FrameSourceId, selector: DeviceSelector) -> Self {
        Self::with_input(id, Input::Device { api: DeviceApi::native(), selector })
    }

    /// ffmpeg's `testsrc2` pattern at the requested size and rate, paced like a camera: a
    /// stand-in for a camera in tests and demos, on any platform.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_vision::stream::{DeviceSource, FrameSource};
    /// use adk_vision::{FrameSourceId, Sampling};
    /// use futures::StreamExt;
    ///
    /// # async fn demo() -> adk_vision::Result<()> {
    /// let pattern = DeviceSource::test_pattern(FrameSourceId("pattern".into())).with_video_size(320, 240);
    /// let frames: Vec<_> = pattern
    ///     .frames(Sampling { fps: 5.0, max_long_edge: None })
    ///     .await?
    ///     .take(5)
    ///     .collect()
    ///     .await;
    /// assert_eq!(frames.len(), 5);
    /// # Ok(())
    /// # }
    /// ```
    pub fn test_pattern(id: FrameSourceId) -> Self {
        Self::with_input(id, Input::TestPattern)
    }

    fn with_input(id: FrameSourceId, input: Input) -> Self {
        Self {
            id,
            input,
            binary: FfmpegBinary::from_env(),
            frame_rate: DEFAULT_CAPTURE_RATE,
            video_size: None,
        }
    }

    /// Captures with `binary`.
    pub fn with_ffmpeg(mut self, binary: FfmpegBinary) -> Self {
        self.binary = binary;
        self
    }

    /// Asks the camera for `fps` frames a second (default [`DEFAULT_CAPTURE_RATE`]); the rate
    /// must be one the camera offers.
    pub fn with_frame_rate(mut self, fps: f32) -> Self {
        self.frame_rate = fps;
        self
    }

    /// Asks the camera for `width` × `height` frames, a size it offers (default: the
    /// camera's choice; 640 × 480 for the test pattern).
    pub fn with_video_size(mut self, width: u32, height: u32) -> Self {
        self.video_size = Some((width, height));
        self
    }

    /// The device this source opens; `None` for the test pattern.
    pub fn selector(&self) -> Option<&DeviceSelector> {
        match &self.input {
            Input::Device { selector, .. } => Some(selector),
            Input::TestPattern => None,
        }
    }

    /// The video devices on this machine, as the [`FfmpegBinary::from_env`] binary lists
    /// them. Needs no camera permission. Blocks while ffmpeg runs; from async code, call it
    /// through `tokio::task::spawn_blocking`.
    ///
    /// # Errors
    ///
    /// [`VisionError::Unavailable`] when ffmpeg does not run (naming how to install it), was
    /// built without this platform's capture input, or cannot list its devices;
    /// [`VisionError::Unsupported`] on a platform with no capture input here.
    pub fn list() -> Result<Vec<CaptureDevice>> {
        let api = DeviceApi::native().ok_or_else(no_native_api)?;
        Self::list_with(&FfmpegBinary::from_env(), api)
    }

    /// The video devices `binary` lists through `api`.
    ///
    /// # Errors
    ///
    /// As [`list`](Self::list).
    pub fn list_with(binary: &FfmpegBinary, api: DeviceApi) -> Result<Vec<CaptureDevice>> {
        let output = std::process::Command::new(binary.path())
            .args(api.list_args())
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|err| binary.missing(&err.to_string()))?;
        // `-list_devices` exits with an error by design once it has printed; the listing is
        // read whatever the exit status.
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        let format = api.ffmpeg_format();
        if text.contains(&format!("Unknown input format: '{format}'"))
            || text.contains(&format!("Unknown input format: {format}"))
        {
            return Err(VisionError::Unavailable {
                backend: "ffmpeg".into(),
                message: format!(
                    "the ffmpeg at `{}` was built without the `{format}` input; install a build with it (Homebrew's, Debian's and gyan.dev's builds have it)",
                    binary.path().display()
                ),
            });
        }
        if let Some(line) = text.lines().find(|line| line.starts_with("Cannot list sources")) {
            return Err(VisionError::Unavailable {
                backend: "ffmpeg".into(),
                message: format!("ffmpeg could not list the {format} devices: {line}"),
            });
        }
        Ok(api.parse_device_list(&text))
    }

    fn check_mode(&self) -> Result<()> {
        if !self.frame_rate.is_finite() || self.frame_rate <= 0.0 {
            return Err(VisionError::InvalidInput(format!(
                "the capture rate of `{}` must be a positive number of frames a second; got {}",
                self.id.0, self.frame_rate
            )));
        }
        if let Some((width, height)) = self.video_size
            && (width == 0 || height == 0)
        {
            return Err(VisionError::InvalidInput(format!(
                "the capture size of `{}` must be at least 1 × 1 pixels; got {width} × {height}",
                self.id.0
            )));
        }
        Ok(())
    }

    /// ffmpeg's arguments up to the output: the input, then the sampling and scaling filter.
    fn capture_args(&self, input: &CaptureInput, sampling: Sampling) -> Vec<String> {
        let mut args = vec!["-nostdin".to_string()];
        match input {
            CaptureInput::Device { api, input } => {
                args.extend(["-f".into(), api.ffmpeg_format().into()]);
                args.extend(["-framerate".into(), self.frame_rate.to_string()]);
                if let Some((width, height)) = self.video_size {
                    args.extend(["-video_size".into(), format!("{width}x{height}")]);
                }
                args.extend(["-i".into(), input.clone()]);
            }
            CaptureInput::TestPattern => {
                let (width, height) = self.video_size.unwrap_or(TEST_PATTERN_SIZE);
                // `-re` reads the pattern at its own rate, as a camera delivers.
                args.extend(["-re".into(), "-f".into(), "lavfi".into()]);
                args.extend([
                    "-i".into(),
                    format!("testsrc2=size={width}x{height}:rate={}", self.frame_rate),
                ]);
            }
        }
        args.extend(["-an", "-sn", "-dn"].map(String::from));
        // Sampling above the capture rate would only repeat frames, so ffmpeg samples only
        // below it.
        let fps = (sampling.fps < self.frame_rate).then_some(sampling.fps);
        if let Some(filter) = filter(fps, sampling.max_long_edge) {
            args.extend(["-vf".into(), filter]);
        }
        args
    }

    /// What to open, with the devices listed when the selector needs them.
    fn resolve(&self) -> Result<CaptureInput> {
        let (api, selector) = match &self.input {
            Input::TestPattern => return Ok(CaptureInput::TestPattern),
            Input::Device { api: None, .. } => return Err(no_native_api()),
            Input::Device { api: Some(api), selector } => (*api, selector),
        };
        let listed = if needs_list(api, selector) {
            Self::list_with(&self.binary, api)?
        } else {
            Vec::new()
        };
        let input = device_input(api, selector, &listed)?;
        Ok(CaptureInput::Device { api, input })
    }

    /// The error for a camera ffmpeg could not open, with what to try on this platform.
    fn open_error(&self, input: &CaptureInput, err: VisionError) -> VisionError {
        let message = match err {
            // ffmpeg's log lines, joined by `forward`, without their prefixes.
            VisionError::Backend { message, .. } => {
                message.split("; ").map(log_text).collect::<Vec<_>>().join("; ")
            }
            other => other.to_string(),
        };
        let not_found = message.contains("Invalid device index")
            || message.contains("not found")
            || message.contains("No such file or directory");
        let (what, hint) = match input {
            CaptureInput::TestPattern => ("the test pattern".to_string(), ""),
            CaptureInput::Device { api, input } => (
                format!("{api} device `{input}`"),
                match api {
                    DeviceApi::AvFoundation if not_found => {
                        "; `adk-rust vision sources list` shows the devices ffmpeg can open"
                    }
                    DeviceApi::AvFoundation => {
                        "; on macOS the first capture asks for camera permission for the app that runs this process (usually the terminal): allow it in System Settings → Privacy & Security → Camera and run again. Also check the device with `adk-rust vision sources list` and that the capture rate and size are a mode it offers"
                    }
                    DeviceApi::V4l2 => {
                        "; check the node with `adk-rust vision sources list`, that your user may read it (the `video` group), that no other program holds the camera, and that the capture rate and size are a mode it offers (`ffmpeg -f v4l2 -list_formats all -i /dev/videoN`)"
                    }
                    DeviceApi::DirectShow => {
                        "; check the name with `adk-rust vision sources list`, that no other program holds the camera, and that the capture rate and size are a mode it offers (`ffmpeg -f dshow -list_options true -i video=NAME`)"
                    }
                },
            ),
        };
        VisionError::Unavailable {
            backend: "ffmpeg".into(),
            message: format!("`{}` could not open {what}: {message}{hint}", self.id.0),
        }
    }
}

/// What ffmpeg opens.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CaptureInput {
    /// `-f <api> -i <input>`.
    Device { api: DeviceApi, input: String },
    /// `-f lavfi -i testsrc2=…`.
    TestPattern,
}

fn no_native_api() -> VisionError {
    VisionError::Unsupported(
        "ffmpeg captures cameras through AVFoundation (macOS), v4l2 (Linux) or DirectShow (Windows); this platform has none of them".into(),
    )
}

/// Whether opening `selector` through `api` needs the device list.
fn needs_list(api: DeviceApi, selector: &DeviceSelector) -> bool {
    match (api, selector) {
        // AVFoundation splits its input on `:`, so such a name is opened by its index.
        (DeviceApi::AvFoundation, DeviceSelector::Name(name)) => name.contains(':'),
        (DeviceApi::V4l2, DeviceSelector::Name(_))
        | (DeviceApi::DirectShow, DeviceSelector::Index(_)) => true,
        (DeviceApi::AvFoundation, DeviceSelector::Index(_) | DeviceSelector::Path(_))
        | (DeviceApi::V4l2, DeviceSelector::Index(_) | DeviceSelector::Path(_))
        | (DeviceApi::DirectShow, DeviceSelector::Name(_) | DeviceSelector::Path(_)) => false,
    }
}

/// The `-i` argument opening `selector` through `api`; `listed` holds the devices when
/// [`needs_list`] says so.
fn device_input(
    api: DeviceApi,
    selector: &DeviceSelector,
    listed: &[CaptureDevice],
) -> Result<String> {
    let not_listed = || {
        let names: Vec<String> =
            listed.iter().map(|d| format!("{} `{}`", d.index, d.name)).collect();
        VisionError::InvalidInput(format!(
            "no {api} video device `{selector}`; ffmpeg lists {}",
            if names.is_empty() { "none".to_string() } else { names.join(", ") }
        ))
    };
    let by_name = |name: &str| {
        listed
            .iter()
            .find(|d| d.name == name)
            .or_else(|| listed.iter().find(|d| d.name.eq_ignore_ascii_case(name)))
    };
    match (api, selector) {
        // `none` opens no audio device.
        (DeviceApi::AvFoundation, DeviceSelector::Index(index)) => Ok(format!("{index}:none")),
        (DeviceApi::AvFoundation, DeviceSelector::Name(name)) if name.contains(':') => {
            by_name(name).map(|d| format!("{}:none", d.index)).ok_or_else(not_listed)
        }
        (DeviceApi::AvFoundation, DeviceSelector::Name(name)) => Ok(format!("{name}:none")),
        (DeviceApi::V4l2, DeviceSelector::Index(index)) => Ok(format!("/dev/video{index}")),
        (DeviceApi::V4l2, DeviceSelector::Path(path)) => Ok(path.display().to_string()),
        (DeviceApi::V4l2, DeviceSelector::Name(name)) => {
            by_name(name).map(|d| d.selector.to_string()).ok_or_else(not_listed)
        }
        (DeviceApi::DirectShow, DeviceSelector::Index(index)) => listed
            .iter()
            .find(|d| d.index == *index)
            .map(|d| format!("video={}", d.name))
            .ok_or_else(not_listed),
        // DirectShow splits its input on `:`; alternative names have none.
        (DeviceApi::DirectShow, DeviceSelector::Name(name)) if name.contains(':') => {
            Err(VisionError::InvalidInput(format!(
                "DirectShow cannot open `{name}` by name, which holds a `:`; use its index or its alternative name (`ffmpeg -list_devices true -f dshow -i dummy` prints it)"
            )))
        }
        (DeviceApi::DirectShow, DeviceSelector::Name(name)) => Ok(format!("video={name}")),
        (DeviceApi::AvFoundation | DeviceApi::DirectShow, DeviceSelector::Path(path)) => {
            Err(VisionError::InvalidInput(format!(
                "{api} opens a device by index or name, not by the path `{}`; `adk-rust vision sources list` shows them",
                path.display()
            )))
        }
    }
}

/// A log line without ffmpeg's `[context @ 0x…] ` prefix and its `[info] ` (or other) level
/// tag.
fn log_text(line: &str) -> &str {
    let mut text = line.trim_end_matches('\r');
    if text.starts_with('[')
        && let Some(end) = text.find("] ")
        && text[..end].contains(" @ ")
    {
        text = &text[end + 2..];
    }
    ["[info] ", "[warning] ", "[error] ", "[fatal] "]
        .iter()
        .find_map(|tag| text.strip_prefix(tag))
        .unwrap_or(text)
}

fn parse_avfoundation(output: &str) -> Vec<CaptureDevice> {
    let mut video = false;
    let mut devices = Vec::new();
    for line in output.lines() {
        let text = log_text(line).trim();
        if text.starts_with("AVFoundation video devices") {
            video = true;
            continue;
        }
        if text.starts_with("AVFoundation audio devices") {
            video = false;
            continue;
        }
        if !video {
            continue;
        }
        let Some((index, name)) = text.strip_prefix('[').and_then(|rest| rest.split_once("] "))
        else {
            continue;
        };
        let Ok(index) = index.parse::<u32>() else { continue };
        // Newer releases append `  [uid:…]` and `[serial:…]`.
        let name = name.split("  [uid:").next().unwrap_or(name).trim();
        if name.is_empty() {
            continue;
        }
        let kind = match name.strip_prefix("Capture screen ") {
            Some(n) if n.bytes().all(|b| b.is_ascii_digit()) => DeviceKind::Screen,
            _ => DeviceKind::Camera,
        };
        devices.push(CaptureDevice {
            index,
            name: name.to_string(),
            kind,
            selector: DeviceSelector::Index(index),
        });
    }
    devices
}

fn parse_v4l2(output: &str) -> Vec<CaptureDevice> {
    let mut devices: Vec<CaptureDevice> = output
        .lines()
        .filter_map(|line| {
            // `* ` marks ffmpeg's default device.
            let body = line.trim_end().trim_start_matches(['*', ' ']);
            let (node, rest) = body.split_once(" [")?;
            let index = node.strip_prefix("/dev/video")?.parse::<u32>().ok()?;
            // ffmpeg 5 and later append the media types, `(video)` or `(none)`.
            let card = match rest.rfind("] (") {
                Some(end) if rest.ends_with(')') => &rest[..end],
                _ => rest.strip_suffix(']')?,
            };
            Some(CaptureDevice {
                index,
                name: card.trim().to_string(),
                kind: DeviceKind::Camera,
                selector: DeviceSelector::Path(PathBuf::from(node)),
            })
        })
        .collect();
    // ffmpeg lists nodes in directory order.
    devices.sort_by_key(|d| d.index);
    devices
}

fn parse_dshow(output: &str) -> Vec<CaptureDevice> {
    // Before ffmpeg 5, headings split video from audio devices; since, each device line
    // carries its media types.
    let mut video_section = None;
    let mut devices = Vec::new();
    for line in output.lines() {
        let text = log_text(line).trim();
        if text.starts_with("DirectShow video devices") {
            video_section = Some(true);
            continue;
        }
        if text.starts_with("DirectShow audio devices") {
            video_section = Some(false);
            continue;
        }
        let Some(rest) = text.strip_prefix('"') else { continue };
        let Some(close) = rest.rfind('"') else { continue };
        let name = &rest[..close];
        let types = rest[close + 1..].trim();
        let video = match types.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            Some(types) => types.split(',').any(|t| t.trim() == "video"),
            None => video_section == Some(true),
        };
        if !video || name.is_empty() {
            continue;
        }
        let index = devices.len() as u32;
        devices.push(CaptureDevice {
            index,
            name: name.to_string(),
            kind: DeviceKind::Camera,
            selector: DeviceSelector::Name(name.to_string()),
        });
    }
    devices
}

#[async_trait]
impl FrameSource for DeviceSource {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> Result<FrameStream> {
        let mut sampler = Sampler::new(sampling)?;
        self.check_mode()?;
        let this = self.clone();
        let input = tokio::task::spawn_blocking(move || {
            this.binary.check()?;
            this.resolve()
        })
        .await
        .map_err(|err| VisionError::Backend {
            backend: "ffmpeg".into(),
            message: err.to_string(),
        })??;

        let mut command = self.binary.command();
        command.args(self.capture_args(&input, sampling));
        // ffmpeg's output is at a constant rate: the sampling rate below the capture rate,
        // the capture rate otherwise.
        let output_fps = f64::from(sampling.fps.min(self.frame_rate));
        let mut decoder = Decoder::spawn(&self.binary, command, SOURCE_QUEUE_FRAMES)?;
        let id = self.id.clone();
        let source = self.clone();
        tracing::info!(frame.source = %id.0, device.input = ?input, "camera opening");

        Ok(spawn_frames(SOURCE_QUEUE_FRAMES, move |tx| async move {
            let mut index = 0u64;
            while let Some(raw) = decoder.next().await {
                let raw = match raw {
                    Ok(raw) => raw,
                    Err(err) => return tx.fail(source.open_error(&input, err)),
                };
                let frame_index = index;
                index += 1;
                // From the frame number, in whole microseconds: ffmpeg-sidecar's `f32`
                // timestamp loses precision over the days a camera may run.
                let pts = Duration::from_micros(
                    (f64::from(raw.number) * 1e6 / output_fps).round() as u64
                );
                // ffmpeg already sampled below the capture rate; the sampler guards the rest.
                if !sampler.admit(pts) {
                    continue;
                }
                let frame = Frame {
                    source: id.clone(),
                    index: frame_index,
                    pts,
                    captured_at: Utc::now(),
                    pixels: FramePixels::Ephemeral(Arc::new(raw.pixels)),
                };
                if tx.push(Ok(frame)) == Pushed::Closed {
                    return;
                }
            }
            tracing::debug!(frame.source = %id.0, frames.received = index, frames.dropped = tx.dropped(), "camera stream ended");
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(api: DeviceApi, selector: DeviceSelector) -> DeviceSource {
        DeviceSource::with_input(
            FrameSourceId("cam".into()),
            Input::Device { api: Some(api), selector },
        )
    }

    fn listed(api: DeviceApi, names: &[&str]) -> Vec<CaptureDevice> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| CaptureDevice {
                index: i as u32,
                name: (*name).to_string(),
                kind: DeviceKind::Camera,
                selector: match api {
                    DeviceApi::V4l2 => DeviceSelector::Path(format!("/dev/video{}", i * 2).into()),
                    DeviceApi::AvFoundation => DeviceSelector::Index(i as u32),
                    DeviceApi::DirectShow => DeviceSelector::Name((*name).to_string()),
                },
            })
            .collect()
    }

    #[test]
    fn each_platform_opens_its_device_with_the_mode_asked_for() {
        let sampling = Sampling { fps: 2.0, max_long_edge: Some(640) };
        let cases = [
            (DeviceApi::AvFoundation, "0:none"),
            (DeviceApi::V4l2, "/dev/video0"),
            (DeviceApi::DirectShow, "video=Integrated Camera"),
        ];
        for (api, input) in cases {
            let source = device(api, DeviceSelector::Index(0)).with_video_size(1280, 720);
            let args = source
                .capture_args(&CaptureInput::Device { api, input: input.to_string() }, sampling);
            assert_eq!(
                args,
                [
                    "-nostdin",
                    "-f",
                    api.ffmpeg_format(),
                    "-framerate",
                    "30",
                    "-video_size",
                    "1280x720",
                    "-i",
                    input,
                    "-an",
                    "-sn",
                    "-dn",
                    "-vf",
                    "fps=2,scale=w='min(iw,640)':h='min(ih,640)':force_original_aspect_ratio=decrease",
                ],
                "{api}"
            );
        }
    }

    #[test]
    fn sampling_at_or_above_the_capture_rate_repeats_no_frame() {
        let source = device(DeviceApi::V4l2, DeviceSelector::Index(0)).with_frame_rate(15.0);
        let input = CaptureInput::Device { api: DeviceApi::V4l2, input: "/dev/video0".into() };
        let args = source.capture_args(&input, Sampling { fps: 15.0, max_long_edge: None });
        assert!(!args.iter().any(|a| a == "-vf"), "{args:?}");
        let args = source.capture_args(&input, Sampling { fps: 30.0, max_long_edge: None });
        assert!(!args.iter().any(|a| a == "-vf"), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-framerate", "15"]), "{args:?}");
    }

    #[test]
    fn the_test_pattern_is_paced_like_a_camera() {
        let source = DeviceSource::test_pattern(FrameSourceId("pattern".into()));
        let args = source
            .capture_args(&CaptureInput::TestPattern, Sampling { fps: 5.0, max_long_edge: None });
        assert_eq!(
            args,
            [
                "-nostdin",
                "-re",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x480:rate=30",
                "-an",
                "-sn",
                "-dn",
                "-vf",
                "fps=5",
            ]
        );
    }

    #[test]
    fn selectors_resolve_per_platform() {
        let mac = listed(DeviceApi::AvFoundation, &["Huddly GO", "Cam: Left"]);
        let linux = listed(DeviceApi::V4l2, &["Integrated Camera: Integrated C", "HUDDLY GO"]);
        let windows = listed(DeviceApi::DirectShow, &["Integrated Camera", "OBS Virtual Camera"]);
        let name = |n: &str| DeviceSelector::Name(n.into());
        let cases: [(DeviceApi, DeviceSelector, &[CaptureDevice], &str); 8] = [
            (DeviceApi::AvFoundation, DeviceSelector::Index(1), &[], "1:none"),
            (DeviceApi::AvFoundation, name("Huddly GO"), &[], "Huddly GO:none"),
            (DeviceApi::AvFoundation, name("Cam: Left"), &mac, "1:none"),
            (DeviceApi::V4l2, DeviceSelector::Index(4), &[], "/dev/video4"),
            (DeviceApi::V4l2, DeviceSelector::Path("/dev/video2".into()), &[], "/dev/video2"),
            (DeviceApi::V4l2, name("huddly go"), &linux, "/dev/video2"),
            (DeviceApi::DirectShow, DeviceSelector::Index(1), &windows, "video=OBS Virtual Camera"),
            (DeviceApi::DirectShow, name("Integrated Camera"), &[], "video=Integrated Camera"),
        ];
        for (api, selector, devices, expected) in cases {
            assert_eq!(needs_list(api, &selector), !devices.is_empty(), "{api} {selector}");
            assert_eq!(
                device_input(api, &selector, devices).unwrap(),
                expected,
                "{api} {selector}"
            );
        }
    }

    #[test]
    fn unopenable_selectors_are_refused_with_what_exists() {
        let windows = listed(DeviceApi::DirectShow, &["Integrated Camera"]);
        let err = device_input(DeviceApi::DirectShow, &DeviceSelector::Index(3), &windows)
            .unwrap_err()
            .to_string();
        assert!(err.contains("0 `Integrated Camera`"), "{err}");
        let err =
            device_input(DeviceApi::AvFoundation, &DeviceSelector::Path("/dev/video0".into()), &[])
                .unwrap_err();
        assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
        let err = device_input(DeviceApi::DirectShow, &DeviceSelector::Name("A: B".into()), &[])
            .unwrap_err();
        assert!(err.to_string().contains("alternative name"), "{err}");
    }

    #[test]
    fn bad_capture_modes_are_refused() {
        for rate in [0.0, -5.0, f32::NAN] {
            let source = device(DeviceApi::V4l2, DeviceSelector::Index(0)).with_frame_rate(rate);
            assert!(matches!(source.check_mode(), Err(VisionError::InvalidInput(_))), "{rate}");
        }
        let source = device(DeviceApi::V4l2, DeviceSelector::Index(0)).with_video_size(0, 480);
        assert!(matches!(source.check_mode(), Err(VisionError::InvalidInput(_))));
    }

    #[test]
    fn a_camera_that_does_not_open_says_what_to_try() {
        let source = device(DeviceApi::AvFoundation, DeviceSelector::Index(0));
        let input = CaptureInput::Device { api: DeviceApi::AvFoundation, input: "0:none".into() };
        let err = source.open_error(
            &input,
            VisionError::Backend {
                backend: "ffmpeg".into(),
                message: "Failed to create AV capture input device: Cannot use Huddly GO".into(),
            },
        );
        assert!(matches!(err, VisionError::Unavailable { .. }), "{err}");
        let text = err.to_string();
        assert!(text.contains("Cannot use Huddly GO"), "{text}");
        assert!(text.contains("Privacy & Security → Camera"), "{text}");
        let err = source.open_error(
            &input,
            VisionError::Backend {
                backend: "ffmpeg".into(),
                message: "[AVFoundation indev @ 0xbef00c140] [error] Invalid device index; [fatal] Error opening input files: Input/output error".into(),
            },
        );
        let text = err.to_string();
        assert!(
            text.contains(": Invalid device index; Error opening input files"),
            "the log prefixes go: {text}"
        );
        assert!(!text.contains("Privacy"), "a missing device is not a permission: {text}");
        assert!(text.contains("sources list"), "{text}");
    }
}
