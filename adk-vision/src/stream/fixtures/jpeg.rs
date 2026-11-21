//! A baseline JPEG encoder with 4:2:0 chroma and the standard Huffman tables: the form
//! RFC 2435 carries over RTP (type 1), which the `image` crate's 4:4:4 encoder cannot make.
//!
//! The quantisation and Huffman tables are those of ITU-T T.81 Annex K, as the `image` crate
//! also states them; the quality scaling is the IJG formula RFC 2435 Appendix A uses.

use crate::media::DecodedImage;

/// The coefficient at each zig-zag position, in natural (row-major) order.
#[rustfmt::skip]
const ZIGZAG: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10, 17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Table K.1, natural order.
#[rustfmt::skip]
const LUMA_Q: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55,
    14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62,
    18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92,
    49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// Table K.2, natural order.
#[rustfmt::skip]
const CHROMA_Q: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99,
    24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

const LUMA_DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const CHROMA_DC_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const LUMA_AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
const CHROMA_AC_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];

#[rustfmt::skip]
const LUMA_AC_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

#[rustfmt::skip]
const CHROMA_AC_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// A symbol's code and its length in bits.
type Codes = [(u16, u8); 256];

/// Annex C: codes from code-length counts.
fn codes(bits: &[u8; 16], values: &[u8]) -> Codes {
    let mut table = [(0u16, 0u8); 256];
    let mut code = 0u16;
    let mut k = 0;
    for (len, count) in bits.iter().enumerate() {
        for _ in 0..*count {
            table[values[k] as usize] = (code, len as u8 + 1);
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    table
}

/// A frame encoded for RFC 2435 type 1: the two quantisation tables and the entropy-coded
/// scan, with no JPEG headers.
pub(crate) struct Baseline420 {
    pub(crate) width: u16,
    pub(crate) height: u16,
    /// Luma then chroma table, each in zig-zag order, as RTP/JPEG and DQT carry them.
    pub(crate) tables: [[u8; 64]; 2],
    /// The entropy-coded data, byte-stuffed, without EOI.
    pub(crate) scan: Vec<u8>,
}

struct Bits {
    out: Vec<u8>,
    acc: u32,
    count: u32,
}

impl Bits {
    fn put(&mut self, code: u16, len: u8) {
        self.acc = (self.acc << len) | u32::from(code);
        self.count += u32::from(len);
        while self.count >= 8 {
            let byte = (self.acc >> (self.count - 8)) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0);
            }
            self.count -= 8;
            self.acc &= (1 << self.count) - 1;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            let pad = 8 - self.count as u8;
            self.put((1 << pad) - 1, pad);
        }
        self.out
    }
}

/// The magnitude category of a coefficient and its value bits (Annex F.1.2).
fn category(value: i32) -> (u8, u16) {
    let magnitude = value.unsigned_abs();
    let size = (32 - magnitude.leading_zeros()) as u8;
    let bits = if value < 0 { (value - 1) as u32 & ((1 << size) - 1) } else { value as u32 };
    (size, bits as u16)
}

struct Tables {
    dc: Codes,
    ac: Codes,
    quant: [f32; 64],
}

fn scaled(base: &[u16; 64], quality: u8) -> [u8; 64] {
    let quality = u32::from(quality.clamp(1, 100));
    let scale = if quality < 50 { 5000 / quality } else { 200 - quality * 2 };
    let mut natural = [0u8; 64];
    for (out, b) in natural.iter_mut().zip(base) {
        *out = ((u32::from(*b) * scale + 50) / 100).clamp(1, 255) as u8;
    }
    natural
}

/// The forward DCT of one block, scaled as T.81 A.3.3.
fn fdct(block: &[f32; 64], cos: &[[f32; 8]; 8]) -> [f32; 64] {
    let mut rows = [0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            let mut sum = 0.0;
            for x in 0..8 {
                sum += block[y * 8 + x] * cos[x][u];
            }
            rows[y * 8 + u] = sum;
        }
    }
    let mut out = [0f32; 64];
    for u in 0..8 {
        for v in 0..8 {
            let mut sum = 0.0;
            for y in 0..8 {
                sum += rows[y * 8 + u] * cos[y][v];
            }
            let cu = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
            let cv = if v == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
            out[v * 8 + u] = 0.25 * cu * cv * sum;
        }
    }
    out
}

fn encode_block(
    block: &[f32; 64],
    tables: &Tables,
    cos: &[[f32; 8]; 8],
    dc_pred: &mut i32,
    bits: &mut Bits,
) {
    let coefficients = fdct(block, cos);
    let mut q = [0i32; 64];
    for (zz, natural) in ZIGZAG.iter().enumerate() {
        q[zz] = (coefficients[*natural] / tables.quant[*natural]).round() as i32;
    }
    let diff = q[0] - *dc_pred;
    *dc_pred = q[0];
    let (size, value) = category(diff);
    let (code, len) = tables.dc[size as usize];
    bits.put(code, len);
    if size > 0 {
        bits.put(value, size);
    }
    let mut run = 0u8;
    for &coefficient in &q[1..] {
        if coefficient == 0 {
            run += 1;
            continue;
        }
        while run >= 16 {
            let (code, len) = tables.ac[0xF0];
            bits.put(code, len);
            run -= 16;
        }
        let (size, value) = category(coefficient);
        let (code, len) = tables.ac[((run << 4) | size) as usize];
        bits.put(code, len);
        bits.put(value, size);
        run = 0;
    }
    if run > 0 {
        let (code, len) = tables.ac[0x00];
        bits.put(code, len);
    }
}

/// Encodes `image` as baseline 4:2:0 at `quality` (1–100).
pub(crate) fn encode(image: &DecodedImage, quality: u8) -> Baseline420 {
    let (w, h) = image.size();
    let rgba = image.rgba();
    let luma_natural = scaled(&LUMA_Q, quality);
    let chroma_natural = scaled(&CHROMA_Q, quality);
    let zigzag = |natural: &[u8; 64]| {
        let mut out = [0u8; 64];
        for (zz, n) in ZIGZAG.iter().enumerate() {
            out[zz] = natural[*n];
        }
        out
    };
    let quant = |natural: &[u8; 64]| natural.map(f32::from);
    let luma = Tables {
        dc: codes(&LUMA_DC_BITS, &DC_VALUES),
        ac: codes(&LUMA_AC_BITS, &LUMA_AC_VALUES),
        quant: quant(&luma_natural),
    };
    let chroma = Tables {
        dc: codes(&CHROMA_DC_BITS, &DC_VALUES),
        ac: codes(&CHROMA_AC_BITS, &CHROMA_AC_VALUES),
        quant: quant(&chroma_natural),
    };
    let mut cos = [[0f32; 8]; 8];
    for (x, row) in cos.iter_mut().enumerate() {
        for (u, c) in row.iter_mut().enumerate() {
            *c = (((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI) / 16.0).cos();
        }
    }
    // Y, Cb and Cr at a source pixel, edges replicated, level-shifted by 128.
    let ycc = |x: u32, y: u32| {
        let i = ((y.min(h - 1) * w + x.min(w - 1)) * 4) as usize;
        let (r, g, b) = (f32::from(rgba[i]), f32::from(rgba[i + 1]), f32::from(rgba[i + 2]));
        (
            0.299 * r + 0.587 * g + 0.114 * b - 128.0,
            -0.168_736 * r - 0.331_264 * g + 0.5 * b,
            0.5 * r - 0.418_688 * g - 0.081_312 * b,
        )
    };
    let mut bits = Bits { out: Vec::new(), acc: 0, count: 0 };
    let mut preds = [0i32; 3];
    for my in 0..h.div_ceil(16) {
        for mx in 0..w.div_ceil(16) {
            let mut cb = [0f32; 64];
            let mut cr = [0f32; 64];
            for by in 0..2 {
                for bx in 0..2 {
                    let mut y_block = [0f32; 64];
                    for j in 0..8 {
                        for i in 0..8 {
                            let (px, py) = (mx * 16 + bx * 8 + i, my * 16 + by * 8 + j);
                            let (y, b, r) = ycc(px, py);
                            y_block[(j * 8 + i) as usize] = y;
                            let c = (((by * 8 + j) / 2) * 8 + (bx * 8 + i) / 2) as usize;
                            cb[c] += b / 4.0;
                            cr[c] += r / 4.0;
                        }
                    }
                    encode_block(&y_block, &luma, &cos, &mut preds[0], &mut bits);
                }
            }
            encode_block(&cb, &chroma, &cos, &mut preds[1], &mut bits);
            encode_block(&cr, &chroma, &cos, &mut preds[2], &mut bits);
        }
    }
    Baseline420 {
        width: u16::try_from(w).unwrap_or(u16::MAX),
        height: u16::try_from(h).unwrap_or(u16::MAX),
        tables: [zigzag(&luma_natural), zigzag(&chroma_natural)],
        scan: bits.finish(),
    }
}

impl Baseline420 {
    /// The frame as a complete JFIF-less JPEG file: what an RTP/JPEG receiver rebuilds.
    pub(crate) fn to_jpeg(&self) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        for (id, table) in self.tables.iter().enumerate() {
            out.extend_from_slice(&[0xFF, 0xDB, 0, 67, id as u8]);
            out.extend_from_slice(table);
        }
        let [hh, hl] = self.height.to_be_bytes();
        let [wh, wl] = self.width.to_be_bytes();
        out.extend_from_slice(&[
            0xFF, 0xC0, 0, 17, 8, hh, hl, wh, wl, 3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1,
        ]);
        let tables: [(u8, &[u8; 16], &[u8]); 4] = [
            (0x00, &LUMA_DC_BITS, &DC_VALUES),
            (0x10, &LUMA_AC_BITS, &LUMA_AC_VALUES),
            (0x01, &CHROMA_DC_BITS, &DC_VALUES),
            (0x11, &CHROMA_AC_BITS, &CHROMA_AC_VALUES),
        ];
        for (class, bits, values) in tables {
            let len = (3 + 16 + values.len()) as u16;
            out.extend_from_slice(&[0xFF, 0xC4]);
            out.extend_from_slice(&len.to_be_bytes());
            out.push(class);
            out.extend_from_slice(bits);
            out.extend_from_slice(values);
        }
        out.extend_from_slice(&[0xFF, 0xDA, 0, 12, 3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);
        out.extend_from_slice(&self.scan);
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::DecodeLimits;

    #[test]
    fn the_encoding_decodes_close_to_the_source() {
        let mut rgba = Vec::new();
        for y in 0..48u32 {
            for x in 0..40u32 {
                rgba.extend_from_slice(&[(x * 6) as u8, (y * 5) as u8, 128, 255]);
            }
        }
        let source = DecodedImage::from_rgba(40, 48, rgba).unwrap();
        let jpeg = encode(&source, 90).to_jpeg();
        let (decoded, mime) = DecodedImage::decode(&jpeg, &DecodeLimits::default()).unwrap();
        assert_eq!(mime, "image/jpeg");
        assert_eq!(decoded.size(), (40, 48));
        let error: f64 = source
            .rgba()
            .iter()
            .zip(decoded.rgba())
            .map(|(a, b)| f64::from((i16::from(*a) - i16::from(*b)).unsigned_abs()))
            .sum::<f64>()
            / source.rgba().len() as f64;
        assert!(error < 4.0, "mean absolute error {error}");
    }

    #[test]
    fn categories_follow_annex_f() {
        assert_eq!(category(0), (0, 0));
        assert_eq!(category(1), (1, 1));
        assert_eq!(category(-1), (1, 0));
        assert_eq!(category(-3), (2, 0));
        assert_eq!(category(5), (3, 5));
    }
}
