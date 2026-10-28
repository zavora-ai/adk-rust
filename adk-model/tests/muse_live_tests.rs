//! Live Meta Model API tests. Ignored by default; run with a key:
//!
//! ```bash
//! META_API_KEY=... cargo test -p adk-model --features muse --test muse_live_tests -- --ignored
//! ```

#![cfg(feature = "muse")]

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use adk_model::muse::{
    ImageOptions, MuseAsrClient, MuseClient, MuseConfig, MuseFilesClient, MuseImagesClient,
    MuseResponsesClient, ResponseRequest, TranscribeOptions,
};
use futures::StreamExt;

fn key() -> String {
    std::env::var("META_API_KEY")
        .or_else(|_| std::env::var("MODEL_API_KEY"))
        .expect("META_API_KEY or MODEL_API_KEY must be set for live Muse tests")
}

/// A valid 2x2 red PNG, built byte by byte (stored zlib block, real CRCs).
fn tiny_png() -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = kind.to_vec();
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        out
    }
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for byte in data {
            a = (a + u32::from(*byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        (b << 16) | a
    }
    // Raw image: 2 rows, filter byte + two red RGB pixels each.
    let mut raw = Vec::new();
    for _ in 0..2 {
        raw.push(0);
        raw.extend_from_slice(&[255, 0, 0, 255, 0, 0]);
    }
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    zlib.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png.extend_from_slice(&chunk(b"IHDR", &ihdr));
    png.extend_from_slice(&chunk(b"IDAT", &zlib));
    png.extend_from_slice(&chunk(b"IEND", &[]));
    png
}

#[tokio::test]
#[ignore = "live test; requires META_API_KEY"]
async fn structured_output_conforms() {
    let client = MuseClient::new(MuseConfig::spark_1_3(key())).unwrap();
    let mut request = LlmRequest::new(
        "muse-spark-1.3",
        vec![Content::new("user").with_text("Invent one idea for a solar backpack.")],
    );
    let config = GenerateContentConfig {
        response_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
        })),
        max_output_tokens: Some(1024),
        ..Default::default()
    };
    request.config = Some(config);
    let mut stream = client.generate_content(request, false).await.unwrap();
    let response = stream.next().await.unwrap().unwrap();
    let text = response
        .content
        .and_then(|c| c.parts.into_iter().next())
        .and_then(|p| match p {
            Part::Text { text } => Some(text),
            _ => None,
        })
        .expect("structured reply should carry text");
    let value: serde_json::Value = serde_json::from_str(&text).expect("reply should be JSON");
    assert!(value["title"].is_string(), "schema field missing: {text}");
}

#[tokio::test]
#[ignore = "live test; requires META_API_KEY"]
async fn vision_answers_about_an_image() {
    let client = MuseClient::new(MuseConfig::spark_1_3(key())).unwrap();
    let content = Content {
        role: "user".to_string(),
        parts: vec![
            Part::Text { text: "What color dominates this image? One word.".to_string() },
            Part::inline_data("image/png", tiny_png()),
        ],
    };
    let mut request = LlmRequest::new("muse-spark-1.3", vec![content]);
    request.config =
        Some(GenerateContentConfig { max_output_tokens: Some(4096), ..Default::default() });
    let mut stream = client.generate_content(request, false).await.unwrap();
    let response = stream.next().await.unwrap().unwrap();
    assert!(response.content.is_some(), "vision reply should not be empty");
}

#[tokio::test]
#[ignore = "live test; requires META_API_KEY"]
async fn responses_chain_two_turns() {
    let client = MuseResponsesClient::new(MuseConfig::spark_1_3(key())).unwrap();
    let first = client
        .create(ResponseRequest::text("muse-spark-1.3", "Remember the codeword ORCHID. Reply OK."))
        .await
        .unwrap();
    assert!(first.is_completed(), "first turn: {:?}", first.status);
    let usage = first.usage_metadata().expect("a completed response reports usage");
    assert!(
        usage.prompt_token_count > 0 && usage.candidates_token_count > 0,
        "Responses usage read as zero: {:?}",
        usage.provider_usage
    );
    let second = client
        .create(
            ResponseRequest::text("muse-spark-1.3", "What was the codeword? One word.")
                .with_previous(first.id.clone()),
        )
        .await
        .unwrap();
    assert!(second.text().contains("ORCHID"), "chained recall failed: {}", second.text());
    client.delete(&first.id).await.unwrap();
}

#[tokio::test]
#[ignore = "live test; requires META_API_KEY"]
async fn images_generate_a_png() {
    let client = MuseImagesClient::new(MuseConfig::new(key(), "muse-image-1.0")).unwrap();
    let response = client
        .generate("a small red barn at dawn, flat illustration", ImageOptions::default())
        .await
        .unwrap();
    let bytes = response.data[0].bytes().unwrap();
    assert!(bytes.len() > 10_000, "image too small: {}", bytes.len());
    let is_image = bytes.starts_with(b"\x89PNG")
        || bytes.starts_with(b"\xFF\xD8\xFF")
        || bytes.starts_with(b"RIFF");
    assert!(is_image, "unknown image magic: {:?}", &bytes[..4.min(bytes.len())]);
}

#[tokio::test]
#[ignore = "live test; requires META_API_KEY"]
async fn files_round_trip() {
    let client = MuseFilesClient::new(MuseConfig::spark_1_3(key())).unwrap();
    let stored = client
        .upload(b"ideate notebook".to_vec(), "note.txt", "text/plain", "user_data")
        .await
        .unwrap();
    let fetched = client.get(&stored.id).await.unwrap();
    assert_eq!(fetched.id, stored.id);
    let bytes = client.content(&stored.id).await.unwrap();
    assert_eq!(bytes, b"ideate notebook");
    client.delete(&stored.id).await.unwrap();
}

#[tokio::test]
#[ignore = "live test; requires META_API_KEY"]
async fn asr_accepts_silence() {
    // 1s of 16kHz mono PCM16 silence in a WAV container.
    let mut wav = Vec::new();
    let data_len = 16_000u32 * 2;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(wav.len() + data_len as usize, 0);
    let client = MuseAsrClient::new(MuseConfig::new(key(), "muse-voice-transcribe-1.0")).unwrap();
    let transcript =
        client.transcribe(wav, "sil.wav", "audio/wav", TranscribeOptions::default()).await.unwrap();
    assert!(transcript.text.is_empty(), "silence should transcribe empty: {:?}", transcript.text);
}
