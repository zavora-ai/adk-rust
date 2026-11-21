//! An in-process RTSP server for tests: one stream, RTP interleaved on the RTSP connection,
//! MJPEG (RFC 2435) encoded here or H.264 (RFC 6184) access units handed in.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::h264::H264Stream;
use super::jpeg;
use crate::error::{Result, VisionError};
use crate::media::DecodedImage;

/// The largest RTP payload the server sends.
const MAX_PAYLOAD: usize = 1_400;

/// Renders frame `index` of an MJPEG stream.
pub type FrameRenderer = Arc<dyn Fn(u64) -> DecodedImage + Send + Sync>;

/// What the server streams.
#[derive(Clone)]
pub enum TestVideo {
    /// Frames rendered on demand, encoded as baseline 4:2:0 JPEG and sent as RTP/JPEG.
    Mjpeg {
        /// Draws frame `index`; its sides must be multiples of 8, at most 2040.
        render: FrameRenderer,
        /// Frames in the stream.
        frames: u64,
        /// Frames a second.
        fps: u32,
        /// JPEG quality, 1–100.
        quality: u8,
    },
    /// H.264 access units, sent as RTP single NAL units and FU-A fragments.
    H264 {
        /// The stream.
        stream: Arc<H264Stream>,
        /// Frames a second.
        fps: u32,
    },
}

impl std::fmt::Debug for TestVideo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mjpeg { frames, fps, quality, .. } => f
                .debug_struct("Mjpeg")
                .field("frames", frames)
                .field("fps", fps)
                .field("quality", quality)
                .finish_non_exhaustive(),
            Self::H264 { stream, fps } => f
                .debug_struct("H264")
                .field("units", &stream.units.len())
                .field("fps", fps)
                .finish(),
        }
    }
}

impl TestVideo {
    fn frames(&self) -> u64 {
        match self {
            Self::Mjpeg { frames, .. } => *frames,
            Self::H264 { stream, .. } => stream.units.len() as u64,
        }
    }

    fn fps(&self) -> u32 {
        match self {
            Self::Mjpeg { fps, .. } | Self::H264 { fps, .. } => (*fps).max(1),
        }
    }
}

/// How the server plays its stream.
#[derive(Clone, Debug)]
pub struct RtspTestOptions {
    /// Playback speed: 1.0 is real time, 4.0 four times faster, 0.0 as fast as the
    /// connection takes it. RTP timestamps always follow the stream's own frame rate.
    pub speed: f64,
    /// Plays the stream again from the start when it ends, until the client goes.
    pub looping: bool,
    /// Requires HTTP Basic credentials, `(user, password)`.
    pub credentials: Option<(String, String)>,
    /// The URL path.
    pub path: String,
}

impl Default for RtspTestOptions {
    fn default() -> Self {
        Self { speed: 1.0, looping: false, credentials: None, path: "conveyor".into() }
    }
}

/// An RTSP server on `127.0.0.1` serving one stream to any number of clients, until
/// [`shutdown`](Self::shutdown) or drop.
///
/// It answers `OPTIONS`, `DESCRIBE`, `SETUP` (TCP interleaved only), `PLAY`, `TEARDOWN`
/// and keep-alive `GET_PARAMETER`s, which is what `retina` and `ffmpeg` ask.
///
/// # Example
///
/// ```
/// use adk_vision::stream::fixtures::{ConveyorClip, ConveyorSpec, RtspTestServer, TestVideo};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let clip = std::sync::Arc::new(ConveyorClip::new(ConveyorSpec::default()));
/// let server = RtspTestServer::start(TestVideo::Mjpeg {
///     render: std::sync::Arc::new({ let clip = clip.clone(); move |i| clip.render(i) }),
///     frames: 20,
///     fps: 10,
///     quality: 80,
/// })
/// .await
/// .unwrap();
/// assert!(server.url().starts_with("rtsp://127.0.0.1:"));
/// server.shutdown().await;
/// # });
/// ```
pub struct RtspTestServer {
    addr: SocketAddr,
    path: String,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    sent: Arc<AtomicU64>,
}

impl std::fmt::Debug for RtspTestServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtspTestServer")
            .field("addr", &self.addr)
            .field("path", &self.path)
            .finish()
    }
}

impl Drop for RtspTestServer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Shared {
    video: TestVideo,
    options: RtspTestOptions,
    sent: Arc<AtomicU64>,
    cancel: CancellationToken,
    /// The encoded MJPEG frames, cached across clients and loops.
    jpegs: std::sync::Mutex<std::collections::HashMap<u64, Arc<jpeg::Baseline420>>>,
}

impl RtspTestServer {
    /// Serves `video` in real time, once, without credentials, at `/conveyor`.
    ///
    /// # Errors
    ///
    /// [`VisionError::Io`] when no local port can be bound.
    pub async fn start(video: TestVideo) -> Result<Self> {
        Self::start_with(video, RtspTestOptions::default()).await
    }

    /// Serves `video` with `options`.
    ///
    /// # Errors
    ///
    /// [`VisionError::Io`] when no local port can be bound, [`VisionError::InvalidInput`]
    /// for MJPEG frames whose sides are not multiples of 8 up to 2040.
    pub async fn start_with(video: TestVideo, options: RtspTestOptions) -> Result<Self> {
        if let TestVideo::Mjpeg { render, frames, .. } = &video
            && *frames > 0
        {
            let (w, h) = render(0).size();
            if w % 8 != 0 || h % 8 != 0 || w > 2040 || h > 2040 {
                return Err(VisionError::InvalidInput(format!(
                    "RTP/JPEG carries sides that are multiples of 8 up to 2040; the frames are {w}x{h}"
                )));
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|err| VisionError::io("bind the RTSP test server on", "127.0.0.1:0", err))?;
        let addr = listener
            .local_addr()
            .map_err(|err| VisionError::io("read the address of", "127.0.0.1:0", err))?;
        let cancel = CancellationToken::new();
        let sent = Arc::new(AtomicU64::new(0));
        let path = options.path.trim_matches('/').to_string();
        let shared = Arc::new(Shared {
            video,
            options,
            sent: Arc::clone(&sent),
            cancel: cancel.clone(),
            jpegs: std::sync::Mutex::new(std::collections::HashMap::new()),
        });
        let accept_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = accept_cancel.cancelled() => return,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { continue };
                let shared = Arc::clone(&shared);
                tokio::spawn(async move {
                    if let Err(err) = serve(stream, addr, shared).await {
                        tracing::debug!(error = %err, "RTSP test connection ended");
                    }
                });
            }
        });
        Ok(Self { addr, path, cancel, task: Some(task), sent })
    }

    /// The stream's URL.
    pub fn url(&self) -> String {
        format!("rtsp://{}/{}", self.addr, self.path)
    }

    /// The server's address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Frames sent so far, over every connection.
    pub fn frames_sent(&self) -> u64 {
        self.sent.load(Ordering::SeqCst)
    }

    /// Stops accepting, ends every connection and waits for the listener to close.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

struct Request {
    method: String,
    cseq: String,
    authorization: Option<String>,
}

async fn read_request(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> std::io::Result<Option<Request>> {
    loop {
        // Interleaved data from the client (RTCP receiver reports) is read and dropped.
        let first = {
            let buf = reader.fill_buf().await?;
            if buf.is_empty() {
                return Ok(None);
            }
            buf[0]
        };
        if first == b'$' {
            let mut header = [0u8; 4];
            reader.read_exact(&mut header).await?;
            let len = u16::from_be_bytes([header[2], header[3]]) as usize;
            let mut skip = vec![0u8; len];
            reader.read_exact(&mut skip).await?;
            continue;
        }
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let method = line.split_whitespace().next().unwrap_or_default().to_string();
        let mut cseq = String::from("0");
        let mut authorization = None;
        let mut body = 0usize;
        loop {
            line.clear();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(None);
            }
            let header = line.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                let value = value.trim().to_string();
                match name.trim().to_ascii_lowercase().as_str() {
                    "cseq" => cseq = value,
                    "authorization" => authorization = Some(value),
                    "content-length" => body = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
        if body > 0 {
            let mut skip = vec![0u8; body];
            reader.read_exact(&mut skip).await?;
        }
        if method.is_empty() {
            continue;
        }
        return Ok(Some(Request { method, cseq, authorization }));
    }
}

async fn respond(
    writer: &Mutex<OwnedWriteHalf>,
    status: &str,
    cseq: &str,
    headers: &[(&str, String)],
    body: &str,
) -> std::io::Result<()> {
    let mut text = format!("RTSP/1.0 {status}\r\nCSeq: {cseq}\r\nServer: adk-vision-test\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() {
        text.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    text.push_str("\r\n");
    text.push_str(body);
    writer.lock().await.write_all(text.as_bytes()).await
}

fn sdp(video: &TestVideo) -> String {
    let media = match video {
        TestVideo::Mjpeg { fps, .. } => {
            format!("m=video 0 RTP/AVP 26\r\na=rtpmap:26 JPEG/90000\r\na=framerate:{fps}\r\n")
        }
        TestVideo::H264 { stream, fps } => {
            let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
            let profile = stream.sps.get(1..4).map(hex::encode).unwrap_or_else(|| "42e01f".into());
            format!(
                "m=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id={profile};sprop-parameter-sets={},{}\r\na=framerate:{fps}\r\n",
                b64(&stream.sps),
                b64(&stream.pps)
            )
        }
    };
    format!(
        "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=adk-vision test stream\r\nc=IN IP4 0.0.0.0\r\nt=0 0\r\na=control:*\r\n{media}a=control:trackID=0\r\n"
    )
}

const SSRC: u32 = 0x5AD4_1E70;
const START_SEQ: u16 = 1_000;
const START_TS: u32 = 90_000;

async fn serve(stream: TcpStream, addr: SocketAddr, shared: Arc<Shared>) -> std::io::Result<()> {
    let (read, write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let writer = Arc::new(Mutex::new(write));
    let base = format!("rtsp://{addr}/{}", shared.options.path.trim_matches('/'));
    let session = format!("{:08x}", SSRC ^ addr.port() as u32);
    let mut playing: Option<CancellationToken> = None;
    // Cancelled when a stream that does not loop has been sent: the server then hangs up,
    // which is how an RTSP client learns the stream is over.
    let ended = CancellationToken::new();
    let expected = shared.options.credentials.as_ref().map(|(user, password)| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        )
    });
    let result = loop {
        let request = tokio::select! {
            () = shared.cancel.cancelled() => break Ok(()),
            () = ended.cancelled() => break Ok(()),
            request = read_request(&mut reader) => request,
        };
        let Some(request) = request? else { break Ok(()) };
        if let Some(expected) = &expected
            && request.method != "OPTIONS"
            && request.authorization.as_deref() != Some(expected.as_str())
        {
            respond(
                &writer,
                "401 Unauthorized",
                &request.cseq,
                &[("WWW-Authenticate", "Basic realm=\"adk-vision\"".into())],
                "",
            )
            .await?;
            continue;
        }
        let session_header = ("Session", format!("{session};timeout=60"));
        match request.method.as_str() {
            "OPTIONS" => {
                respond(
                    &writer,
                    "200 OK",
                    &request.cseq,
                    &[("Public", "OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN, GET_PARAMETER".into())],
                    "",
                )
                .await?;
            }
            "DESCRIBE" => {
                respond(
                    &writer,
                    "200 OK",
                    &request.cseq,
                    &[
                        ("Content-Base", format!("{base}/")),
                        ("Content-Type", "application/sdp".into()),
                    ],
                    &sdp(&shared.video),
                )
                .await?;
            }
            "SETUP" => {
                respond(
                    &writer,
                    "200 OK",
                    &request.cseq,
                    &[
                        session_header,
                        (
                            "Transport",
                            format!("RTP/AVP/TCP;unicast;interleaved=0-1;ssrc={SSRC:08X}"),
                        ),
                    ],
                    "",
                )
                .await?;
            }
            "PLAY" => {
                respond(
                    &writer,
                    "200 OK",
                    &request.cseq,
                    &[
                        session_header,
                        ("Range", "npt=0.000-".into()),
                        (
                            "RTP-Info",
                            format!("url={base}/trackID=0;seq={START_SEQ};rtptime={START_TS}"),
                        ),
                    ],
                    "",
                )
                .await?;
                if playing.is_none() {
                    let stop = shared.cancel.child_token();
                    playing = Some(stop.clone());
                    tokio::spawn(play(
                        Arc::clone(&shared),
                        Arc::clone(&writer),
                        stop,
                        ended.clone(),
                    ));
                }
            }
            "TEARDOWN" => {
                respond(&writer, "200 OK", &request.cseq, &[session_header], "").await?;
                break Ok(());
            }
            "GET_PARAMETER" | "SET_PARAMETER" => {
                respond(&writer, "200 OK", &request.cseq, &[session_header], "").await?;
            }
            _ => respond(&writer, "501 Not Implemented", &request.cseq, &[], "").await?,
        }
    };
    if let Some(stop) = playing {
        stop.cancel();
    }
    result
}

fn rtp_header(marker: bool, payload_type: u8, seq: u16, timestamp: u32) -> [u8; 12] {
    let mut header = [0u8; 12];
    header[0] = 0x80;
    header[1] = (u8::from(marker) << 7) | payload_type;
    header[2..4].copy_from_slice(&seq.to_be_bytes());
    header[4..8].copy_from_slice(&timestamp.to_be_bytes());
    header[8..12].copy_from_slice(&SSRC.to_be_bytes());
    header
}

/// RTP packets for one RTP/JPEG frame: the main header on every packet, the quantisation
/// tables on the first (Q = 255: tables in band, RFC 2435 §3.1.8).
fn jpeg_payloads(frame: &jpeg::Baseline420) -> Vec<Vec<u8>> {
    let mut payloads = Vec::new();
    let mut offset = 0usize;
    while offset < frame.scan.len() || offset == 0 {
        let mut payload = Vec::with_capacity(MAX_PAYLOAD);
        let [_, o1, o2, o3] = (offset as u32).to_be_bytes();
        payload.extend_from_slice(&[
            0,
            o1,
            o2,
            o3,
            1,
            255,
            (frame.width / 8) as u8,
            (frame.height / 8) as u8,
        ]);
        if offset == 0 {
            payload.extend_from_slice(&[0, 0, 0, 128]);
            payload.extend_from_slice(&frame.tables[0]);
            payload.extend_from_slice(&frame.tables[1]);
        }
        let room = MAX_PAYLOAD - payload.len();
        let end = (offset + room).min(frame.scan.len());
        payload.extend_from_slice(&frame.scan[offset..end]);
        payloads.push(payload);
        offset = end;
        if offset >= frame.scan.len() {
            break;
        }
    }
    payloads
}

/// RTP payloads for one H.264 access unit: a NAL unit whole when it fits, FU-A fragments
/// otherwise (RFC 6184 §5.8).
fn h264_payloads(unit: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut payloads = Vec::new();
    for nal in unit {
        if nal.is_empty() {
            continue;
        }
        if nal.len() <= MAX_PAYLOAD {
            payloads.push(nal.clone());
            continue;
        }
        let (header, body) = (nal[0], &nal[1..]);
        let indicator = (header & 0xE0) | 28;
        let chunks: Vec<&[u8]> = body.chunks(MAX_PAYLOAD - 2).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let fu =
                (u8::from(i == 0) << 7) | (u8::from(i + 1 == chunks.len()) << 6) | (header & 0x1F);
            let mut payload = Vec::with_capacity(chunk.len() + 2);
            payload.extend_from_slice(&[indicator, fu]);
            payload.extend_from_slice(chunk);
            payloads.push(payload);
        }
    }
    payloads
}

async fn play(
    shared: Arc<Shared>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
    stop: CancellationToken,
    ended: CancellationToken,
) {
    let fps = shared.video.fps();
    let frames = shared.video.frames();
    let started = tokio::time::Instant::now();
    let mut seq = START_SEQ;
    let mut played = 0u64;
    let payload_type = match &shared.video {
        TestVideo::Mjpeg { .. } => 26,
        TestVideo::H264 { .. } => 96,
    };
    loop {
        for index in 0..frames {
            if stop.is_cancelled() {
                return;
            }
            if shared.options.speed > 0.0 {
                let due =
                    Duration::from_secs_f64(played as f64 / f64::from(fps) / shared.options.speed);
                tokio::select! {
                    () = stop.cancelled() => return,
                    () = tokio::time::sleep_until(started + due) => {}
                }
            }
            let timestamp = START_TS.wrapping_add((played * 90_000 / u64::from(fps)) as u32);
            let payloads = match &shared.video {
                TestVideo::Mjpeg { render, quality, .. } => {
                    let cached =
                        shared.jpegs.lock().ok().and_then(|cache| cache.get(&index).cloned());
                    let frame = match cached {
                        Some(frame) => frame,
                        None => {
                            let (render, quality) = (Arc::clone(render), *quality);
                            let Ok(frame) = tokio::task::spawn_blocking(move || {
                                Arc::new(jpeg::encode(&render(index), quality))
                            })
                            .await
                            else {
                                return;
                            };
                            if let Ok(mut cache) = shared.jpegs.lock() {
                                cache.insert(index, Arc::clone(&frame));
                            }
                            frame
                        }
                    };
                    jpeg_payloads(&frame)
                }
                TestVideo::H264 { stream, .. } => h264_payloads(&stream.units[index as usize]),
            };
            let mut packet = Vec::new();
            let last = payloads.len().saturating_sub(1);
            for (i, payload) in payloads.iter().enumerate() {
                let rtp = rtp_header(i == last, payload_type, seq, timestamp);
                seq = seq.wrapping_add(1);
                let len = (rtp.len() + payload.len()) as u16;
                packet.push(b'$');
                packet.push(0);
                packet.extend_from_slice(&len.to_be_bytes());
                packet.extend_from_slice(&rtp);
                packet.extend_from_slice(payload);
            }
            if writer.lock().await.write_all(&packet).await.is_err() {
                return;
            }
            played += 1;
            shared.sent.fetch_add(1, Ordering::SeqCst);
        }
        if !shared.options.looping {
            let _ = writer.lock().await.shutdown().await;
            ended.cancel();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_frames_split_with_offsets_and_tables_first() {
        let frame = jpeg::Baseline420 {
            width: 64,
            height: 48,
            tables: [[1; 64], [2; 64]],
            scan: (0..4_000u32).map(|i| i as u8).collect(),
        };
        let payloads = jpeg_payloads(&frame);
        // 1,260 bytes of scan after the tables, then 1,392 a packet.
        assert_eq!(payloads.len(), 3);
        assert_eq!(&payloads[0][..8], &[0, 0, 0, 0, 1, 255, 8, 6]);
        assert_eq!(&payloads[0][8..12], &[0, 0, 0, 128]);
        let second_offset = u32::from_be_bytes([0, payloads[1][1], payloads[1][2], payloads[1][3]]);
        assert_eq!(second_offset as usize, MAX_PAYLOAD - 8 - 4 - 128);
        let carried: usize = payloads
            .iter()
            .enumerate()
            .map(|(i, p)| p.len() - 8 - if i == 0 { 132 } else { 0 })
            .sum();
        assert_eq!(carried, 4_000);
    }

    #[test]
    fn large_nal_units_become_fu_a_fragments() {
        let nal: Vec<u8> = std::iter::once(0x65).chain((0..3_000u32).map(|i| i as u8)).collect();
        let payloads = h264_payloads(&[vec![0x09, 0xF0], nal]);
        assert_eq!(payloads[0], vec![0x09, 0xF0]);
        let fragments = &payloads[1..];
        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[0][0] & 0x1F, 28);
        assert_eq!(fragments[0][1], 0x80 | 5);
        assert_eq!(fragments[2][1], 0x40 | 5);
        let body: usize = fragments.iter().map(|f| f.len() - 2).sum();
        assert_eq!(body, 3_000);
    }
}
