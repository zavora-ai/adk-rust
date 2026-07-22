use adk_realtime::audio::{AudioChunk, AudioFormat};
use bytes::Bytes;

pub const FANOUT: usize = 4;

#[derive(Clone, Copy, Debug)]
pub enum FrameKind {
    G711Ulaw,
    Pcm16_16Khz,
    Pcm16_24Khz,
}

#[derive(Clone, Copy, Debug)]
pub struct FrameCase {
    pub name: &'static str,
    pub bytes: usize,
    kind: FrameKind,
}

impl FrameCase {
    pub fn format(self) -> AudioFormat {
        match self.kind {
            FrameKind::G711Ulaw => AudioFormat::g711_ulaw(),
            FrameKind::Pcm16_16Khz => AudioFormat::pcm16_16khz(),
            FrameKind::Pcm16_24Khz => AudioFormat::pcm16_24khz(),
        }
    }

    pub fn payload(self) -> Vec<u8> {
        (0..self.bytes).map(|index| (index % 251) as u8).collect()
    }
}

pub const FRAME_CASES: [FrameCase; 3] = [
    FrameCase { name: "g711_ulaw_8khz_20ms", bytes: 160, kind: FrameKind::G711Ulaw },
    FrameCase { name: "pcm16_16khz_20ms", bytes: 640, kind: FrameKind::Pcm16_16Khz },
    FrameCase { name: "pcm16_24khz_20ms", bytes: 960, kind: FrameKind::Pcm16_24Khz },
];

pub trait BenchChunk: Clone + Send + 'static {
    const LABEL: &'static str;

    fn from_owned(data: Vec<u8>, format: AudioFormat) -> Self;
    fn from_borrowed(data: &[u8], format: AudioFormat) -> Self;
    fn data(&self) -> &[u8];
    fn format(&self) -> &AudioFormat;
}

#[derive(Clone, Debug)]
pub struct VecChunk {
    data: Vec<u8>,
    format: AudioFormat,
}

impl BenchChunk for VecChunk {
    const LABEL: &'static str = "vec";

    fn from_owned(data: Vec<u8>, format: AudioFormat) -> Self {
        Self { data, format }
    }

    fn from_borrowed(data: &[u8], format: AudioFormat) -> Self {
        Self { data: data.to_vec(), format }
    }

    fn data(&self) -> &[u8] {
        &self.data
    }

    fn format(&self) -> &AudioFormat {
        &self.format
    }
}

#[derive(Clone, Debug)]
pub struct BytesChunk {
    data: Bytes,
    format: AudioFormat,
}

impl BenchChunk for BytesChunk {
    const LABEL: &'static str = "bytes";

    fn from_owned(data: Vec<u8>, format: AudioFormat) -> Self {
        Self { data: Bytes::from(data), format }
    }

    fn from_borrowed(data: &[u8], format: AudioFormat) -> Self {
        Self { data: Bytes::copy_from_slice(data), format }
    }

    fn data(&self) -> &[u8] {
        &self.data
    }

    fn format(&self) -> &AudioFormat {
        &self.format
    }
}

pub fn validate_representations(frame: FrameCase) {
    let payload = frame.payload();
    let format = frame.format();
    let production = AudioChunk::new(payload.clone(), format.clone());
    let vec_chunk = VecChunk::from_owned(payload.clone(), format.clone());
    let bytes_chunk = BytesChunk::from_owned(payload.clone(), format.clone());

    assert_eq!(production.data, payload);
    assert_eq!(vec_chunk.data(), bytes_chunk.data());
    assert_eq!(vec_chunk.data(), &production.data[..]);
    assert_eq!(vec_chunk.format(), bytes_chunk.format());
    assert_eq!(vec_chunk.format(), &production.format);

    let borrowed_vec = VecChunk::from_borrowed(&payload, format.clone());
    let borrowed_bytes = BytesChunk::from_borrowed(&payload, format);
    assert_eq!(borrowed_vec.data(), borrowed_bytes.data());
    assert_eq!(borrowed_vec.data(), payload.as_slice());
}
