//! Text preprocessing pipeline for ONNX TTS models.
//!
//! Different ONNX TTS models expect different input formats:
//! - Kokoro: text → espeak-ng phonemization → IPA vocab tokenization → token IDs
//! - Generic HuggingFace models: text → tokenizer.json → token IDs
//!
//! The [`Preprocessor`] trait abstracts this pipeline so `OnnxTtsProvider`
//! can run any model without hardcoding the text→tokens conversion.

use std::path::Path;

use crate::error::{AudioError, AudioResult};

/// Output of a preprocessor: everything the ONNX session needs as input.
#[derive(Debug, Clone)]
pub struct PreprocessorOutput {
    /// Token IDs for the model's primary input tensor.
    pub token_ids: Vec<i64>,
    /// Optional voice/style embedding (e.g., Kokoro's 256-dim vector indexed by token length).
    pub style_embedding: Option<Vec<f32>>,
    /// Speed multiplier (passed as a separate tensor for models that support it).
    pub speed: Option<f32>,
}

/// Abstracts the text→model-inputs pipeline for ONNX TTS models.
///
/// Implementors handle text normalization, phonemization, tokenization,
/// and any model-specific input preparation (voice embeddings, speed tensors).
pub trait Preprocessor: Send + Sync {
    /// Convert text into model-ready inputs.
    ///
    /// # Arguments
    /// * `text` — raw input text to synthesize
    /// * `voice_id` — voice identifier (interpretation is model-specific)
    /// * `speed` — speaking speed multiplier
    /// * `model_dir` — path to the model directory (for loading vocab, voices, etc.)
    fn preprocess(
        &self,
        text: &str,
        voice_id: &str,
        speed: f32,
        model_dir: &Path,
    ) -> AudioResult<PreprocessorOutput>;

    /// Name of this preprocessor (for logging/diagnostics).
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// TokenizerPreprocessor — default, uses HuggingFace tokenizer.json
// ---------------------------------------------------------------------------

/// Default preprocessor using a HuggingFace `tokenizer.json` file.
///
/// Suitable for generic ONNX TTS models that ship with a tokenizer.
pub struct TokenizerPreprocessor {
    tokenizer: tokenizers::Tokenizer,
}

impl TokenizerPreprocessor {
    /// Load from a `tokenizer.json` file in the model directory.
    pub fn from_model_dir(model_dir: &Path) -> AudioResult<Self> {
        let path = model_dir.join("tokenizer.json");
        let tokenizer = tokenizers::Tokenizer::from_file(&path).map_err(|e| AudioError::Tts {
            provider: "ONNX".into(),
            message: format!("failed to load tokenizer from {}: {e}", path.display()),
        })?;
        Ok(Self { tokenizer })
    }
}

impl Preprocessor for TokenizerPreprocessor {
    fn preprocess(
        &self,
        text: &str,
        _voice_id: &str,
        speed: f32,
        _model_dir: &Path,
    ) -> AudioResult<PreprocessorOutput> {
        let encoding = self.tokenizer.encode(text, true).map_err(|e| AudioError::Tts {
            provider: "ONNX".into(),
            message: format!("tokenization failed: {e}"),
        })?;
        let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        if token_ids.is_empty() {
            return Err(AudioError::Tts {
                provider: "ONNX".into(),
                message: "tokenization produced no tokens".into(),
            });
        }
        Ok(PreprocessorOutput {
            token_ids,
            style_embedding: None,
            speed: if (speed - 1.0).abs() > f32::EPSILON { Some(speed) } else { None },
        })
    }

    fn name(&self) -> &str {
        "TokenizerPreprocessor"
    }
}

// ---------------------------------------------------------------------------
// KokoroPreprocessor — espeak-ng phonemizer + Kokoro IPA vocab
// ---------------------------------------------------------------------------

/// Kokoro IPA vocabulary: maps IPA characters to token indices.
///
/// Constructed from: pad + punctuation + ASCII letters + IPA symbols.
/// This is the same vocab used by the Kokoro-82M ONNX model.
#[cfg(feature = "kokoro")]
fn build_kokoro_vocab() -> std::collections::HashMap<char, usize> {
    let pad = "$";
    let punctuation = ";:,.!?¡¿—…\"«»\u{201c}\u{201d} ";
    let letters = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let letters_ipa = "ɑɐɒæɓʙβɔɕçɗɖðʤəɘɚɛɜɝɞɟʄɡɠɢʛɦɧħɥʜɨɪʝɭɬɫɮʟɱɯɰŋɳɲɴøɵɸθœɶʘɹɺɾɻʀʁɽʂʃʈʧʉʊʋⱱʌɣɤʍχʎʏʑʐʒʔʡʕʢǀǁǂǃˈˌːˑʼʴʰʱʲʷˠˤ˞↓↑→↗↘\u{2018}̩\u{2019}ᵻ";

    let symbols: String = [pad, punctuation, letters, letters_ipa].concat();
    symbols.chars().enumerate().map(|(idx, c)| (c, idx)).collect()
}

/// Tokenize a phoneme string using the Kokoro IPA vocabulary.
#[cfg(feature = "kokoro")]
fn kokoro_tokenize(phonemes: &str, vocab: &std::collections::HashMap<char, usize>) -> Vec<i64> {
    phonemes.chars().filter_map(|c| vocab.get(&c)).map(|&idx| idx as i64).collect()
}

/// Preprocessor for Kokoro-82M and compatible models.
///
/// Pipeline: text → espeak-ng phonemization → Kokoro IPA vocab tokenization.
/// Also loads voice embeddings from an `.npz` voices file.
///
/// # Prerequisites
///
/// Requires `espeak-ng` installed on the system:
/// ```bash
/// # macOS
/// brew install espeak-ng pkg-config
/// # Ubuntu/Debian
/// sudo apt-get install espeak-ng libespeak-ng-dev
/// ```
#[cfg(feature = "kokoro")]
pub struct KokoroPreprocessor {
    vocab: std::collections::HashMap<char, usize>,
    voices: KokoroVoices,
    language: String,
}

/// Loaded Kokoro voice embeddings from an `.npz` file.
///
/// Each voice is a tensor of shape `(N, 1, 256)` where N indexes by token length.
/// At inference time, the embedding for the current token count is selected.
#[cfg(feature = "kokoro")]
pub struct KokoroVoices {
    /// Map from voice name → Vec of 256-dim embeddings indexed by token length.
    styles: std::collections::HashMap<String, Vec<[f32; 256]>>,
}

#[cfg(feature = "kokoro")]
impl KokoroVoices {
    /// Load voices from an `.npz` file (same format as kokoros voices-v1.0.bin).
    pub fn load(voices_path: &Path) -> AudioResult<Self> {
        use ndarray::Array3;
        use ndarray_npy::NpzReader;
        use std::fs::File;

        let file = File::open(voices_path).map_err(|e| AudioError::Tts {
            provider: "ONNX/Kokoro".into(),
            message: format!("failed to open voices file {}: {e}", voices_path.display()),
        })?;
        let mut npz = NpzReader::new(file).map_err(|e| AudioError::Tts {
            provider: "ONNX/Kokoro".into(),
            message: format!("failed to read npz voices file: {e}"),
        })?;

        let names = npz.names().map_err(|e| AudioError::Tts {
            provider: "ONNX/Kokoro".into(),
            message: format!("failed to list voices in npz: {e}"),
        })?;

        let mut styles = std::collections::HashMap::new();
        for name in names {
            let arr: Array3<f32> = npz.by_name(&name).map_err(|e| AudioError::Tts {
                provider: "ONNX/Kokoro".into(),
                message: format!("failed to read voice '{name}': {e}"),
            })?;
            // Shape is (N, 1, 256) — flatten the middle dim
            let n = arr.shape()[0];
            let mut embeddings = Vec::with_capacity(n);
            for i in 0..n {
                let mut emb = [0.0f32; 256];
                for (k, val) in arr.slice(ndarray::s![i, 0, ..]).iter().enumerate() {
                    emb[k] = *val;
                }
                embeddings.push(emb);
            }
            styles.insert(name, embeddings);
        }

        tracing::info!("loaded {} Kokoro voice styles", styles.len());
        Ok(Self { styles })
    }

    /// Get the style embedding for a voice at a given token length.
    pub fn get_style(&self, voice_id: &str, token_len: usize) -> AudioResult<Vec<f32>> {
        let embeddings = self.styles.get(voice_id).ok_or_else(|| AudioError::Tts {
            provider: "ONNX/Kokoro".into(),
            message: format!(
                "voice '{voice_id}' not found. Available: {:?}",
                self.available_voices()
            ),
        })?;
        // Clamp to valid range
        let idx = token_len.min(embeddings.len().saturating_sub(1));
        Ok(embeddings[idx].to_vec())
    }

    /// List available voice names.
    pub fn available_voices(&self) -> Vec<String> {
        let mut v: Vec<String> = self.styles.keys().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(feature = "kokoro")]
impl KokoroPreprocessor {
    /// Create a new Kokoro preprocessor.
    ///
    /// # Arguments
    /// * `voices_path` — path to the `.npz` voices file (e.g., `voices-v1.0.bin`)
    /// * `language` — espeak-ng language code (e.g., `"en-us"`, `"en-gb"`)
    pub fn new(voices_path: &Path, language: &str) -> AudioResult<Self> {
        let vocab = build_kokoro_vocab();
        let voices = KokoroVoices::load(voices_path)?;
        Ok(Self { vocab, voices, language: language.to_string() })
    }

    /// Get a reference to the loaded voices for catalog queries.
    pub fn voices(&self) -> &KokoroVoices {
        &self.voices
    }
}

#[cfg(feature = "kokoro")]
impl Preprocessor for KokoroPreprocessor {
    fn preprocess(
        &self,
        text: &str,
        voice_id: &str,
        speed: f32,
        _model_dir: &Path,
    ) -> AudioResult<PreprocessorOutput> {
        // 1. Phonemize via espeak-ng
        let phonemes = espeak_rs::text_to_phonemes(text, &self.language, None, true, false)
            .map_err(|e| AudioError::Tts {
                provider: "ONNX/Kokoro".into(),
                message: format!("espeak-ng phonemization failed: {e}"),
            })?
            .join("");

        if phonemes.is_empty() {
            return Err(AudioError::Tts {
                provider: "ONNX/Kokoro".into(),
                message: "phonemization produced empty output".into(),
            });
        }

        // 2. Tokenize using Kokoro IPA vocab
        let mut token_ids = kokoro_tokenize(&phonemes, &self.vocab);
        if token_ids.is_empty() {
            return Err(AudioError::Tts {
                provider: "ONNX/Kokoro".into(),
                message: "tokenization produced no tokens from phonemes".into(),
            });
        }

        // 3. Pad with BOS/EOS (token 0 = '$' pad symbol)
        token_ids.insert(0, 0);
        token_ids.push(0);

        // 4. Load voice style embedding for this token length (pre-padding length)
        let style_len = token_ids.len() - 2; // original token count before padding
        let style = self.voices.get_style(voice_id, style_len)?;

        Ok(PreprocessorOutput { token_ids, style_embedding: Some(style), speed: Some(speed) })
    }

    fn name(&self) -> &str {
        "KokoroPreprocessor"
    }
}
