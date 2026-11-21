// Copyright (c) Meta Platforms, Inc. and affiliates. All Rights Reserved.
//
// Rust port of `typescript/packages/parser/src/mask-codec.ts` from @meta-sam/parser 0.1.0
// (github.com/meta-models/meta-sam at 5f2e6e7f1da5b41dcd10bec971201e5910efc18a), by the
// ADK-Rust contributors. Distributed under the SAM License; see LICENSE and NOTICE.

//! Decodes Meta `sam-3.1` mask payloads into rasters.
//!
//! A `sam-3.1` mask token carries `data=H,W,<payload>`: base85 text whose first character
//! selects the encoding (`~` lossless, `!` one_bit) and whose body is arithmetic coded.
//! [`decode_mask_to_raster`] turns one payload into a row-major `H × W` raster of 0 and 1
//! bytes with the same structural checks, the same acceptance and the same output as
//! `decodeMaskToRaster` in Meta's `@meta-sam/parser`.
//!
//! **Licence.** This crate is a port of Meta's code and is distributed under the SAM
//! License, not under ADK-Rust's Apache-2.0. Using or distributing it binds you to that
//! licence, including its use restrictions (clause 1.b.v). `adk-vision` reaches it only
//! through its opt-in `sam-codec` feature, after a person records acceptance.
//!
//! # Example
//!
//! ```
//! use adk_vision_sam_codec::{EncodedMask, Encoding, decode_mask_to_raster, encode_one_bit};
//!
//! // A 3×2 raster with its middle column set.
//! let raster = [0, 1, 0, 0, 1, 0];
//! let payload = encode_one_bit(&raster, 3, 2).unwrap();
//! let mask = EncodedMask { encoding: Encoding::OneBit, width: 3, height: 2, payload: &payload };
//! assert_eq!(decode_mask_to_raster(&mask, 6).unwrap(), raster);
//!
//! // A raster larger than the caller accepts is refused before anything is allocated.
//! assert!(decode_mask_to_raster(&mask, 5).is_err());
//! ```

#![warn(missing_docs)]

use std::collections::HashMap;

/// Printable ASCII `!`..`~` without the seven wire delimiters: 87 characters, of which the
/// first 85 are base85 digits and `}` and `~` are never digits.
const ALPHABET: [u8; 87] = {
    let mut out = [0u8; 87];
    let mut code = 0x21u8;
    let mut index = 0;
    while code <= 0x7e {
        if !matches!(code, b'"' | b'\\' | b',' | b';' | b'<' | b'>' | b'|') {
            out[index] = code;
            index += 1;
        }
        code += 1;
    }
    out
};
const RADIX: u64 = 85;
const PREFIX_LENGTH: usize = 5;
const CHARACTER_VALUES: [i16; 128] = {
    let mut values = [-1i16; 128];
    let mut index = 0;
    while index < ALPHABET.len() {
        values[ALPHABET[index] as usize] = index as i16;
        index += 1;
    }
    values
};

const TOP: u32 = 1 << 24;
const BOTTOM: u32 = 1 << 16;
const MASK32: u64 = 0xffff_ffff;
const INCREMENT: u16 = 14;
const COUNT_LIMIT: u16 = 4096;
const CONTEXT_COUNT: usize = 1 << 12;
const PADDING: usize = 2;
const SYMBOLS: usize = 256;
const TOTAL_INDEX: usize = SYMBOLS;
const SPATIAL_MODES: u8 = 5;
const ZERO_INCREMENT: u32 = 32;
const ZERO_LIMIT: u32 = 16384;
const ORDER_ONE_INCREMENT: u16 = 56;
const ORDER_ONE_LIMIT: i32 = 8192;
const ORDER_TWO_STEP: u16 = 14 * 44;
const ORDER_TWO_LIMIT: u16 = 14 * 3584;
const ORDER_ZERO_STEP: u16 = 2 * 16;
const ORDER_ZERO_LIMIT: u32 = 2 * 2048;
const ORDER_ZERO_INITIAL: u16 = 2;
const ORDER_ONE_INITIAL: u16 = 1;
/// JavaScript's `Number.MAX_SAFE_INTEGER`, the largest area Meta's decoder accepts.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// A mask payload's encoding, from its first character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// `!`: one bit per pixel, arithmetic coded with a 12-neighbour context.
    OneBit,
    /// `~`: the API default, coverage bytes thresholded at 129.
    Lossless,
}

impl Encoding {
    /// The character a payload of this encoding starts with.
    pub fn marker(self) -> u8 {
        match self {
            Self::OneBit => b'!',
            Self::Lossless => b'~',
        }
    }
}

/// One complete mask: its encoding, its raster size and its payload as the API sent it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodedMask<'a> {
    /// The payload's encoding.
    pub encoding: Encoding,
    /// Raster columns.
    pub width: u32,
    /// Raster rows.
    pub height: u32,
    /// The payload, starting with its encoding marker.
    pub payload: &'a str,
}

/// A payload the codec refuses, with Meta's reason.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid sam-3.1 mask payload: {reason} Pass payloads exactly as the API sent them, with their declared width and height"
)]
pub struct InvalidMask {
    reason: &'static str,
}

impl InvalidMask {
    /// Why the payload was refused.
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

fn invalid(reason: &'static str) -> InvalidMask {
    InvalidMask { reason }
}

/// Decodes one complete mask into a row-major `height × width` raster of 0 and 1 bytes.
///
/// The checks are Meta's: a supported encoding marker, positive dimensions whose product
/// is a safe JavaScript integer, a valid base85 alphabet, length prefix, groups and tail
/// in canonical form, sufficient arithmetic-coder finalization and, for `one_bit`, a
/// canonical round trip. A `lossless` payload may carry trailing bytes or alternate unused
/// finalization bytes, as Meta's decoder allows. `max_area` bounds the raster before
/// anything is allocated; Meta's decoder has no such bound.
///
/// # Errors
///
/// [`InvalidMask`] when any check fails or the raster is larger than `max_area`.
///
/// # Example
///
/// ```
/// use adk_vision_sam_codec::{EncodedMask, Encoding, decode_mask_to_raster};
///
/// // A 5×5 one_bit mask from Meta's protocol documentation.
/// let mask = EncodedMask {
///     encoding: Encoding::OneBit,
///     width: 5,
///     height: 5,
///     payload: "!!!!!(QO(0lu8?",
/// };
/// let raster = decode_mask_to_raster(&mask, 25).unwrap();
/// assert_eq!(raster.len(), 25);
/// assert!(raster.iter().all(|&b| b <= 1));
/// ```
pub fn decode_mask_to_raster(
    mask: &EncodedMask<'_>,
    max_area: usize,
) -> Result<Vec<u8>, InvalidMask> {
    if mask.width == 0 || mask.height == 0 {
        return Err(invalid("Mask dimensions must be positive safe integers."));
    }
    let area = u64::from(mask.width) * u64::from(mask.height);
    if area > MAX_SAFE_INTEGER {
        return Err(invalid("Mask dimensions produce an unsafe decoded area."));
    }
    if area > max_area as u64 {
        return Err(invalid("Mask area is larger than the caller accepts."));
    }
    let payload = mask.payload.as_bytes();
    if payload.first() != Some(&mask.encoding.marker()) {
        return Err(invalid(match mask.encoding {
            Encoding::OneBit => "one_bit mask payloads must start with !.",
            Encoding::Lossless => "lossless mask payloads must start with ~.",
        }));
    }
    let (width, height) = (mask.width as usize, mask.height as usize);
    match mask.encoding {
        Encoding::Lossless => {
            let coverage = decode_lossless_raster(&payload[1..], width, height)?;
            Ok(coverage.into_iter().map(|value| u8::from(value >= 129)).collect())
        }
        Encoding::OneBit => {
            let raster = decode_raster(&payload[1..], width, height)?;
            if encode_raster(&raster, width, height)?.as_bytes() != payload {
                return Err(invalid("Mask payload is not canonical or has invalid finalization."));
            }
            Ok(raster)
        }
    }
}

/// Encodes a row-major raster of 0 and 1 bytes as a canonical `one_bit` payload, the
/// form the API sends for `metadata.mask_encoding = "one_bit"`.
///
/// # Errors
///
/// [`InvalidMask`] when a dimension is zero, `raster` is not `width × height` bytes, or a
/// byte is neither 0 nor 1.
///
/// # Example
///
/// ```
/// use adk_vision_sam_codec::encode_one_bit;
///
/// let payload = encode_one_bit(&[1, 0, 0, 1], 2, 2).unwrap();
/// assert!(payload.starts_with('!'));
/// assert!(encode_one_bit(&[2, 0, 0, 1], 2, 2).is_err());
/// ```
pub fn encode_one_bit(raster: &[u8], width: u32, height: u32) -> Result<String, InvalidMask> {
    let area = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || area > MAX_SAFE_INTEGER || area != raster.len() as u64 {
        return Err(invalid("Mask raster dimensions do not match its complete payload."));
    }
    encode_raster(raster, width as usize, height as usize)
}

fn digit(character: u8) -> Result<u64, InvalidMask> {
    let value = CHARACTER_VALUES.get(usize::from(character)).copied().unwrap_or(-1);
    if value < 0 || value as u64 >= RADIX {
        return Err(invalid("Mask payload contains an invalid character."));
    }
    Ok(value as u64)
}

fn pack(input: &[u8]) -> Result<String, InvalidMask> {
    let mut length = input.len() as u64;
    let mut prefix = [0u8; PREFIX_LENGTH];
    for slot in prefix.iter_mut().rev() {
        *slot = ALPHABET[(length % RADIX) as usize];
        length /= RADIX;
    }
    if length != 0 {
        return Err(invalid("Mask payload is too large."));
    }
    let mut output = Vec::with_capacity(PREFIX_LENGTH + input.len().div_ceil(4) * 5);
    output.extend_from_slice(&prefix);
    let mut chunks = input.chunks_exact(4);
    for chunk in &mut chunks {
        let mut value = u64::from(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        let mut digits = [0u8; 5];
        for slot in digits.iter_mut().rev() {
            *slot = ALPHABET[(value % RADIX) as usize];
            value /= RADIX;
        }
        output.extend_from_slice(&digits);
    }
    let remainder = chunks.remainder();
    if !remainder.is_empty() {
        let mut bytes = [0u8; 4];
        bytes[..remainder.len()].copy_from_slice(remainder);
        let mut value = u64::from(u32::from_be_bytes(bytes));
        let mut digits = [0u8; 5];
        for slot in digits.iter_mut().rev() {
            *slot = ALPHABET[(value % RADIX) as usize];
            value /= RADIX;
        }
        output.extend_from_slice(&digits[..=remainder.len()]);
    }
    // Every byte comes from the ASCII alphabet.
    Ok(output.into_iter().map(char::from).collect())
}

fn unpack(payload: &[u8]) -> Result<Vec<u8>, InvalidMask> {
    if payload.len() < PREFIX_LENGTH {
        return Err(invalid("Mask payload is missing its length prefix."));
    }
    let mut length = 0u64;
    for &character in &payload[..PREFIX_LENGTH] {
        length = length * RADIX + digit(character)?;
    }
    let remainder = length % 4;
    let expected =
        PREFIX_LENGTH as u64 + (length / 4) * 5 + if remainder == 0 { 0 } else { remainder + 1 };
    if payload.len() as u64 != expected {
        return Err(invalid("Mask payload length does not match its prefix."));
    }
    // The length now matches the payload, so this allocation is bounded by its size.
    let length = length as usize;
    let remainder = remainder as usize;
    let mut output = vec![0u8; length];
    let mut source = PREFIX_LENGTH;
    let full = length - remainder;
    for destination in (0..full).step_by(4) {
        let mut value = 0u64;
        for &character in &payload[source..source + 5] {
            value = value * RADIX + digit(character)?;
        }
        if value > MASK32 {
            return Err(invalid("Mask payload contains an out-of-range group."));
        }
        source += 5;
        output[destination..destination + 4].copy_from_slice(&(value as u32).to_be_bytes());
    }
    if remainder > 0 {
        let mut value = 0u64;
        for index in 0..5 {
            value = value * RADIX
                + if index < remainder + 1 { digit(payload[source + index])? } else { RADIX - 1 };
        }
        if value > MASK32 {
            return Err(invalid("Mask payload contains an out-of-range tail."));
        }
        output[full..].copy_from_slice(&(value as u32).to_be_bytes()[..remainder]);
    }
    if pack(&output)?.as_bytes() != payload {
        return Err(invalid("Mask payload is not in canonical form."));
    }
    Ok(output)
}

/// Runs the carry-less range coder's normalisation: while the top byte of `low` and of
/// `low + range` agree, or `range` has fallen under `BOTTOM` (then truncated to the next
/// `BOTTOM` boundary), shift one byte out through `shift`.
macro_rules! normalise {
    ($low:expr, $range:expr, $shift:block) => {
        loop {
            if ($low ^ $low.wrapping_add($range)) >= TOP {
                if $range < BOTTOM {
                    $range = $low.wrapping_neg() & (BOTTOM - 1);
                } else {
                    break;
                }
            }
            $shift
            $low <<= 8;
            $range <<= 8;
        }
    };
}

struct RangeEncoder {
    low: u32,
    range: u32,
    output: Vec<u8>,
}

impl RangeEncoder {
    fn new() -> Self {
        Self { low: 0, range: u32::MAX, output: Vec::new() }
    }

    fn encode(&mut self, cumulative: u32, frequency: u32, total: u32) {
        let scaled = self.range / total;
        self.low = self.low.wrapping_add((u64::from(scaled) * u64::from(cumulative)) as u32);
        // `frequency <= total`, so the product fits in the previous range.
        self.range = (u64::from(scaled) * u64::from(frequency)) as u32;
        normalise!(self.low, self.range, {
            self.output.push((self.low >> 24) as u8);
        });
    }

    fn finish(mut self) -> Vec<u8> {
        for _ in 0..4 {
            self.output.push((self.low >> 24) as u8);
            self.low <<= 8;
        }
        self.output
    }
}

struct RangeDecoder<'a> {
    input: &'a [u8],
    position: usize,
    low: u32,
    range: u32,
    code: u32,
    scaled: u32,
}

impl<'a> RangeDecoder<'a> {
    fn new(input: &'a [u8]) -> Result<Self, InvalidMask> {
        if input.len() < 4 {
            return Err(invalid("Mask payload is missing its finalization."));
        }
        let code = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
        Ok(Self { input, position: 4, low: 0, range: u32::MAX, code, scaled: 0 })
    }

    fn frequency(&mut self, total: u32) -> Result<u32, InvalidMask> {
        self.scaled = if total == 0 { 0 } else { self.range / total };
        if self.scaled == 0 {
            return Err(invalid("Mask payload has invalid coding state."));
        }
        let value = self.code.wrapping_sub(self.low) / self.scaled;
        Ok(if value >= total { total - 1 } else { value })
    }

    fn decode(&mut self, cumulative: u32, frequency: u32) -> Result<(), InvalidMask> {
        self.low = self.low.wrapping_add((u64::from(self.scaled) * u64::from(cumulative)) as u32);
        let range = u64::from(self.scaled) * u64::from(frequency);
        if range > MASK32 {
            return Err(invalid("Mask payload has invalid coding state."));
        }
        self.range = range as u32;
        normalise!(self.low, self.range, {
            let byte = *self
                .input
                .get(self.position)
                .ok_or_else(|| invalid("Mask payload ended before decoding completed."))?;
            self.position += 1;
            self.code = (self.code << 8) | u32::from(byte);
        });
        Ok(())
    }
}

fn paeth(left: i32, up: i32, upper_left: i32) -> i32 {
    let estimate = left + up - upper_left;
    let left_distance = (estimate - left).abs();
    let up_distance = (estimate - up).abs();
    let upper_left_distance = (estimate - upper_left).abs();
    if left_distance <= up_distance && left_distance <= upper_left_distance {
        left
    } else if up_distance <= upper_left_distance {
        up
    } else {
        upper_left
    }
}

fn predict_byte(output: &[u8], offset: usize, x: usize, y: usize, mode: u8, width: usize) -> i32 {
    let left = if x > 0 { i32::from(output[offset - 1]) } else { 0 };
    let up = if y > 0 { i32::from(output[offset - width]) } else { 0 };
    match mode {
        1 => up,
        2 => left,
        3 => (left + up) >> 1,
        _ => {
            let upper_left = if x > 0 && y > 0 { i32::from(output[offset - width - 1]) } else { 0 };
            if mode == 0 {
                paeth(left, up, upper_left)
            } else if upper_left >= left.max(up) {
                left.min(up)
            } else if upper_left <= left.min(up) {
                left.max(up)
            } else {
                left + up - upper_left
            }
        }
    }
}

fn unzig(value: u32, prediction: i32) -> u8 {
    let value = value as i32;
    let delta = if value & 1 == 1 { -((value + 1) >> 1) } else { value >> 1 };
    ((prediction + delta) & 0xff) as u8
}

fn zero_context(residuals: &[u8], offset: usize, x: usize, y: usize, width: usize) -> usize {
    let is_zero = |index: usize| usize::from(residuals[index] == 0);
    let left = if x > 0 { is_zero(offset - 1) } else { 1 };
    let up = if y > 0 { is_zero(offset - width) } else { 1 };
    let upper_left = if x > 0 && y > 0 { is_zero(offset - width - 1) } else { 1 };
    let upper_right = if y > 0 && x < width - 1 { is_zero(offset - width + 1) } else { 1 };
    left | (up << 1) | (upper_left << 2) | (upper_right << 3)
}

fn decode_lossless_raster(
    body: &[u8],
    width: usize,
    height: usize,
) -> Result<Vec<u8>, InvalidMask> {
    let packed = unpack(body)?;
    if packed.len() < 5 {
        return Err(invalid("Lossless mask payload is truncated."));
    }
    let selector = packed[0];
    // Meta's decoder refuses an unknown predictor at the first pixel, which every raster has.
    if selector >= SPATIAL_MODES {
        return Err(invalid("Lossless mask payload uses an invalid predictor."));
    }
    let mut decoder = RangeDecoder::new(&packed[1..])?;
    let length = width * height;
    let mut output = vec![0u8; length];
    let mut residuals = vec![0u8; length];
    let mut zero_counts = [1u32; 32];
    let mut order_one = vec![ORDER_ONE_INITIAL; SYMBOLS * SYMBOLS];
    let mut order_one_totals = [(SYMBOLS as i32) * i32::from(ORDER_ONE_INITIAL); SYMBOLS];
    let mut order_zero = [ORDER_ZERO_INITIAL; SYMBOLS];
    let mut order_zero_total = (SYMBOLS as u32) * u32::from(ORDER_ZERO_INITIAL);
    let mut order_two: HashMap<usize, Box<[u16; SYMBOLS + 1]>> = HashMap::new();
    let mut previous = 0usize;
    for offset in 0..length {
        let x = offset % width;
        let y = offset / width;
        let zero_offset = zero_context(&residuals, offset, x, y, width) << 1;
        let zero = zero_counts[zero_offset];
        let nonzero = zero_counts[zero_offset + 1];
        let zero_frequency = decoder.frequency(zero + nonzero)?;
        let bit = usize::from(zero_frequency >= zero);
        if bit == 0 {
            decoder.decode(0, zero)?;
        } else {
            decoder.decode(zero, nonzero)?;
        }
        zero_counts[zero_offset + bit] += ZERO_INCREMENT;
        if zero_counts[zero_offset] + zero_counts[zero_offset + 1] >= ZERO_LIMIT {
            zero_counts[zero_offset] = (zero_counts[zero_offset] >> 1).max(1);
            zero_counts[zero_offset + 1] = (zero_counts[zero_offset + 1] >> 1).max(1);
        }
        let mut symbol = 0usize;
        if bit == 1 {
            let above = if offset >= width { usize::from(residuals[offset - width]) } else { 0 };
            let second = order_two
                .entry((previous << 8) | above)
                .or_insert_with(|| Box::new([0u16; SYMBOLS + 1]));
            let first_offset = previous << 8;
            let frequency_of = |second: &[u16; SYMBOLS + 1], symbol: usize| {
                u32::from(second[symbol])
                    + u32::from(order_one[first_offset + symbol])
                    + u32::from(order_zero[symbol])
            };
            let excluded_zero = frequency_of(second, 0);
            let total = (u32::from(second[TOTAL_INDEX])
                + order_one_totals[previous] as u32
                + order_zero_total)
                .checked_sub(excluded_zero)
                .ok_or_else(|| invalid("Lossless mask payload is malformed."))?;
            let target = decoder.frequency(total)?;
            let mut cumulative = 0u32;
            symbol = 1;
            let mut frequency = frequency_of(second, 1);
            while cumulative + frequency <= target {
                cumulative += frequency;
                symbol += 1;
                if symbol >= SYMBOLS {
                    return Err(invalid("Lossless mask payload is malformed."));
                }
                frequency = frequency_of(second, symbol);
            }
            decoder.decode(cumulative, frequency)?;
            second[symbol] = second[symbol].wrapping_add(ORDER_TWO_STEP);
            second[TOTAL_INDEX] = second[TOTAL_INDEX].wrapping_add(ORDER_TWO_STEP);
            if second[TOTAL_INDEX] >= ORDER_TWO_LIMIT {
                let mut total_after_scaling = 0u32;
                for value in second.iter_mut().take(SYMBOLS) {
                    *value >>= 1;
                    total_after_scaling += u32::from(*value);
                }
                second[TOTAL_INDEX] = total_after_scaling as u16;
            }
            order_one[first_offset + symbol] =
                order_one[first_offset + symbol].wrapping_add(ORDER_ONE_INCREMENT);
            order_one_totals[previous] += i32::from(ORDER_ONE_INCREMENT);
            if order_one_totals[previous] >= ORDER_ONE_LIMIT {
                let mut total_after_scaling = 0i32;
                for value in &mut order_one[first_offset..first_offset + SYMBOLS] {
                    *value = (*value >> 1).max(1);
                    total_after_scaling += i32::from(*value);
                }
                order_one_totals[previous] = total_after_scaling;
            }
            order_zero[symbol] = order_zero[symbol].wrapping_add(ORDER_ZERO_STEP);
            order_zero_total += u32::from(ORDER_ZERO_STEP);
            if order_zero_total >= ORDER_ZERO_LIMIT {
                let mut total_after_scaling = 0u32;
                for value in &mut order_zero {
                    *value >>= 1;
                    total_after_scaling += u32::from(*value);
                }
                order_zero_total = total_after_scaling;
            }
        }
        residuals[offset] = symbol as u8;
        output[offset] = unzig(symbol as u32, predict_byte(&output, offset, x, y, selector, width));
        previous = symbol;
    }
    Ok(output)
}

fn context_at(raster: &[u8], position: usize, prior: usize, earlier: usize) -> usize {
    usize::from(raster[position - 1])
        | usize::from(raster[position - 2]) << 1
        | usize::from(raster[prior - 2]) << 2
        | usize::from(raster[prior - 1]) << 3
        | usize::from(raster[prior]) << 4
        | usize::from(raster[prior + 1]) << 5
        | usize::from(raster[prior + 2]) << 6
        | usize::from(raster[earlier - 2]) << 7
        | usize::from(raster[earlier - 1]) << 8
        | usize::from(raster[earlier]) << 9
        | usize::from(raster[earlier + 1]) << 10
        | usize::from(raster[earlier + 2]) << 11
}

fn update_counts(counts: &mut [u16], offset: usize, bit: usize) {
    counts[offset + bit] = counts[offset + bit].wrapping_add(INCREMENT);
    if counts[offset] + counts[offset + 1] >= COUNT_LIMIT {
        counts[offset] = (counts[offset] >> 1).max(1);
        counts[offset + 1] = (counts[offset + 1] >> 1).max(1);
    }
}

fn encode_raster(input: &[u8], width: usize, height: usize) -> Result<String, InvalidMask> {
    let mut encoder = RangeEncoder::new();
    let mut counts = vec![1u16; CONTEXT_COUNT * 2];
    let padded_width = width + 2 * PADDING;
    let mut padded = vec![0u8; padded_width * (height + PADDING)];
    for y in 0..height {
        let row = (y + PADDING) * padded_width + PADDING;
        for x in 0..width {
            let position = row + x;
            let context =
                context_at(&padded, position, position - padded_width, position - 2 * padded_width);
            let offset = context << 1;
            let zero = u32::from(counts[offset]);
            let one = u32::from(counts[offset + 1]);
            let bit = input[y * width + x];
            match bit {
                0 => encoder.encode(0, zero, zero + one),
                1 => encoder.encode(zero, one, zero + one),
                _ => return Err(invalid("Decoded mask is not binary.")),
            }
            padded[position] = bit;
            update_counts(&mut counts, offset, usize::from(bit));
        }
    }
    Ok(format!("!{}", pack(&encoder.finish())?))
}

fn decode_raster(body: &[u8], width: usize, height: usize) -> Result<Vec<u8>, InvalidMask> {
    let packed = unpack(body)?;
    let mut decoder = RangeDecoder::new(&packed)?;
    let mut counts = vec![1u16; CONTEXT_COUNT * 2];
    let padded_width = width + 2 * PADDING;
    let mut padded = vec![0u8; padded_width * (height + PADDING)];
    let mut output = vec![0u8; width * height];
    for y in 0..height {
        let row = (y + PADDING) * padded_width + PADDING;
        for x in 0..width {
            let position = row + x;
            let context =
                context_at(&padded, position, position - padded_width, position - 2 * padded_width);
            let offset = context << 1;
            let zero = u32::from(counts[offset]);
            let one = u32::from(counts[offset + 1]);
            let value = decoder.frequency(zero + one)?;
            let bit = u8::from(value >= zero);
            if bit == 0 {
                decoder.decode(0, zero)?;
            } else {
                decoder.decode(zero, one)?;
            }
            padded[position] = bit;
            output[y * width + x] = bit;
            update_counts(&mut counts, offset, usize::from(bit));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digits_are_the_documented_alphabet() {
        // protocol/sam3.md quotes the 85 digits verbatim; `}` and `~` follow, never digits.
        assert_eq!(
            &ALPHABET[..85],
            b"!#$%&'()*+-./0123456789:=?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[]^_`abcdefghijklmnopqrstuvwxyz{"
        );
        assert_eq!(&ALPHABET[85..], b"}~");
        for character in [b'"', b'\\', b',', b';', b'<', b'>', b'|', b'}', b'~', b' '] {
            assert!(digit(character).is_err(), "{}", character as char);
        }
    }

    #[test]
    fn base85_round_trips_every_tail_width() {
        for length in 0..13usize {
            let raw: Vec<u8> = (0..length).map(|i| ((i * 37 + 11) & 0xff) as u8).collect();
            assert_eq!(unpack(pack(&raw).unwrap().as_bytes()).unwrap(), raw);
        }
    }

    #[test]
    fn base85_refuses_bad_groups_tails_and_spellings() {
        let highest = ALPHABET[84] as char;
        let four = pack(&[0; 4]).unwrap();
        let group = format!("{}{}", &four[..5], highest.to_string().repeat(5));
        assert_eq!(
            unpack(group.as_bytes()).unwrap_err().reason(),
            "Mask payload contains an out-of-range group."
        );
        let one = pack(&[0]).unwrap();
        let tail = format!("{}{}", &one[..5], highest.to_string().repeat(2));
        assert_eq!(
            unpack(tail.as_bytes()).unwrap_err().reason(),
            "Mask payload contains an out-of-range tail."
        );
        let last = digit(*one.as_bytes().last().unwrap()).unwrap() as usize;
        let noncanonical = format!("{}{}", &one[..one.len() - 1], ALPHABET[last + 1] as char);
        assert_eq!(
            unpack(noncanonical.as_bytes()).unwrap_err().reason(),
            "Mask payload is not in canonical form."
        );
        assert_eq!(
            unpack("😀😀".as_bytes()).unwrap_err().reason(),
            "Mask payload contains an invalid character."
        );
    }

    #[test]
    fn every_small_one_bit_raster_round_trips() {
        for (width, height) in [(1usize, 1usize), (2, 2), (3, 3)] {
            let area = width * height;
            for value in 0u32..(1 << area) {
                let raster: Vec<u8> = (0..area).map(|i| ((value >> i) & 1) as u8).collect();
                let payload = encode_raster(&raster, width, height).unwrap();
                let mask = EncodedMask {
                    encoding: Encoding::OneBit,
                    width: width as u32,
                    height: height as u32,
                    payload: &payload,
                };
                assert_eq!(decode_mask_to_raster(&mask, area).unwrap(), raster);
            }
        }
    }

    #[test]
    fn the_lossless_predictors_match_their_definitions() {
        // Paeth picks the neighbour nearest left + up - upper-left, preferring left, then up.
        assert_eq!(paeth(10, 20, 15), 15);
        assert_eq!(paeth(10, 20, 10), 20);
        assert_eq!(paeth(10, 20, 20), 10);
        // A 2x2 output with the pixel at (1, 1) still to predict.
        let output = [10u8, 20, 30, 0];
        assert_eq!(predict_byte(&output, 3, 1, 1, 1, 2), 20);
        assert_eq!(predict_byte(&output, 3, 1, 1, 2, 2), 30);
        assert_eq!(predict_byte(&output, 3, 1, 1, 3, 2), 25);
        // The median predictor: upper-left 10 is below both neighbours, so their maximum.
        assert_eq!(predict_byte(&output, 3, 1, 1, 4, 2), 30);
        assert_eq!(unzig(0, 7), 7);
        assert_eq!(unzig(1, 7), 6);
        assert_eq!(unzig(2, 7), 8);
        assert_eq!(unzig(1, 0), 255);
    }
}
