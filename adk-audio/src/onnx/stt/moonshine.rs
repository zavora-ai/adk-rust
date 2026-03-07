//! Moonshine encoder-decoder pipeline for ONNX inference.
//!
//! [`MoonshineDecoder`] runs the Moonshine encoder on raw 16 kHz mono PCM
//! samples and then performs greedy decoding to produce a [`Transcript`].
//!
//! Unlike Whisper, Moonshine accepts **variable-length** input — there is no
//! fixed 30-second window or mel spectrogram computation. Audio is fed
//! directly as `f32[1, N]` where N is the number of 16 kHz samples.
//!
//! # Model Variants
//!
//! | Variant | Parameters | HuggingFace ID |
//! |---------|-----------|----------------|
//! | Tiny | ~27 M | `usefulsensors/moonshine-tiny-onnx` |
//! | Base | ~61 M | `usefulsensors/moonshine-base-onnx` |

use crate::error::{AudioError, AudioResult};
use crate::traits::{SttOptions, Transcript};

use ort::session::Session;
use ort::value::Value;

/// End-of-text token — signals decoding is complete.
const EOT_TOKEN: u32 = 2;

/// Maximum number of tokens the decoder will produce before stopping.
const MAX_DECODE_TOKENS: usize = 448;

// ── MoonshineDecoder ───────────────────────────────────────────────────

/// Moonshine encoder-decoder pipeline (variable-length input).
///
/// Holds a tokenizer for decoding token IDs back to text. The actual ONNX
/// sessions (encoder + decoder) are passed in by the caller so that session
/// lifetime is managed externally.
///
/// # Example
///
/// ```rust,ignore
/// let decoder = MoonshineDecoder::new(tokenizer);
/// let transcript = decoder.transcribe(
///     &mut encoder_session,
///     &mut decoder_session,
///     &pcm_samples,
///     &SttOptions::default(),
/// )?;
/// println!("{}", transcript.text);
/// ```
pub struct MoonshineDecoder {
    /// HuggingFace tokenizer for decoding token IDs to text.
    tokenizer: tokenizers::Tokenizer,
}

impl MoonshineDecoder {
    /// Create a new decoder with the given tokenizer.
    pub fn new(tokenizer: tokenizers::Tokenizer) -> Self {
        Self { tokenizer }
    }

    /// Run the full Moonshine encoder-decoder pipeline on raw PCM samples.
    ///
    /// # Arguments
    ///
    /// * `encoder` — ONNX encoder session (input: `audio f32[1, N]`)
    /// * `decoder` — ONNX decoder session (input: `encoder_output` + `input_ids`)
    /// * `samples` — 16 kHz mono f32 PCM samples (variable length, NOT padded to 30 s)
    /// * `_opts` — Transcription options (reserved for future use)
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Stt`] on tensor creation failure, ONNX inference
    /// errors, or tokenizer decode failures.
    pub fn transcribe(
        &self,
        encoder: &mut Session,
        decoder: &mut Session,
        samples: &[f32],
        _opts: &SttOptions,
    ) -> AudioResult<Transcript> {
        if samples.is_empty() {
            return Ok(Transcript {
                text: String::new(),
                confidence: 0.0,
                language_detected: None,
                words: Vec::new(),
                ..Default::default()
            });
        }

        // 1. Run encoder on raw PCM
        let encoder_output = self.run_encoder(encoder, samples)?;

        // 2. Greedy decode
        let decoded_tokens = self.greedy_decode(decoder, &encoder_output)?;

        // 3. Build transcript
        self.build_transcript(&decoded_tokens)
    }

    // ── Encoder ────────────────────────────────────────────────────────

    /// Run the Moonshine encoder on raw PCM samples `f32[1, N]`.
    ///
    /// Returns the encoder hidden states as a flat `Vec<f32>` with shape
    /// `[1, T, D]`.
    fn run_encoder(&self, encoder: &mut Session, samples: &[f32]) -> AudioResult<EncoderOutput> {
        let n_samples = samples.len() as i64;

        let audio_tensor =
            Value::from_array(([1i64, n_samples], samples.to_vec())).map_err(|e| {
                AudioError::Stt {
                    provider: "ONNX/Moonshine".into(),
                    message: format!("failed to create audio tensor [1, {n_samples}]: {e}"),
                }
            })?;

        let outputs =
            encoder.run(ort::inputs!["audio" => audio_tensor]).map_err(|e| AudioError::Stt {
                provider: "ONNX/Moonshine".into(),
                message: format!("encoder inference failed: {e}"),
            })?;

        let (shape, data) =
            outputs[0].try_extract_tensor::<f32>().map_err(|e| AudioError::Stt {
                provider: "ONNX/Moonshine".into(),
                message: format!("failed to extract encoder hidden states: {e}"),
            })?;

        let shape_vec: Vec<i64> = shape.iter().copied().collect();
        Ok(EncoderOutput { data: data.to_vec(), shape: shape_vec })
    }

    // ── Greedy decoding ────────────────────────────────────────────────

    /// Greedy decode: at each step pick the token with the highest logit.
    ///
    /// Stops when the end-of-text token is produced or [`MAX_DECODE_TOKENS`]
    /// is reached.
    fn greedy_decode(
        &self,
        decoder: &mut Session,
        encoder_hidden: &EncoderOutput,
    ) -> AudioResult<Vec<u32>> {
        // Moonshine decoder starts with a single start-of-sequence token (ID 1)
        let mut tokens: Vec<u32> = vec![1];
        let mut output_tokens: Vec<u32> = Vec::new();

        for _ in 0..MAX_DECODE_TOKENS {
            let logits = self.run_decoder_step(decoder, encoder_hidden, &tokens)?;

            let next_token = argmax(&logits);
            if next_token == EOT_TOKEN {
                break;
            }

            output_tokens.push(next_token);
            tokens.push(next_token);
        }

        Ok(output_tokens)
    }

    // ── Decoder step ───────────────────────────────────────────────────

    /// Run a single decoder step: feed `encoder_output` and `input_ids`
    /// to the decoder session, return the logits for the last position.
    fn run_decoder_step(
        &self,
        decoder: &mut Session,
        encoder_hidden: &EncoderOutput,
        tokens: &[u32],
    ) -> AudioResult<Vec<f32>> {
        let input_ids: Vec<i64> = tokens.iter().map(|&t| t as i64).collect();
        let seq_len = input_ids.len() as i64;

        let ids_tensor =
            Value::from_array(([1i64, seq_len], input_ids)).map_err(|e| AudioError::Stt {
                provider: "ONNX/Moonshine".into(),
                message: format!("failed to create input_ids tensor: {e}"),
            })?;

        let encoder_tensor =
            Value::from_array((encoder_hidden.shape.clone(), encoder_hidden.data.clone()))
                .map_err(|e| AudioError::Stt {
                    provider: "ONNX/Moonshine".into(),
                    message: format!("failed to create encoder_output tensor: {e}"),
                })?;

        let outputs = decoder
            .run(ort::inputs!["input_ids" => ids_tensor, "encoder_output" => encoder_tensor])
            .map_err(|e| AudioError::Stt {
                provider: "ONNX/Moonshine".into(),
                message: format!("decoder inference failed: {e}"),
            })?;

        let (shape, logits_data) =
            outputs[0].try_extract_tensor::<f32>().map_err(|e| AudioError::Stt {
                provider: "ONNX/Moonshine".into(),
                message: format!("failed to extract decoder logits: {e}"),
            })?;

        // logits shape: [1, seq_len, vocab_size]
        // We want the last position's logits.
        let vocab_size = if shape.len() == 3 { shape[2] as usize } else { logits_data.len() };
        let total = logits_data.len();
        let start = total.saturating_sub(vocab_size);

        Ok(logits_data[start..].to_vec())
    }

    // ── Transcript construction ────────────────────────────────────────

    /// Build a [`Transcript`] from decoded token IDs.
    fn build_transcript(&self, decoded_tokens: &[u32]) -> AudioResult<Transcript> {
        if decoded_tokens.is_empty() {
            return Ok(Transcript {
                text: String::new(),
                confidence: 0.0,
                language_detected: None,
                words: Vec::new(),
                ..Default::default()
            });
        }

        let text = self
            .tokenizer
            .decode(decoded_tokens, true)
            .map_err(|e| AudioError::Stt {
                provider: "ONNX/Moonshine".into(),
                message: format!("tokenizer decode failed: {e}"),
            })?
            .trim()
            .to_string();

        // Confidence heuristic: shorter outputs relative to max are more confident
        let confidence = if decoded_tokens.is_empty() {
            0.0
        } else {
            (1.0 - (decoded_tokens.len() as f32 / MAX_DECODE_TOKENS as f32)).max(0.1)
        };

        Ok(Transcript {
            text,
            confidence,
            language_detected: None,
            words: Vec::new(),
            ..Default::default()
        })
    }
}

// ── Helper types ───────────────────────────────────────────────────────

/// Encoder output: hidden states data and shape.
struct EncoderOutput {
    /// Flat f32 data of shape `[1, T, D]`.
    data: Vec<f32>,
    /// Shape as `[1, T, D]`.
    shape: Vec<i64>,
}

// ── Free functions ─────────────────────────────────────────────────────

/// Argmax over a float slice, returning the index of the maximum value.
fn argmax(values: &[f32]) -> u32 {
    values
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_argmax_basic() {
        assert_eq!(argmax(&[0.1, 0.9, 0.5]), 1);
        assert_eq!(argmax(&[3.0, 1.0, 2.0]), 0);
    }

    #[test]
    fn test_argmax_empty() {
        assert_eq!(argmax(&[]), 0);
    }

    #[test]
    fn test_eot_token_value() {
        // Moonshine uses token ID 2 for end-of-text
        assert_eq!(EOT_TOKEN, 2);
    }

    #[test]
    fn test_max_decode_tokens() {
        assert_eq!(MAX_DECODE_TOKENS, 448);
    }
}
