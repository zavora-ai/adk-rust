//! H.264 and MP4 fixtures encoded by a user-installed `ffmpeg` (libx264): the project ships
//! no encoder and no encoded clip.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Result, VisionError};
use crate::media::DecodedImage;

/// An H.264 elementary stream split into access units, for the RTSP test server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct H264Stream {
    /// The first sequence parameter set, without start code.
    pub sps: Vec<u8>,
    /// The first picture parameter set, without start code.
    pub pps: Vec<u8>,
    /// Access units in decode order, each its NAL units without start codes; access unit
    /// delimiters are dropped.
    pub units: Vec<Vec<Vec<u8>>>,
}

/// NAL units of an Annex B byte stream, start codes removed.
fn nal_units(bytes: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let end = starts.get(n + 1).map_or(bytes.len(), |next| next - 3);
            // A four-byte start code leaves its leading zero on the unit before.
            let mut unit = &bytes[start..end];
            while let [rest @ .., 0] = unit {
                unit = rest;
            }
            unit
        })
        .filter(|unit| !unit.is_empty())
        .collect()
}

impl H264Stream {
    /// Splits an Annex B stream at its access unit delimiters (NAL type 9).
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when the stream has no SPS, no PPS or no delimiters.
    pub fn from_annex_b(bytes: &[u8]) -> Result<Self> {
        let mut stream = Self::default();
        let mut current: Option<Vec<Vec<u8>>> = None;
        for unit in nal_units(bytes) {
            match unit[0] & 0x1F {
                9 => {
                    if let Some(done) = current.take().filter(|u| !u.is_empty()) {
                        stream.units.push(done);
                    }
                    current = Some(Vec::new());
                    continue;
                }
                7 if stream.sps.is_empty() => stream.sps = unit.to_vec(),
                8 if stream.pps.is_empty() => stream.pps = unit.to_vec(),
                _ => {}
            }
            if let Some(au) = current.as_mut() {
                au.push(unit.to_vec());
            }
        }
        if let Some(done) = current.filter(|u| !u.is_empty()) {
            stream.units.push(done);
        }
        if stream.sps.is_empty() || stream.pps.is_empty() || stream.units.is_empty() {
            return Err(VisionError::InvalidInput(
                "an H.264 fixture needs an SPS, a PPS and access unit delimiters (x264 `aud=1`)"
                    .into(),
            ));
        }
        Ok(stream)
    }

    /// Encodes `frames` frames drawn by `render` at `fps` with libx264 through the `ffmpeg`
    /// at `ffmpeg`: baseline-friendly settings for a camera — no B-frames, a keyframe a
    /// second, delimiters and parameter sets on every keyframe.
    ///
    /// # Errors
    ///
    /// [`VisionError::Unavailable`] when `ffmpeg` does not run or has no libx264, and
    /// [`from_annex_b`](Self::from_annex_b)'s errors.
    pub fn encode(
        ffmpeg: &Path,
        render: &dyn Fn(u64) -> DecodedImage,
        frames: u64,
        fps: u32,
    ) -> Result<Self> {
        let bytes = run_encoder(
            ffmpeg,
            render,
            frames,
            fps,
            &[
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-bf",
                "0",
                "-pix_fmt",
                "yuv420p",
                "-x264-params",
            ],
            &[&format!("aud=1:repeat-headers=1:keyint={fps}"), "-f", "h264", "pipe:1"],
        )?;
        Self::from_annex_b(&bytes)
    }
}

/// Writes `frames` frames drawn by `render` at `fps` to an H.264 MP4 at `path`, with the
/// `ffmpeg` at `ffmpeg`.
///
/// # Errors
///
/// [`VisionError::Unavailable`] when `ffmpeg` does not run or fails, or an I/O error writing
/// `path`.
pub fn write_mp4(
    ffmpeg: &Path,
    path: &Path,
    render: &dyn Fn(u64) -> DecodedImage,
    frames: u64,
    fps: u32,
) -> Result<()> {
    let bytes = run_encoder(
        ffmpeg,
        render,
        frames,
        fps,
        &["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-movflags"],
        &["frag_keyframe+empty_moov", "-f", "mp4", "pipe:1"],
    )?;
    std::fs::write(path, bytes).map_err(|err| VisionError::io("write the MP4 fixture", path, err))
}

fn run_encoder(
    ffmpeg: &Path,
    render: &dyn Fn(u64) -> DecodedImage,
    frames: u64,
    fps: u32,
    codec: &[&str],
    output: &[&str],
) -> Result<Vec<u8>> {
    let first = render(0);
    let (w, h) = first.size();
    let unavailable =
        |message: String| VisionError::Unavailable { backend: "ffmpeg".into(), message };
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{w}x{h}"), "-r", &fps.to_string(), "-i", "pipe:0"])
        .args(codec)
        .args(output)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| unavailable(format!("cannot run `{}`: {err}", ffmpeg.display())))?;
    let mut stdin = child.stdin.take().ok_or_else(|| unavailable("no stdin".into()))?;
    let mut stdout = child.stdout.take().ok_or_else(|| unavailable("no stdout".into()))?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).map(|_| out)
    });
    let mut wrote = Ok(());
    for index in 0..frames {
        let frame = if index == 0 { first.clone() } else { render(index) };
        if let Err(err) = stdin.write_all(frame.rgba()) {
            wrote = Err(err);
            break;
        }
    }
    drop(stdin);
    let out = reader.join().map_err(|_| unavailable("the ffmpeg reader panicked".into()))?;
    let status = child.wait().map_err(|err| unavailable(err.to_string()))?;
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    if !status.success() || wrote.is_err() {
        return Err(unavailable(format!("ffmpeg exited with {status}: {}", stderr.trim())));
    }
    out.map_err(|err| unavailable(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_at_delimiters_and_keeps_parameter_sets() {
        let stream = [
            &[0, 0, 0, 1, 0x09, 0xF0][..],
            &[0, 0, 0, 1, 0x67, 1, 2][..],
            &[0, 0, 1, 0x68, 3][..],
            &[0, 0, 1, 0x65, 4, 5, 6][..],
            &[0, 0, 0, 1, 0x09, 0x30][..],
            &[0, 0, 1, 0x41, 7][..],
        ]
        .concat();
        let parsed = H264Stream::from_annex_b(&stream).unwrap();
        assert_eq!(parsed.sps, vec![0x67, 1, 2]);
        assert_eq!(parsed.pps, vec![0x68, 3]);
        assert_eq!(parsed.units.len(), 2);
        assert_eq!(parsed.units[0], vec![vec![0x67, 1, 2], vec![0x68, 3], vec![0x65, 4, 5, 6]]);
        assert_eq!(parsed.units[1], vec![vec![0x41, 7]]);
    }

    #[test]
    fn a_stream_without_parameter_sets_is_refused() {
        assert!(H264Stream::from_annex_b(&[0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x41, 7]).is_err());
    }
}
