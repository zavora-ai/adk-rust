//! Reads an MP4 or MOV container's video track — codec, coded size, duration and frame
//! count — from its boxes, without decoding a frame.

use serde::{Deserialize, Serialize};

/// The video codecs a clip may carry: the three `sam-3.1` accepts, which Gemini accepts
/// too.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VideoCodec {
    /// H.264 (`avc1`, `avc3`).
    H264,
    /// H.265 (`hvc1`, `hev1`).
    H265,
    /// MPEG-4 Part 2 (`mp4v`).
    Mpeg4,
}

/// What the container says about its first video track.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Probe {
    pub mime_type: &'static str,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub frames: u64,
}

/// One box: its type and payload.
struct Atom<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

/// The boxes laid end to end in `data`.
fn atoms(mut data: &[u8]) -> Result<Vec<Atom<'_>>, String> {
    let mut out = Vec::new();
    while !data.is_empty() {
        if data.len() < 8 {
            return Err(format!("{} trailing bytes are not a box", data.len()));
        }
        let size32 = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let kind = [data[4], data[5], data[6], data[7]];
        let (header, size) = match size32 {
            0 => (8, data.len() as u64),
            1 => {
                let large = data.get(8..16).ok_or("a 64-bit box size is cut short")?;
                (16, u64::from_be_bytes(large.try_into().map_err(|_| "bad box size")?))
            }
            n => (8, u64::from(n)),
        };
        let size = usize::try_from(size).map_err(|_| "a box is larger than memory")?;
        if size < header || size > data.len() {
            return Err(format!(
                "box `{}` claims {size} bytes where {} remain",
                String::from_utf8_lossy(&kind),
                data.len()
            ));
        }
        out.push(Atom { kind, body: &data[header..size] });
        data = &data[size..];
    }
    Ok(out)
}

fn child<'a>(atoms: &'a [Atom<'a>], kind: &[u8; 4]) -> Option<&'a Atom<'a>> {
    atoms.iter().find(|a| &a.kind == kind)
}

fn be32(data: &[u8], at: usize) -> Result<u32, String> {
    data.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "a box is cut short".to_string())
}

fn be64(data: &[u8], at: usize) -> Result<u64, String> {
    data.get(at..at + 8)
        .map(|b| u64::from_be_bytes(b.try_into().unwrap_or([0; 8])))
        .ok_or_else(|| "a box is cut short".to_string())
}

/// `(timescale, duration)` of an `mvhd` or `mdhd` box.
fn timing(body: &[u8]) -> Result<(u32, u64), String> {
    match body.first() {
        Some(1) => Ok((be32(body, 20)?, be64(body, 24)?)),
        Some(_) => Ok((be32(body, 12)?, u64::from(be32(body, 16)?))),
        None => Err("an empty header box".into()),
    }
}

fn millis(timescale: u32, duration: u64) -> u64 {
    if timescale == 0 {
        return 0;
    }
    u64::try_from(u128::from(duration) * 1_000 / u128::from(timescale)).unwrap_or(u64::MAX)
}

/// What a video track's boxes state.
struct VideoTrack {
    id: u32,
    codec: VideoCodec,
    width: u32,
    height: u32,
    /// The media duration in milliseconds, which counts a B-frame reorder delay.
    media_ms: u64,
    /// The edit list's presentation duration, in the movie's timescale.
    edited: Option<u64>,
    frames: u64,
}

/// The presentation duration an `elst` box states, in the movie's timescale.
fn edit_duration(elst: &[u8]) -> Result<u64, String> {
    let version = *elst.first().ok_or("an empty `elst`")?;
    let count = be32(elst, 4)? as usize;
    let entry = if version == 1 { 20 } else { 12 };
    (0..count.min(1_024)).try_fold(0u64, |total, i| {
        let at = 8 + i * entry;
        let duration = if version == 1 { be64(elst, at)? } else { u64::from(be32(elst, at)?) };
        Ok(total.saturating_add(duration))
    })
}

fn video_track(trak: &[u8]) -> Result<Option<VideoTrack>, String> {
    let trak = atoms(trak)?;
    let Some(mdia) = child(&trak, b"mdia") else { return Ok(None) };
    let mdia = atoms(mdia.body)?;
    let handler = child(&mdia, b"hdlr").map(|h| h.body.get(8..12).unwrap_or_default());
    if handler != Some(b"vide".as_slice()) {
        return Ok(None);
    }
    let id = child(&trak, b"tkhd")
        .map(|t| if t.body.first() == Some(&1) { be32(t.body, 20) } else { be32(t.body, 12) })
        .transpose()?
        .unwrap_or(0);
    let (scale, duration) =
        child(&mdia, b"mdhd").map(|m| timing(m.body)).transpose()?.unwrap_or((0, 0));
    let minf = atoms(child(&mdia, b"minf").ok_or("a video track has no `minf`")?.body)?;
    let stbl = atoms(child(&minf, b"stbl").ok_or("a video track has no `stbl`")?.body)?;
    let stsd = child(&stbl, b"stsd").ok_or("a video track has no `stsd`")?.body;
    let entry = stsd.get(8..).ok_or("an empty `stsd`")?;
    let format: [u8; 4] =
        entry.get(4..8).and_then(|f| f.try_into().ok()).ok_or("an empty `stsd`")?;
    let codec = match &format {
        b"avc1" | b"avc3" => VideoCodec::H264,
        b"hvc1" | b"hev1" => VideoCodec::H265,
        b"mp4v" => VideoCodec::Mpeg4,
        other => {
            return Err(format!(
                "the video is `{}`; only H.264, H.265 and MPEG-4 are accepted (VP9, AV1 and \
                 ProRes are not) — re-encode with `ffmpeg -i in -c:v libx264 out.mp4`",
                String::from_utf8_lossy(other).trim()
            ));
        }
    };
    let width = entry.get(32..34).map_or(0, |b| u32::from(u16::from_be_bytes([b[0], b[1]])));
    let height = entry.get(34..36).map_or(0, |b| u32::from(u16::from_be_bytes([b[0], b[1]])));
    let frames = child(&stbl, b"stsz").map(|s| be32(s.body, 8)).transpose()?.unwrap_or(0);
    let edited = match child(&trak, b"edts") {
        Some(edts) => atoms(edts.body)?
            .iter()
            .find(|a| &a.kind == b"elst")
            .map(|elst| edit_duration(elst.body))
            .transpose()?,
        None => None,
    };
    Ok(Some(VideoTrack {
        id,
        codec,
        width,
        height,
        media_ms: millis(scale, duration),
        edited: edited.filter(|d| *d > 0),
        frames: u64::from(frames),
    }))
}

/// Frames of `track` in the fragments (`moof` boxes) of a fragmented file.
fn fragment_frames(top: &[Atom<'_>], track: u32) -> Result<u64, String> {
    let mut frames = 0u64;
    for moof in top.iter().filter(|a| &a.kind == b"moof") {
        for traf in atoms(moof.body)?.iter().filter(|a| &a.kind == b"traf") {
            let traf = atoms(traf.body)?;
            let id = child(&traf, b"tfhd").map(|t| be32(t.body, 4)).transpose()?;
            if id.is_some_and(|id| id != track && track != 0) {
                continue;
            }
            for trun in traf.iter().filter(|a| &a.kind == b"trun") {
                frames += u64::from(be32(trun.body, 4)?);
            }
        }
    }
    Ok(frames)
}

/// Reads the container of `bytes`.
///
/// # Errors
///
/// A message naming what is missing or not accepted: a file that is not MP4 or MOV, has no
/// video track, carries another codec, or states no duration.
pub(crate) fn probe(bytes: &[u8]) -> Result<Probe, String> {
    let top = atoms(bytes).map_err(|e| format!("not an MP4 or MOV file: {e}"))?;
    let brand = child(&top, b"ftyp").and_then(|f| f.body.get(0..4));
    let mime_type = match brand {
        Some(b"qt  ") => "video/quicktime",
        Some(_) => "video/mp4",
        // QuickTime files may start straight with `moov`, `mdat` or `wide`.
        None if top.iter().any(|a| &a.kind == b"moov") => "video/quicktime",
        None => return Err("not an MP4 or MOV file: no `ftyp` or `moov` box".into()),
    };
    let moov = child(&top, b"moov").ok_or("the file has no `moov` box")?;
    let moov = atoms(moov.body)?;
    let mut track = None;
    for trak in moov.iter().filter(|a| &a.kind == b"trak") {
        if let Some(found) = video_track(trak.body)? {
            track = Some(found);
            break;
        }
    }
    let VideoTrack { id, codec, width, height, media_ms, edited, mut frames } =
        track.ok_or("the file has no video track")?;
    let movie = child(&moov, b"mvhd").map(|m| timing(m.body)).transpose()?;
    // The edit list states what plays; the media duration also counts the B-frame delay
    // the edit list trims.
    let mut duration_ms = match (edited, movie) {
        (Some(edited), Some((scale, _))) => millis(scale, edited),
        _ => media_ms,
    };
    if duration_ms == 0 {
        duration_ms = movie.map_or(0, |(scale, d)| millis(scale, d));
    }
    if frames == 0 {
        frames = fragment_frames(&top, id)?;
    }
    if duration_ms == 0 {
        let mehd = child(&moov, b"mvex")
            .map(|m| atoms(m.body))
            .transpose()?
            .and_then(|mvex| {
                child(&mvex, b"mehd").map(|m| {
                    if m.body.first() == Some(&1) {
                        be64(m.body, 4)
                    } else {
                        be32(m.body, 4).map(u64::from)
                    }
                })
            })
            .transpose()?;
        let scale = child(&moov, b"mvhd").map(|m| timing(m.body)).transpose()?.map_or(0, |t| t.0);
        duration_ms = mehd.map_or(0, |d| millis(scale, d));
    }
    if duration_ms == 0 || frames == 0 || width == 0 || height == 0 {
        return Err(format!(
            "the video track states {frames} frames, {duration_ms} ms and {width}x{height}; \
             remux it with `ffmpeg -i in.mp4 -c copy -movflags +faststart out.mp4`"
        ));
    }
    Ok(Probe { mime_type, codec, width, height, duration_ms, frames })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    /// A minimal MP4 header: one video track of `frames` frames at `codec`, `width`×`height`,
    /// `duration_ms` long, with `mdat` holding `payload` bytes.
    pub(crate) fn mp4(
        codec: &[u8; 4],
        width: u16,
        height: u16,
        duration_ms: u32,
        frames: u32,
        payload: usize,
    ) -> Vec<u8> {
        let mut mdhd = vec![0u8; 4];
        mdhd.extend_from_slice(&[0; 8]);
        mdhd.extend_from_slice(&1_000u32.to_be_bytes());
        mdhd.extend_from_slice(&duration_ms.to_be_bytes());
        mdhd.extend_from_slice(&[0; 4]);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(b"vide");
        hdlr.extend_from_slice(&[0; 12]);
        let mut entry = vec![0u8; 24];
        entry.extend_from_slice(&width.to_be_bytes());
        entry.extend_from_slice(&height.to_be_bytes());
        entry.extend_from_slice(&[0; 50]);
        let mut stsd = vec![0u8, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(atom(codec, &entry));
        let mut stsz = vec![0u8; 8];
        stsz.extend_from_slice(&frames.to_be_bytes());
        let stbl = [atom(b"stsd", &stsd), atom(b"stsz", &stsz)].concat();
        let minf = atom(b"stbl", &stbl);
        let mdia = [atom(b"mdhd", &mdhd), atom(b"hdlr", &hdlr), atom(b"minf", &minf)].concat();
        let mut tkhd = vec![0u8; 12];
        tkhd.extend_from_slice(&1u32.to_be_bytes());
        tkhd.extend_from_slice(&[0; 64]);
        let trak = [atom(b"tkhd", &tkhd), atom(b"mdia", &mdia)].concat();
        let moov = atom(b"trak", &trak);
        [
            atom(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41"),
            atom(b"moov", &moov),
            atom(b"mdat", &vec![0u8; payload]),
        ]
        .concat()
    }

    #[test]
    fn a_video_track_is_read_from_its_boxes() {
        let probe = probe(&mp4(b"avc1", 640, 360, 20_000, 600, 32)).unwrap();
        assert_eq!(
            probe,
            Probe {
                mime_type: "video/mp4",
                codec: VideoCodec::H264,
                width: 640,
                height: 360,
                duration_ms: 20_000,
                frames: 600,
            }
        );
    }

    #[test]
    fn other_codecs_and_files_are_refused() {
        let vp9 = probe(&mp4(b"vp09", 640, 360, 1_000, 30, 0)).unwrap_err();
        assert!(vp9.contains("vp09"), "{vp9}");
        assert!(probe(b"\x89PNG\r\n\x1a\n").is_err());
        assert!(probe(&atom(b"ftyp", b"isom")).unwrap_err().contains("moov"));
        let mut cut = mp4(b"avc1", 640, 360, 1_000, 30, 0);
        cut.truncate(cut.len() - 20);
        assert!(probe(&cut).is_err());
        assert!(probe(&mp4(b"hvc1", 640, 360, 0, 30, 0)).unwrap_err().contains("faststart"));
    }
}
