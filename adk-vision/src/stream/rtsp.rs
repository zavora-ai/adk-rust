//! [`RtspSource`]: cameras over RTSP through `retina` (spec 3.7 §7.13, D-11).

use std::sync::Arc;
use std::time::Duration;

use adk_core::{ActingContext, CredentialBroker, CredentialSecret};
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use retina::client::{
    Credentials, PlayOptions, Session, SessionOptions, SetupOptions, TcpTransportOptions,
    Transport, UdpTransportOptions,
};
use retina::codec::{CodecItem, FrameFormat};
use url::Url;

use super::queue::{Pushed, QueueSender, spawn_frames};
use super::{FrameSource, FrameStream, SOURCE_QUEUE_FRAMES, Sampler};
use crate::error::{Result, VisionError};
use crate::media::{DecodeLimits, DecodedImage, Frame, FramePixels, FrameSourceId};
use crate::traits::Sampling;

/// How RTP reaches the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RtspTransport {
    /// Interleaved on the RTSP connection: works through NAT and firewalls.
    #[default]
    Tcp,
    /// UDP, on a LAN: `retina` keeps no reorder buffer, so out-of-order packets are lost.
    Udp,
}

enum RtspCredentials {
    Static {
        username: String,
        password: CredentialSecret,
    },
    Broker {
        broker: Arc<dyn CredentialBroker>,
        acting: Box<ActingContext>,
        username: String,
        scopes: Vec<String>,
    },
}

/// The video codec of the stream the source plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Jpeg,
    H264,
    H265,
}

/// A camera over RTSP: `retina` (MIT OR Apache-2.0) depacketises H.264, H.265 and MJPEG
/// (spec 3.7 §7.13).
///
/// MJPEG decodes in process, and the sampler skips frames before they are decoded. H.264
/// and H.265 decode through a user-installed `ffmpeg` (feature `ffmpeg`, Q9): every access
/// unit goes to the decoder, since later frames depend on earlier ones, and the sampler
/// keeps decoded frames by their RTP timestamps. Without the feature an H.264 or H.265
/// camera is refused with the feature to enable.
///
/// Camera credentials never appear in the source's identifier, its `Debug` output, logs,
/// audit records or errors (§9.1): credentials in the URL are moved out of it on
/// construction, and a [`CredentialBroker`] can supply the password per session instead.
///
/// # Example
///
/// ```
/// use adk_vision::stream::RtspSource;
/// use adk_vision::FrameSourceId;
///
/// let camera = RtspSource::new(FrameSourceId("line-1".into()), "rtsp://admin:hunter2@10.0.0.8:554/stream1").unwrap();
/// assert_eq!(camera.url().as_str(), "rtsp://10.0.0.8:554/stream1");
/// assert!(!format!("{camera:?}").contains("hunter2"));
/// ```
pub struct RtspSource {
    id: FrameSourceId,
    url: Url,
    credentials: Option<RtspCredentials>,
    transport: RtspTransport,
    limits: DecodeLimits,
    #[cfg(feature = "ffmpeg")]
    ffmpeg: super::FfmpegBinary,
}

impl std::fmt::Debug for RtspSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtspSource")
            .field("id", &self.id)
            .field("url", &self.url.as_str())
            .field("credentials", &self.credentials.as_ref().map(|_| "[redacted]"))
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl RtspSource {
    /// A camera at `url` (`rtsp://`). A user name and password in the URL become the
    /// source's credentials and are removed from the URL.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a URL that does not parse or is not `rtsp://`. The
    /// message never repeats the URL, which may hold a password.
    pub fn new(id: FrameSourceId, url: &str) -> Result<Self> {
        let mut url = Url::parse(url).map_err(|err| {
            VisionError::InvalidInput(format!("the RTSP URL of `{}` does not parse: {err}", id.0))
        })?;
        if url.scheme() != "rtsp" {
            return Err(VisionError::InvalidInput(format!(
                "the camera URL of `{}` is `{}://`; RtspSource reads rtsp:// URLs",
                id.0,
                url.scheme()
            )));
        }
        let credentials = (!url.username().is_empty()).then(|| RtspCredentials::Static {
            username: percent_decode(url.username()),
            password: CredentialSecret::new(percent_decode(url.password().unwrap_or_default())),
        });
        // Neither setter fails on an rtsp:// URL with a host.
        let _ = url.set_username("");
        let _ = url.set_password(None);
        Ok(Self {
            id,
            url,
            credentials,
            transport: RtspTransport::default(),
            limits: DecodeLimits::default(),
            #[cfg(feature = "ffmpeg")]
            ffmpeg: super::FfmpegBinary::from_env(),
        })
    }

    /// The camera URL, without credentials.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Authenticates as `username` with `password`.
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: CredentialSecret,
    ) -> Self {
        self.credentials = Some(RtspCredentials::Static { username: username.into(), password });
        self
    }

    /// Authenticates as `username` with a password `broker` mints for `acting` and this
    /// camera's URL, asked for at every session start.
    pub fn with_credential_broker(
        mut self,
        broker: Arc<dyn CredentialBroker>,
        acting: ActingContext,
        username: impl Into<String>,
        scopes: Vec<String>,
    ) -> Self {
        self.credentials = Some(RtspCredentials::Broker {
            broker,
            acting: Box::new(acting),
            username: username.into(),
            scopes,
        });
        self
    }

    /// Sets the RTP transport.
    pub fn with_transport(mut self, transport: RtspTransport) -> Self {
        self.transport = transport;
        self
    }

    /// Decodes H.264 and H.265 with `binary`.
    #[cfg(feature = "ffmpeg")]
    pub fn with_ffmpeg(mut self, binary: super::FfmpegBinary) -> Self {
        self.ffmpeg = binary;
        self
    }

    async fn credentials(&self) -> Result<Option<Credentials>> {
        Ok(match &self.credentials {
            None => None,
            Some(RtspCredentials::Static { username, password }) => Some(Credentials {
                username: username.clone(),
                password: password.expose().to_string(),
            }),
            Some(RtspCredentials::Broker { broker, acting, username, scopes }) => {
                let token = broker.token_for(acting, &self.url, scopes).await?;
                let password = token.authorize_for(&self.url)?;
                Some(Credentials {
                    username: username.clone(),
                    password: password.expose().to_string(),
                })
            }
        })
    }

    fn unavailable(&self, step: &str, err: &retina::Error) -> VisionError {
        VisionError::Unavailable {
            backend: "rtsp".into(),
            message: format!("{step} {} for `{}`: {err}", self.url, self.id.0),
        }
    }
}

#[async_trait]
impl FrameSource for RtspSource {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> Result<FrameStream> {
        let sampler = Sampler::new(sampling)?;
        let options = SessionOptions::default()
            .creds(self.credentials().await?)
            .user_agent(format!("adk-vision/{}", env!("CARGO_PKG_VERSION")));
        let mut session = Session::describe(self.url.clone(), options)
            .await
            .map_err(|err| self.unavailable("describing", &err))?;
        let found = session.streams().iter().enumerate().find_map(|(i, stream)| {
            let codec = match (stream.media(), stream.encoding_name()) {
                ("video" | "image", "jpeg") => Codec::Jpeg,
                ("video", "h264") => Codec::H264,
                ("video", "h265") => Codec::H265,
                _ => return None,
            };
            Some((i, codec))
        });
        let Some((stream, codec)) = found else {
            return Err(VisionError::Unsupported(format!(
                "camera `{}` offers no H.264, H.265 or MJPEG video stream",
                self.id.0
            )));
        };
        #[cfg(not(feature = "ffmpeg"))]
        if codec != Codec::Jpeg {
            return Err(VisionError::Unsupported(format!(
                "camera `{}` streams {codec:?}; decoding H.264 and H.265 needs adk-vision's `ffmpeg` feature and a user-installed ffmpeg",
                self.id.0
            )));
        }
        let transport = match self.transport {
            RtspTransport::Tcp => Transport::Tcp(TcpTransportOptions::default()),
            RtspTransport::Udp => Transport::Udp(UdpTransportOptions::default()),
        };
        session
            .setup(
                stream,
                SetupOptions::default().transport(transport).frame_format(FrameFormat::SIMPLE),
            )
            .await
            .map_err(|err| self.unavailable("setting up", &err))?;
        let playing = session
            .play(PlayOptions::default())
            .await
            .map_err(|err| self.unavailable("playing", &err))?
            .demuxed()
            .map_err(|err| self.unavailable("demuxing", &err))?;
        tracing::info!(frame.source = %self.id.0, rtsp.codec = ?codec, "camera playing");
        let playing = Box::pin(playing);
        let id = self.id.clone();
        let label = format!("{} for `{}`", self.url, self.id.0);
        match codec {
            Codec::Jpeg => {
                let limits = self.limits;
                Ok(spawn_frames(SOURCE_QUEUE_FRAMES, move |tx| {
                    mjpeg(playing, tx, id, label, sampler, limits)
                }))
            }
            #[cfg(feature = "ffmpeg")]
            Codec::H264 | Codec::H265 => {
                let format = if codec == Codec::H264 { "h264" } else { "hevc" };
                let decoder = super::ffmpeg::StreamDecoder::spawn(
                    &self.ffmpeg,
                    format,
                    sampling.max_long_edge,
                )?;
                Ok(spawn_frames(SOURCE_QUEUE_FRAMES, move |tx| {
                    h26x(playing, decoder, tx, id, label, sampler)
                }))
            }
            #[cfg(not(feature = "ffmpeg"))]
            Codec::H264 | Codec::H265 => unreachable!("refused above without the ffmpeg feature"),
        }
    }
}

/// `%XX` escapes in a URL's user information, decoded.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

type Playing = std::pin::Pin<Box<retina::client::Demuxed>>;

fn pts_of(frame: &retina::codec::VideoFrame) -> Duration {
    Duration::from_secs_f64(frame.timestamp().elapsed_secs().max(0.0))
}

fn read_error(label: &str, err: &retina::Error) -> VisionError {
    VisionError::Unavailable { backend: "rtsp".into(), message: format!("reading {label}: {err}") }
}

/// MJPEG: sample first, then decode only the frames kept.
async fn mjpeg(
    mut playing: Playing,
    tx: QueueSender<Result<Frame>>,
    id: FrameSourceId,
    label: String,
    mut sampler: Sampler,
    limits: DecodeLimits,
) {
    let mut index = 0u64;
    while let Some(item) = playing.next().await {
        let frame = match item {
            Ok(CodecItem::VideoFrame(frame)) => frame,
            Ok(_) => continue,
            Err(err) => return tx.fail(read_error(&label, &err)),
        };
        let pts = pts_of(&frame);
        let frame_index = index;
        index += 1;
        if !sampler.admit(pts) {
            continue;
        }
        let captured_at = Utc::now();
        let jpeg = frame.into_data();
        let fitter = sampler.clone();
        let decoded = tokio::task::spawn_blocking(move || {
            DecodedImage::decode(&jpeg, &limits).map(|(pixels, _)| fitter.fit(pixels))
        })
        .await;
        let pixels = match decoded {
            Ok(Ok(pixels)) => pixels,
            Ok(Err(err)) => {
                // A corrupt frame (a lost packet) is skipped; the next one is whole.
                tracing::debug!(frame.source = %id.0, frame.index = frame_index, error = %err, "MJPEG frame skipped");
                continue;
            }
            Err(err) => {
                return tx.fail(VisionError::Decode(format!("MJPEG decode panicked: {err}")));
            }
        };
        let frame = Frame {
            source: id.clone(),
            index: frame_index,
            pts,
            captured_at,
            pixels: FramePixels::Ephemeral(Arc::new(pixels)),
        };
        if tx.push(Ok(frame)) == Pushed::Closed {
            return;
        }
    }
    tracing::debug!(frame.source = %id.0, frames.received = index, frames.dropped = tx.dropped(), "camera stream ended");
}

/// H.264 and H.265: every access unit to ffmpeg; decoded frames matched to their RTP
/// timestamps in order, then sampled. Cameras do not reorder frames, so decode order is
/// presentation order.
#[cfg(feature = "ffmpeg")]
async fn h26x(
    mut playing: Playing,
    mut decoder: super::ffmpeg::StreamDecoder,
    tx: QueueSender<Result<Frame>>,
    id: FrameSourceId,
    label: String,
    mut sampler: Sampler,
) {
    let mut pending = std::collections::VecDeque::new();
    let mut index = 0u64;
    let mut input_open = true;
    loop {
        tokio::select! {
            item = playing.next(), if input_open => match item {
                Some(Ok(CodecItem::VideoFrame(frame))) => {
                    pending.push_back(pts_of(&frame));
                    if let Err(err) = decoder.feed(frame.into_data()).await {
                        return tx.fail(err);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => return tx.fail(read_error(&label, &err)),
                None => {
                    input_open = false;
                    decoder.finish();
                }
            },
            decoded = decoder.next() => match decoded {
                Some(Ok(pixels)) => {
                    let pts = pending.pop_front().unwrap_or_default();
                    let frame_index = index;
                    index += 1;
                    if !sampler.admit(pts) {
                        continue;
                    }
                    let frame = Frame {
                        source: id.clone(),
                        index: frame_index,
                        pts,
                        captured_at: Utc::now(),
                        pixels: FramePixels::Ephemeral(Arc::new(sampler.fit(pixels))),
                    };
                    if tx.push(Ok(frame)) == Pushed::Closed {
                        return;
                    }
                }
                Some(Err(err)) => return tx.fail(err),
                None => {
                    tracing::debug!(frame.source = %id.0, frames.received = index, frames.dropped = tx.dropped(), "camera stream ended");
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_leave_the_url_and_debug_output() {
        let camera =
            RtspSource::new(FrameSourceId("cam".into()), "rtsp://op%40site:p%3Ass@cam.local/live")
                .unwrap();
        assert_eq!(camera.url().as_str(), "rtsp://cam.local/live");
        let text = format!("{camera:?}");
        assert!(!text.contains("p%3Ass") && !text.contains("p:ss"), "{text}");
        match camera.credentials {
            Some(RtspCredentials::Static { username, password }) => {
                assert_eq!(username, "op@site");
                assert_eq!(password.expose(), "p:ss");
            }
            _ => panic!("expected static credentials"),
        }
    }

    #[test]
    fn non_rtsp_urls_are_refused_without_echoing_them() {
        let err = RtspSource::new(FrameSourceId("cam".into()), "http://u:secret@cam/").unwrap_err();
        assert!(!err.to_string().contains("secret"), "{err}");
        let err =
            RtspSource::new(FrameSourceId("cam".into()), "not a url with secret").unwrap_err();
        assert!(!err.to_string().contains("secret"), "{err}");
    }
}
