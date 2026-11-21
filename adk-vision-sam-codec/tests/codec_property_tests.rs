//! Round trips and refusals over arbitrary rasters and payloads.

use adk_vision_sam_codec::{EncodedMask, Encoding, decode_mask_to_raster, encode_one_bit};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Every binary raster survives a one_bit round trip.
    #[test]
    fn one_bit_round_trips(
        (width, height, raster) in (1u32..48, 1u32..48).prop_flat_map(|(w, h)| {
            (Just(w), Just(h), proptest::collection::vec(0u8..=1, (w * h) as usize))
        })
    ) {
        let payload = encode_one_bit(&raster, width, height).unwrap();
        let mask = EncodedMask { encoding: Encoding::OneBit, width, height, payload: &payload };
        prop_assert_eq!(decode_mask_to_raster(&mask, raster.len()).unwrap(), raster);
    }

    /// Arbitrary text never panics the decoder, and anything it accepts is a binary raster
    /// of the declared size.
    #[test]
    fn arbitrary_payloads_are_refused_or_binary(
        lossless in any::<bool>(),
        width in 1u32..64,
        height in 1u32..64,
        body in "[!-~]{0,96}",
    ) {
        let encoding = if lossless { Encoding::Lossless } else { Encoding::OneBit };
        let payload = format!("{}{body}", encoding.marker() as char);
        let mask = EncodedMask { encoding, width, height, payload: &payload };
        if let Ok(raster) = decode_mask_to_raster(&mask, 4096) {
            prop_assert_eq!(raster.len(), (width * height) as usize);
            prop_assert!(raster.iter().all(|b| *b <= 1));
        }
    }

    /// A one_bit payload with one character changed is refused, or is itself the canonical
    /// encoding of the different raster it decodes to: nothing non-canonical gets through.
    #[test]
    fn a_changed_one_bit_payload_is_refused_or_canonical(
        (width, height, raster) in (2u32..24, 2u32..24).prop_flat_map(|(w, h)| {
            (Just(w), Just(h), proptest::collection::vec(0u8..=1, (w * h) as usize))
        }),
        at in any::<prop::sample::Index>(),
        to in b'!'..=b'{',
    ) {
        let payload = encode_one_bit(&raster, width, height).unwrap();
        let mut bytes = payload.into_bytes();
        let at = 1 + at.index(bytes.len() - 1);
        prop_assume!(bytes[at] != to);
        bytes[at] = to;
        let changed = String::from_utf8(bytes).unwrap();
        let mask = EncodedMask { encoding: Encoding::OneBit, width, height, payload: &changed };
        if let Ok(decoded) = decode_mask_to_raster(&mask, raster.len()) {
            prop_assert_ne!(&decoded, &raster);
            prop_assert_eq!(encode_one_bit(&decoded, width, height).unwrap(), changed);
        }
    }
}
