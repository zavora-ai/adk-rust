//! ITU-T G.711 companding (A-law and μ-law) codecs.
//!
//! Exposes SOTA spec-exact arithmetic implementations of A-law and μ-law
//! encoding and decoding. Designed to be lightweight and zero-allocation.

/// Decodes a single 8-bit u-law sample to 16-bit PCM.
///
/// Matches the ITU-T G.711 standard.
pub fn decode_ulaw(ulaw: u8) -> i16 {
    let mu = !ulaw;
    let sign = (mu & 0x80) != 0;
    let exponent = (mu >> 4) & 0x07;
    let mantissa = mu & 0x0F;
    let mut sample = (((mantissa as i16) << 3) + 0x84) << exponent;
    sample -= 0x84;

    if sign { -sample } else { sample }
}

/// Encodes a single 16-bit PCM sample to 8-bit u-law.
///
/// Matches the ITU-T G.711 standard.
pub fn encode_ulaw(pcm: i16) -> u8 {
    let mut pcm = pcm as i32;
    let sign = if pcm < 0 {
        pcm = -pcm;
        0x80
    } else {
        0x00
    };

    if pcm > 32635 {
        pcm = 32635;
    }
    pcm += 0x84;

    let exponent = if pcm > 0x4000 {
        7
    } else if pcm > 0x2000 {
        6
    } else if pcm > 0x1000 {
        5
    } else if pcm > 0x0800 {
        4
    } else if pcm > 0x0400 {
        3
    } else if pcm > 0x0200 {
        2
    } else if pcm > 0x0100 {
        1
    } else {
        0
    };

    let mantissa = (pcm >> (exponent + 3)) & 0x0f;
    !(sign | (exponent << 4) | mantissa as u8)
}

/// Decodes a single 8-bit A-law sample to 16-bit PCM.
///
/// Matches the ITU-T G.711 standard.
pub fn decode_alaw(alaw: u8) -> i16 {
    let a = alaw ^ 0x55;
    let sign = (a & 0x80) != 0;
    let exponent = (a >> 4) & 0x07;
    let mantissa = a & 0x0F;

    let sample = if exponent == 0 {
        ((mantissa as i16) << 4) + 0x08
    } else {
        (((mantissa as i16) << 4) + 0x108) << (exponent - 1)
    };

    if sign { sample } else { -sample }
}

/// Encodes a single 16-bit PCM sample to 8-bit A-law.
///
/// Matches the ITU-T G.711 standard.
pub fn encode_alaw(pcm: i16) -> u8 {
    let mut pcm = pcm as i32;
    let sign = if pcm >= 0 {
        0x80
    } else {
        pcm = -pcm;
        0x00
    };

    if pcm > 31743 {
        pcm = 31743;
    }

    let (exponent, mantissa) = if pcm >= 0x0100 {
        let mut exponent = 7;
        let mut mask = 0x4000;
        while (pcm & mask) == 0 && exponent > 1 {
            exponent -= 1;
            mask >>= 1;
        }
        let mantissa = (pcm >> (exponent + 3)) & 0x0f;
        (exponent, mantissa)
    } else {
        (0, pcm >> 4)
    };

    (sign | (exponent << 4) | mantissa as u8) ^ 0x55
}

/// Decodes a frame of u-law bytes into PCM16 samples.
pub fn decode_ulaw_frame(payload: &[u8]) -> Vec<i16> {
    payload.iter().copied().map(decode_ulaw).collect()
}

/// Encodes a frame of PCM16 samples into u-law bytes.
pub fn encode_ulaw_frame(samples: &[i16]) -> Vec<u8> {
    samples.iter().copied().map(encode_ulaw).collect()
}

/// Decodes a frame of A-law bytes into PCM16 samples.
pub fn decode_alaw_frame(payload: &[u8]) -> Vec<i16> {
    payload.iter().copied().map(decode_alaw).collect()
}

/// Encodes a frame of PCM16 samples into A-law bytes.
pub fn encode_alaw_frame(samples: &[i16]) -> Vec<u8> {
    samples.iter().copied().map(encode_alaw).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ulaw_canonical_reference() {
        assert_eq!(decode_ulaw(0x00), -32124);
        assert_eq!(decode_ulaw(0xFF), 0);
        assert_eq!(decode_ulaw(0x7F), 0);
        assert_eq!(encode_ulaw(-32124), 0x00);
        assert_eq!(encode_ulaw(0), 0xFF);
        assert_eq!(encode_ulaw(32124), 0x80);
    }

    #[test]
    fn test_alaw_canonical_reference() {
        assert_eq!(decode_alaw(0x00), -5504);
        assert_eq!(decode_alaw(0xD5), 8);
        assert_eq!(decode_alaw(0x55), -8);
        assert_eq!(encode_alaw(-5504), 0x00);
        assert_eq!(encode_alaw(8), 0xD5);
        assert_eq!(encode_alaw(0), 0xD5);
    }

    #[test]
    fn test_ulaw_roundtrip() {
        let samples = vec![0, 100, -100, 1000, -1000, 5000, -5000, 15000, -15000];
        for &sample in &samples {
            let encoded = encode_ulaw(sample);
            let decoded = decode_ulaw(encoded);
            let diff = (sample as i32 - decoded as i32).abs();
            assert!(diff < 1000, "Large diff {} for sample {}", diff, sample);
        }
    }

    #[test]
    fn test_alaw_roundtrip() {
        let samples = vec![0, 100, -100, 1000, -1000, 5000, -5000, 15000, -15000];
        for &sample in &samples {
            let encoded = encode_alaw(sample);
            let decoded = decode_alaw(encoded);
            let diff = (sample as i32 - decoded as i32).abs();
            assert!(diff < 1000, "Large diff {} for sample {}", diff, sample);
        }
    }
}
