//! Whisper / Distil-Whisper encoder-decoder pipeline for ONNX inference.
//!
//! [`WhisperDecoder`] runs the Whisper encoder on a mel spectrogram tensor
//! and then performs greedy or beam-search decoding to produce a [`Transcript`].
//!
//! # Special Tokens
//!
//! | Token | ID |
//! |---|---|
//! | `<\|endoftext\|>` | 50257 |
//! | `<\|startoftranscript\|>` | 50258 |
//! | `<\|en\|>` | 50259 |
//! | `<\|transcribe\|>` | 50359 |
//! | `<\|translate\|>` | 50358 |
//! | `<\|nospeech\|>` | 50362 |
//! | `<\|notimestamps\|>` | 50363 |
//! | Timestamp tokens | 50364+ (each = 20ms) |

use crate::error::{AudioError, AudioResult};
use crate::traits::{SttOptions, Transcript, Word};

use super::config::OnnxSttConfig;
use ort::session::Session;
use ort::value::Value;

// ── Whisper special token IDs ──────────────────────────────────────────

/// `<|endoftext|>` — signals decoding is complete.
const EOT_TOKEN: u32 = 50257;
/// `<|startoftranscript|>` — always the first decoder token.
const SOT_TOKEN: u32 = 50258;
/// `<|en|>` — first language token (English). Other languages follow sequentially.
const EN_TOKEN: u32 = 50259;
/// `<|translate|>` — task token for translation mode.
#[allow(dead_code)]
const TRANSLATE_TOKEN: u32 = 50358;
/// `<|transcribe|>` — task token for transcription mode.
const TRANSCRIBE_TOKEN: u32 = 50359;
/// `<|nospeech|>` — indicates no speech detected.
const NOSPEECH_TOKEN: u32 = 50362;
/// `<|notimestamps|>` — suppress timestamp token generation.
const NO_TIMESTAMPS_TOKEN: u32 = 50363;
/// First timestamp token. Each subsequent token represents +20ms.
const TIMESTAMP_BEGIN: u32 = 50364;

/// Maximum number of tokens the decoder will produce before stopping.
const MAX_DECODE_TOKENS: usize = 448;

/// Probability threshold above which `<|nospeech|>` triggers an empty transcript.
const NOSPEECH_THRESHOLD: f64 = 0.6;

/// Number of mel bands expected by the Whisper encoder.
const N_MELS: i64 = 80;
/// Number of time frames expected by the Whisper encoder (30 seconds).
const N_FRAMES: i64 = 3000;

/// Milliseconds per timestamp token step.
const TIMESTAMP_STEP_MS: u32 = 20;

// ── Beam search candidate ──────────────────────────────────────────────

/// A single candidate sequence maintained during beam search.
#[derive(Clone)]
struct BeamCandidate {
    /// Token IDs generated so far.
    tokens: Vec<u32>,
    /// Cumulative log-probability.
    score: f64,
}

// ── WhisperDecoder ─────────────────────────────────────────────────────

/// Whisper / Distil-Whisper encoder-decoder pipeline.
///
/// Holds the STT configuration and a tokenizer for decoding token IDs back
/// to text. The actual ONNX sessions (encoder + decoder) are passed in by
/// the caller so that session lifetime is managed externally.
///
/// # Example
///
/// ```rust,ignore
/// let decoder = WhisperDecoder::new(config, tokenizer);
/// let transcript = decoder.transcribe(
///     &mut encoder_session,
///     &mut decoder_session,
///     &mel_data,
///     &SttOptions::default(),
/// )?;
/// println!("{}", transcript.text);
/// ```
pub struct WhisperDecoder {
    /// STT configuration (beam size, temperature, language, etc.).
    config: OnnxSttConfig,
    /// HuggingFace tokenizer for decoding token IDs to text.
    tokenizer: tokenizers::Tokenizer,
}

impl WhisperDecoder {
    /// Create a new decoder with the given configuration and tokenizer.
    pub fn new(config: OnnxSttConfig, tokenizer: tokenizers::Tokenizer) -> Self {
        Self { config, tokenizer }
    }

    /// Run the full Whisper encoder-decoder pipeline on a mel spectrogram.
    ///
    /// # Arguments
    ///
    /// * `encoder` — ONNX encoder session (input: `input_features [1, 80, 3000]`)
    /// * `decoder` — ONNX decoder session (input: `encoder_hidden_states` + `input_ids`)
    /// * `mel` — Flat mel spectrogram of exactly `80 × 3000` elements
    /// * `opts` — Transcription options (language hint, word timestamps, etc.)
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Stt`] on tensor creation failure, ONNX inference
    /// errors, or tokenizer decode failures.
    pub fn transcribe(
        &self,
        encoder: &mut Session,
        decoder: &mut Session,
        mel: &[f32],
        opts: &SttOptions,
    ) -> AudioResult<Transcript> {
        // 1. Run encoder
        let encoder_hidden = self.run_encoder(encoder, mel)?;

        // 2. Build initial decoder token sequence
        let initial_tokens = self.build_initial_tokens(opts, decoder, &encoder_hidden)?;

        // 3. Decode (greedy or beam search)
        let decoded_tokens = if self.config.beam_size > 1 {
            self.beam_search_decode(decoder, &encoder_hidden, &initial_tokens.tokens)?
        } else {
            self.greedy_decode(decoder, &encoder_hidden, &initial_tokens.tokens)?
        };

        // 4. Build transcript from token IDs
        let mut transcript = self.build_transcript(&decoded_tokens, opts)?;
        transcript.language_detected = initial_tokens.detected_language;
        Ok(transcript)
    }

    // ── Encoder ────────────────────────────────────────────────────────

    /// Run the Whisper encoder on a mel spectrogram tensor `[1, 80, 3000]`.
    ///
    /// Returns the raw encoder hidden states as a flat `Vec<f32>` along with
    /// the shape `[1, T, D]` where T is the number of time steps and D is
    /// the hidden dimension.
    fn run_encoder(&self, encoder: &mut Session, mel: &[f32]) -> AudioResult<EncoderOutput> {
        let mel_tensor =
            Value::from_array(([1i64, N_MELS, N_FRAMES], mel.to_vec())).map_err(|e| {
                AudioError::Stt {
                    provider: "ONNX/Whisper".into(),
                    message: format!("failed to create mel tensor [1, 80, 3000]: {e}"),
                }
            })?;

        let outputs = encoder.run(ort::inputs!["input_features" => mel_tensor]).map_err(|e| {
            AudioError::Stt {
                provider: "ONNX/Whisper".into(),
                message: format!("encoder inference failed: {e}"),
            }
        })?;

        let (shape, data) =
            outputs[0].try_extract_tensor::<f32>().map_err(|e| AudioError::Stt {
                provider: "ONNX/Whisper".into(),
                message: format!("failed to extract encoder hidden states: {e}"),
            })?;

        let shape_vec: Vec<i64> = shape.iter().copied().collect();
        Ok(EncoderOutput { data: data.to_vec(), shape: shape_vec })
    }

    // ── Initial token sequence ─────────────────────────────────────────

    /// Build the initial decoder token sequence based on options.
    ///
    /// Sequence: `<|startoftranscript|>` [language_token] `<|transcribe|>` [`<|notimestamps|>`]
    ///
    /// If no language is specified, runs one decoder step to auto-detect
    /// the language from logits over language token positions.
    fn build_initial_tokens(
        &self,
        opts: &SttOptions,
        decoder: &mut Session,
        encoder_hidden: &EncoderOutput,
    ) -> AudioResult<InitialTokens> {
        let mut tokens: Vec<u32> = vec![SOT_TOKEN];
        let mut detected_language: Option<String> = None;

        if let Some(ref lang) = opts.language.as_ref().or(self.config.language.as_ref()) {
            // Language explicitly set — look up the token
            let lang_token_str = format!("<|{lang}|>");
            if let Some(id) = self.tokenizer.token_to_id(&lang_token_str) {
                tokens.push(id);
            } else {
                // Fallback: use English
                tokens.push(EN_TOKEN);
            }
        } else {
            // Auto-detect language: run one decoder step with just SOT
            let logits = self.run_decoder_step(decoder, encoder_hidden, &tokens)?;
            let lang_id = self.detect_language_from_logits(&logits);
            tokens.push(lang_id);
            detected_language = self.token_id_to_language(lang_id);
        }

        // Add task token
        tokens.push(TRANSCRIBE_TOKEN);

        // Add no-timestamps token unless word timestamps are requested
        if !opts.word_timestamps {
            tokens.push(NO_TIMESTAMPS_TOKEN);
        }

        Ok(InitialTokens { tokens, detected_language })
    }

    // ── Greedy decoding ────────────────────────────────────────────────

    /// Greedy decode: at each step pick the token with the highest logit.
    ///
    /// Stops when `<|endoftext|>` is produced or [`MAX_DECODE_TOKENS`] is reached.
    /// If `<|nospeech|>` probability exceeds [`NOSPEECH_THRESHOLD`], returns
    /// an empty token list signalling silence.
    fn greedy_decode(
        &self,
        decoder: &mut Session,
        encoder_hidden: &EncoderOutput,
        initial: &[u32],
    ) -> AudioResult<Vec<u32>> {
        let mut tokens = initial.to_vec();
        let mut output_tokens: Vec<u32> = Vec::new();

        for _ in 0..MAX_DECODE_TOKENS {
            let logits = self.run_decoder_step(decoder, encoder_hidden, &tokens)?;

            // Check nospeech probability on the first generated token
            if output_tokens.is_empty() && self.is_nospeech(&logits) {
                return Ok(Vec::new());
            }

            let next_token = argmax(&logits);
            if next_token == EOT_TOKEN {
                break;
            }

            output_tokens.push(next_token);
            tokens.push(next_token);
        }

        Ok(output_tokens)
    }

    // ── Beam search decoding ───────────────────────────────────────────

    /// Simple beam search: maintain top-k candidates at each step.
    ///
    /// `k` is determined by `config.beam_size`. Returns the highest-scoring
    /// completed sequence, or the best partial sequence if none completed.
    fn beam_search_decode(
        &self,
        decoder: &mut Session,
        encoder_hidden: &EncoderOutput,
        initial: &[u32],
    ) -> AudioResult<Vec<u32>> {
        let beam_width = self.config.beam_size as usize;

        let mut beams = vec![BeamCandidate { tokens: initial.to_vec(), score: 0.0 }];
        let mut completed: Vec<BeamCandidate> = Vec::new();
        let initial_len = initial.len();

        for _ in 0..MAX_DECODE_TOKENS {
            let mut all_candidates: Vec<BeamCandidate> = Vec::new();

            for beam in &beams {
                let logits = self.run_decoder_step(decoder, encoder_hidden, &beam.tokens)?;

                // Check nospeech on first generated token
                if beam.tokens.len() == initial_len && self.is_nospeech(&logits) {
                    return Ok(Vec::new());
                }

                let log_probs = log_softmax(&logits);

                // Take top-k tokens
                let mut indexed: Vec<(usize, f64)> =
                    log_probs.iter().copied().enumerate().collect();
                indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                indexed.truncate(beam_width);

                for (token_idx, log_prob) in indexed {
                    let token = token_idx as u32;
                    let new_score = beam.score + log_prob;
                    let mut new_tokens = beam.tokens.clone();
                    new_tokens.push(token);

                    let candidate = BeamCandidate { tokens: new_tokens, score: new_score };

                    if token == EOT_TOKEN {
                        completed.push(candidate);
                    } else {
                        all_candidates.push(candidate);
                    }
                }
            }

            if all_candidates.is_empty() {
                break;
            }

            // Keep top beam_width candidates
            all_candidates
                .sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
            all_candidates.truncate(beam_width);
            beams = all_candidates;
        }

        // Pick best completed sequence, or best partial if none completed
        let best = completed
            .iter()
            .chain(beams.iter())
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));

        match best {
            Some(b) => Ok(b.tokens[initial_len..].to_vec()),
            None => Ok(Vec::new()),
        }
    }

    // ── Decoder step ───────────────────────────────────────────────────

    /// Run a single decoder step: feed `encoder_hidden_states` and `input_ids`
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
                provider: "ONNX/Whisper".into(),
                message: format!("failed to create input_ids tensor: {e}"),
            })?;

        let encoder_tensor =
            Value::from_array((encoder_hidden.shape.clone(), encoder_hidden.data.clone()))
                .map_err(|e| AudioError::Stt {
                    provider: "ONNX/Whisper".into(),
                    message: format!("failed to create encoder_hidden_states tensor: {e}"),
                })?;

        // Check if the decoder model expects a `use_cache_branch` input
        // (merged decoder models from Optimum/HuggingFace use this boolean
        // to switch between initial pass and cached KV-state pass).
        // Since we don't manage KV cache, always pass `false`.
        let has_cache_input =
            decoder.inputs().iter().any(|input| input.name() == "use_cache_branch");

        let outputs = if has_cache_input {
            let cache_branch =
                Value::from_array(([1i64], vec![false])).map_err(|e| AudioError::Stt {
                    provider: "ONNX/Whisper".into(),
                    message: format!("failed to create use_cache_branch tensor: {e}"),
                })?;
            decoder
                .run(ort::inputs![
                    "input_ids" => ids_tensor,
                    "encoder_hidden_states" => encoder_tensor,
                    "use_cache_branch" => cache_branch
                ])
                .map_err(|e| AudioError::Stt {
                    provider: "ONNX/Whisper".into(),
                    message: format!("decoder inference failed: {e}"),
                })?
        } else {
            decoder
                .run(ort::inputs![
                    "input_ids" => ids_tensor,
                    "encoder_hidden_states" => encoder_tensor
                ])
                .map_err(|e| AudioError::Stt {
                    provider: "ONNX/Whisper".into(),
                    message: format!("decoder inference failed: {e}"),
                })?
        };

        let logits_value = &outputs[0];
        let (shape, logits_data) =
            logits_value.try_extract_tensor::<f32>().map_err(|e| AudioError::Stt {
                provider: "ONNX/Whisper".into(),
                message: format!("failed to extract decoder logits: {e}"),
            })?;

        // logits shape: [1, seq_len, vocab_size]
        // We want the last position's logits.
        let vocab_size = if shape.len() == 3 { shape[2] as usize } else { logits_data.len() };
        let total = logits_data.len();
        let start = total.saturating_sub(vocab_size);

        Ok(logits_data[start..].to_vec())
    }

    // ── Language detection ──────────────────────────────────────────────

    /// Detect language from decoder logits by finding the argmax over
    /// language token positions (50259..50358).
    fn detect_language_from_logits(&self, logits: &[f32]) -> u32 {
        // Language tokens span from EN_TOKEN (50259) to TRANSLATE_TOKEN (50358) exclusive
        let lang_start = EN_TOKEN as usize;
        let lang_end = TRANSLATE_TOKEN as usize; // 50358

        if logits.len() <= lang_start {
            return EN_TOKEN; // fallback
        }

        let end = lang_end.min(logits.len());
        let mut best_idx = lang_start;
        let mut best_val = f32::NEG_INFINITY;

        for (i, &val) in logits.iter().enumerate().take(end).skip(lang_start) {
            if val > best_val {
                best_val = val;
                best_idx = i;
            }
        }

        best_idx as u32
    }

    /// Convert a language token ID back to a BCP-47 language code string.
    fn token_id_to_language(&self, token_id: u32) -> Option<String> {
        // Decode the token to get e.g. "<|en|>" then strip delimiters
        if let Some(token_str) = self.tokenizer.id_to_token(token_id) {
            let stripped = token_str.trim_start_matches("<|").trim_end_matches("|>");
            if !stripped.is_empty() && stripped != token_str {
                return Some(stripped.to_string());
            }
        }
        None
    }

    // ── Nospeech detection ─────────────────────────────────────────────

    /// Check if the `<|nospeech|>` token probability exceeds the threshold.
    fn is_nospeech(&self, logits: &[f32]) -> bool {
        let idx = NOSPEECH_TOKEN as usize;
        if idx >= logits.len() {
            return false;
        }

        let probs = softmax(logits);
        probs[idx] > NOSPEECH_THRESHOLD as f32
    }

    // ── Transcript construction ────────────────────────────────────────

    /// Build a [`Transcript`] from decoded token IDs.
    ///
    /// Filters out special tokens, decodes text via the tokenizer, and
    /// optionally extracts word-level timestamps from timestamp tokens.
    fn build_transcript(
        &self,
        decoded_tokens: &[u32],
        opts: &SttOptions,
    ) -> AudioResult<Transcript> {
        // Empty tokens → nospeech / silence
        if decoded_tokens.is_empty() {
            return Ok(Transcript {
                text: String::new(),
                confidence: 0.0,
                language_detected: None,
                words: Vec::new(),
                ..Default::default()
            });
        }

        // Extract word timestamps if requested
        let words = if opts.word_timestamps {
            self.extract_word_timestamps(decoded_tokens)
        } else {
            Vec::new()
        };

        // Filter out special/timestamp tokens for text decoding
        let text_token_ids: Vec<u32> = decoded_tokens
            .iter()
            .copied()
            .filter(|&t| t < EOT_TOKEN || (t > NO_TIMESTAMPS_TOKEN && t < TIMESTAMP_BEGIN))
            .collect();

        let text = self
            .tokenizer
            .decode(&text_token_ids, true)
            .map_err(|e| AudioError::Stt {
                provider: "ONNX/Whisper".into(),
                message: format!("tokenizer decode failed: {e}"),
            })?
            .trim()
            .to_string();

        // Estimate confidence from the number of tokens produced vs max
        let confidence = if decoded_tokens.is_empty() {
            0.0
        } else {
            // Simple heuristic: shorter outputs relative to max are more confident
            // Real confidence would require tracking per-token probabilities
            (1.0 - (decoded_tokens.len() as f32 / MAX_DECODE_TOKENS as f32)).max(0.1)
        };

        Ok(Transcript {
            text,
            confidence,
            language_detected: None, // Set by caller from InitialTokens
            words,
            ..Default::default()
        })
    }

    /// Extract per-word timing from timestamp tokens in the decoded sequence.
    ///
    /// Timestamp tokens (≥ 50364) encode time as `(token_id - 50364) × 20ms`.
    /// Words between consecutive timestamp pairs get the corresponding time range.
    fn extract_word_timestamps(&self, tokens: &[u32]) -> Vec<Word> {
        let mut words = Vec::new();
        let mut current_start_ms: Option<u32> = None;
        let mut current_word_tokens: Vec<u32> = Vec::new();

        for &token in tokens {
            if token >= TIMESTAMP_BEGIN {
                let time_ms = (token - TIMESTAMP_BEGIN) * TIMESTAMP_STEP_MS;

                if current_start_ms.is_none() {
                    // Opening timestamp
                    current_start_ms = Some(time_ms);
                } else if let Some(start) = current_start_ms {
                    // Closing timestamp — emit word if we have tokens
                    if !current_word_tokens.is_empty() {
                        if let Ok(text) = self.tokenizer.decode(&current_word_tokens, true) {
                            let text = text.trim().to_string();
                            if !text.is_empty() {
                                words.push(Word {
                                    text,
                                    start_ms: start,
                                    end_ms: time_ms,
                                    confidence: 0.5, // No per-word confidence without tracking
                                    speaker: None,
                                });
                            }
                        }
                        current_word_tokens.clear();
                    }
                    // This closing timestamp becomes the next opening timestamp
                    current_start_ms = Some(time_ms);
                }
            } else if token < EOT_TOKEN {
                // Regular text token
                current_word_tokens.push(token);
            }
        }

        // Flush remaining tokens without a closing timestamp
        if !current_word_tokens.is_empty() {
            if let Ok(text) = self.tokenizer.decode(&current_word_tokens, true) {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    let start = current_start_ms.unwrap_or(0);
                    words.push(Word {
                        text,
                        start_ms: start,
                        end_ms: start, // Unknown end
                        confidence: 0.5,
                        speaker: None,
                    });
                }
            }
        }

        words
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

/// Result of building the initial token sequence.
struct InitialTokens {
    /// Token IDs including SOT, language, task, and optional notimestamps.
    tokens: Vec<u32>,
    /// Auto-detected language (if language was not specified in options).
    detected_language: Option<String>,
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

/// Compute softmax probabilities from logits.
fn softmax(logits: &[f32]) -> Vec<f32> {
    if logits.is_empty() {
        return Vec::new();
    }
    let max_val = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = logits.iter().map(|&x| (x - max_val).exp()).collect();
    let sum: f32 = exps.iter().sum();
    if sum == 0.0 {
        return vec![0.0; logits.len()];
    }
    exps.iter().map(|&e| e / sum).collect()
}

/// Compute log-softmax from logits (for beam search scoring).
fn log_softmax(logits: &[f32]) -> Vec<f64> {
    if logits.is_empty() {
        return Vec::new();
    }
    let max_val = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
    let shifted: Vec<f64> = logits.iter().map(|&x| x as f64 - max_val).collect();
    let log_sum_exp = shifted.iter().map(|&x| x.exp()).sum::<f64>().ln();
    shifted.iter().map(|&x| x - log_sum_exp).collect()
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
    fn test_softmax_sums_to_one() {
        let logits = vec![1.0, 2.0, 3.0, 4.0];
        let probs = softmax(&logits);
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_softmax_empty() {
        assert!(softmax(&[]).is_empty());
    }

    #[test]
    fn test_softmax_max_element() {
        let logits = vec![0.0, 0.0, 10.0, 0.0];
        let probs = softmax(&logits);
        // The element at index 2 should dominate
        assert!(probs[2] > 0.99);
    }

    #[test]
    fn test_log_softmax_consistency() {
        let logits = vec![1.0, 2.0, 3.0];
        let log_probs = log_softmax(&logits);
        let probs: Vec<f64> = log_probs.iter().map(|&lp| lp.exp()).collect();
        let sum: f64 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_timestamp_token_to_ms() {
        // Token 50364 = 0ms, 50365 = 20ms, 50414 = 1000ms
        assert_eq!((50364u32 - TIMESTAMP_BEGIN) * TIMESTAMP_STEP_MS, 0);
        assert_eq!((50365u32 - TIMESTAMP_BEGIN) * TIMESTAMP_STEP_MS, 20);
        assert_eq!((50414u32 - TIMESTAMP_BEGIN) * TIMESTAMP_STEP_MS, 1000);
    }

    #[test]
    fn test_special_token_constants() {
        assert_eq!(EOT_TOKEN, 50257);
        assert_eq!(SOT_TOKEN, 50258);
        assert_eq!(EN_TOKEN, 50259);
        assert_eq!(TRANSCRIBE_TOKEN, 50359);
        assert_eq!(NOSPEECH_TOKEN, 50362);
        assert_eq!(NO_TIMESTAMPS_TOKEN, 50363);
        assert_eq!(TIMESTAMP_BEGIN, 50364);
    }
}
