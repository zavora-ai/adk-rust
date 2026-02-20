# Gemini Multimodal Example

Demonstrates sending images to Gemini models using ADK-Rust's multimodal content types.

## What it shows

1. **Inline image analysis** — Send a base64-encoded PNG via `Part::InlineData` and ask Gemini to describe it
2. **Multi-image comparison** — Send two images in a single request for comparison
3. **Vision agent** — An `LlmAgent` configured for image analysis tasks with an interactive console

## Setup

```bash
export GOOGLE_API_KEY=your-api-key
# or
export GEMINI_API_KEY=your-api-key
```

## Run

```bash
cargo run --example gemini_multimodal
```

## Key types

- `Part::InlineData { mime_type, data }` — Embed binary image data directly in the request
- `Part::FileData { mime_type, file_uri }` — Reference an image by URL (supported by Gemini API but requires the file to be accessible)

## Notes

- Gemini supports JPEG, PNG, GIF, WebP, and PDF inline data
- The `adk-model` Gemini client automatically base64-encodes `InlineData` bytes for the API
- For production use with large images, consider using Google Cloud Storage URIs via `Part::FileData`
