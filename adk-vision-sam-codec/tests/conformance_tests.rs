// Copyright (c) Meta Platforms, Inc. and affiliates. All Rights Reserved.
//
// Ports the mask-codec cases of Meta's `python/tests/test_mask_codec.py` and
// `test_conformance_codec.py` (github.com/meta-models/meta-sam at 5f2e6e7f), by the
// ADK-Rust contributors. Distributed under the SAM License; see LICENSE and NOTICE.

//! Meta's shared conformance mask vectors and the codec's refusals.

use adk_vision_sam_codec::{EncodedMask, Encoding, decode_mask_to_raster, encode_one_bit};
use serde_json::Value;

struct Vector {
    case: String,
    encoding: Encoding,
    width: u32,
    height: u32,
    payload: String,
    raster: Vec<u8>,
}

impl Vector {
    fn mask(&self) -> EncodedMask<'_> {
        EncodedMask {
            encoding: self.encoding,
            width: self.width,
            height: self.height,
            payload: &self.payload,
        }
    }
}

fn vectors() -> Vec<Vector> {
    let text = include_str!("fixtures/meta-conformance-masks.json");
    let doc: Value = serde_json::from_str(text).unwrap();
    doc["masks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| Vector {
            case: m["case"].as_str().unwrap().to_string(),
            encoding: match m["encoding"].as_str().unwrap() {
                "one_bit" => Encoding::OneBit,
                "lossless" => Encoding::Lossless,
                other => panic!("unknown encoding {other}"),
            },
            width: m["width"].as_u64().unwrap() as u32,
            height: m["height"].as_u64().unwrap() as u32,
            payload: m["payload"].as_str().unwrap().to_string(),
            raster: m["raster"].as_str().unwrap().bytes().map(|b| b - b'0').collect(),
        })
        .collect()
}

fn vector(encoding: Encoding, case: &str) -> Vector {
    vectors().into_iter().find(|v| v.encoding == encoding && v.case == case).unwrap()
}

fn reason(mask: EncodedMask<'_>) -> &'static str {
    decode_mask_to_raster(&mask, usize::MAX).unwrap_err().reason()
}

#[test]
fn meta_conformance_vectors_decode_to_their_rasters() {
    let vectors = vectors();
    assert!(vectors.iter().any(|v| v.encoding == Encoding::OneBit));
    assert!(vectors.iter().any(|v| v.encoding == Encoding::Lossless));
    for v in &vectors {
        let area = (v.width * v.height) as usize;
        assert_eq!(decode_mask_to_raster(&v.mask(), area).unwrap(), v.raster, "{}", v.case);
    }
}

#[test]
fn one_bit_vectors_are_the_canonical_encoding_of_their_rasters() {
    for v in vectors().iter().filter(|v| v.encoding == Encoding::OneBit) {
        assert_eq!(encode_one_bit(&v.raster, v.width, v.height).unwrap(), v.payload, "{}", v.case);
    }
}

#[test]
fn lossless_accepts_trailing_bytes_and_alternate_finalization() {
    // Meta's decoder accepts both variants for lossless payloads and returns the same raster.
    let v = vector(Encoding::Lossless, "compact-api-image-lossless");
    let area = (v.width * v.height) as usize;
    let trailing = repack(&v.payload[1..], |bytes| bytes.push(0));
    let alternate = repack(&v.payload[1..], |bytes| {
        let at = bytes.len() - 4;
        bytes[at] = 0;
    });
    for payload in [trailing, alternate] {
        assert_ne!(payload, v.payload);
        let mask = EncodedMask { payload: &payload, ..v.mask() };
        assert_eq!(decode_mask_to_raster(&mask, area).unwrap(), v.raster);
    }
}

#[test]
fn one_bit_refuses_truncated_trailing_and_short_payloads() {
    let v = vector(Encoding::OneBit, "compact-api-confidence");
    let short = format!("!{}", pack(&[0; 3]));
    let truncated = repack(&v.payload[1..], |bytes| {
        bytes.pop();
    });
    let trailing = repack(&v.payload[1..], |bytes| bytes.push(0));
    for payload in [short, truncated.replacen('~', "!", 1), trailing.replacen('~', "!", 1)] {
        let why = reason(EncodedMask { payload: &payload, ..v.mask() });
        assert!(
            [
                "Mask payload is missing its finalization.",
                "Mask payload ended before decoding completed.",
                "Mask payload is not canonical or has invalid finalization.",
            ]
            .contains(&why),
            "{payload}: {why}"
        );
    }
}

#[test]
fn malformed_envelopes_are_refused_with_metas_reasons() {
    let one_bit = vector(Encoding::OneBit, "compact-api-confidence");
    let lossless = vector(Encoding::Lossless, "compact-api-image-lossless");
    let cases: Vec<(EncodedMask<'_>, &str)> = vec![
        (
            EncodedMask { payload: "!", ..one_bit.mask() },
            "Mask payload is missing its length prefix.",
        ),
        (EncodedMask { payload: "", ..one_bit.mask() }, "one_bit mask payloads must start with !."),
        (
            EncodedMask {
                payload: &lossless.payload,
                encoding: Encoding::OneBit,
                ..lossless.mask()
            },
            "one_bit mask payloads must start with !.",
        ),
        (
            EncodedMask {
                payload: &one_bit.payload,
                encoding: Encoding::Lossless,
                ..one_bit.mask()
            },
            "lossless mask payloads must start with ~.",
        ),
        (
            EncodedMask { width: 0, ..one_bit.mask() },
            "Mask dimensions must be positive safe integers.",
        ),
        (
            EncodedMask { width: u32::MAX, height: u32::MAX, ..one_bit.mask() },
            "Mask dimensions produce an unsafe decoded area.",
        ),
    ];
    for (mask, want) in cases {
        assert_eq!(reason(mask), want, "{mask:?}");
    }
    let extra = format!("{}!", one_bit.payload);
    assert_eq!(
        reason(EncodedMask { payload: &extra, ..one_bit.mask() }),
        "Mask payload length does not match its prefix."
    );
    let pipe = format!("{}|", &one_bit.payload[..one_bit.payload.len() - 1]);
    assert_eq!(
        reason(EncodedMask { payload: &pipe, ..one_bit.mask() }),
        "Mask payload contains an invalid character."
    );
    let truncated = format!("~{}", pack(&[0; 4]));
    assert_eq!(
        reason(EncodedMask { payload: &truncated, ..lossless.mask() }),
        "Lossless mask payload is truncated."
    );
    let predictor = repack(&lossless.payload[1..], |bytes| bytes[0] = 5);
    assert_eq!(
        reason(EncodedMask { payload: &predictor, ..lossless.mask() }),
        "Lossless mask payload uses an invalid predictor."
    );
}

#[test]
fn the_area_bound_is_checked_before_decoding() {
    let v = vector(Encoding::OneBit, "compact-api-confidence");
    let area = (v.width * v.height) as usize;
    assert!(decode_mask_to_raster(&v.mask(), area).is_ok());
    assert_eq!(
        decode_mask_to_raster(&v.mask(), area - 1).unwrap_err().reason(),
        "Mask area is larger than the caller accepts."
    );
}

#[test]
fn long_and_tall_rasters_renormalise_and_rescale() {
    // Meta's fixed-width cases: thousands of pixels drive the coder's renormalisation and
    // the context counts' rescaling.
    let cases: [(u32, u32, Vec<u8>); 3] = [
        (5000, 1, (0..5000).map(|i| (i & 1) as u8).collect()),
        (1, 5000, (0..5000).map(|i| ((i / 7) & 1) as u8).collect()),
        (73, 71, (0..5183).map(|i| ((i * 17 + i / 13) & 1) as u8).collect()),
    ];
    for (width, height, raster) in cases {
        let payload = encode_one_bit(&raster, width, height).unwrap();
        let mask = EncodedMask { encoding: Encoding::OneBit, width, height, payload: &payload };
        assert_eq!(decode_mask_to_raster(&mask, raster.len()).unwrap(), raster);
    }
}

/// Base85-packs bytes the way the codec does, for building malformed payloads.
fn pack(bytes: &[u8]) -> String {
    const DIGITS: &[u8] =
        b"!#$%&'()*+-./0123456789:=?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[]^_`abcdefghijklmnopqrstuvwxyz{";
    let mut out = Vec::new();
    let mut length = bytes.len() as u64;
    let mut prefix = [0u8; 5];
    for slot in prefix.iter_mut().rev() {
        *slot = DIGITS[(length % 85) as usize];
        length /= 85;
    }
    out.extend_from_slice(&prefix);
    for chunk in bytes.chunks(4) {
        let mut group = [0u8; 4];
        group[..chunk.len()].copy_from_slice(chunk);
        let mut value = u64::from(u32::from_be_bytes(group));
        let mut digits = [0u8; 5];
        for slot in digits.iter_mut().rev() {
            *slot = DIGITS[(value % 85) as usize];
            value /= 85;
        }
        out.extend_from_slice(&digits[..chunk.len() + 1]);
    }
    String::from_utf8(out).unwrap()
}

/// Unpacks a payload body, edits its bytes and repacks it as a lossless payload.
fn repack(body: &str, edit: impl FnOnce(&mut Vec<u8>)) -> String {
    const DIGITS: &[u8] =
        b"!#$%&'()*+-./0123456789:=?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[]^_`abcdefghijklmnopqrstuvwxyz{";
    let value = |c: u8| DIGITS.iter().position(|d| *d == c).unwrap() as u64;
    let body = body.as_bytes();
    let length = body[..5].iter().fold(0u64, |n, c| n * 85 + value(*c)) as usize;
    let mut bytes = Vec::with_capacity(length);
    for group in body[5..].chunks(5) {
        let mut digits = [84u64; 5];
        for (slot, c) in digits.iter_mut().zip(group) {
            *slot = value(*c);
        }
        let n = digits.iter().fold(0u64, |n, d| n * 85 + d) as u32;
        // A full group carries four bytes; a tail of r + 1 digits carries r.
        bytes.extend_from_slice(&n.to_be_bytes()[..group.len() - 1]);
    }
    assert_eq!(bytes.len(), length);
    edit(&mut bytes);
    format!("~{}", pack(&bytes))
}
