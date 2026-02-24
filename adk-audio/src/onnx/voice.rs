//! Voice embedding loading for ONNX TTS models.

use std::path::Path;

use crate::error::{AudioError, AudioResult};
use crate::traits::Voice;

/// Discover available voices from a model directory.
pub fn discover_voices(model_path: &Path) -> Vec<Voice> {
    let voices_dir = model_path.join("voices");
    if voices_dir.is_dir() {
        std::fs::read_dir(&voices_dir)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.path().file_stem()?.to_str()?.to_string();
                Some(Voice {
                    id: name.clone(),
                    name: name.clone(),
                    language: "en".into(),
                    gender: None,
                })
            })
            .collect()
    } else {
        vec![Voice {
            id: "default".into(),
            name: "Default".into(),
            language: "en".into(),
            gender: None,
        }]
    }
}

/// Load a voice embedding from a binary file.
#[allow(dead_code)] // Available for models with voice embeddings
pub fn load_voice_embedding(model_path: &Path, voice_id: &str) -> AudioResult<Option<Vec<f32>>> {
    if voice_id == "default" || voice_id.is_empty() {
        return Ok(None);
    }

    let voice_path = model_path.join("voices").join(format!("{voice_id}.bin"));
    if !voice_path.exists() {
        return Err(AudioError::Tts {
            provider: "ONNX".into(),
            message: format!("voice embedding not found: {voice_id}"),
        });
    }

    let bytes = std::fs::read(&voice_path).map_err(|e| AudioError::Tts {
        provider: "ONNX".into(),
        message: format!("failed to read voice embedding: {e}"),
    })?;
    let embedding: Vec<f32> = bytemuck::cast_slice(&bytes).to_vec();
    Ok(Some(embedding))
}
