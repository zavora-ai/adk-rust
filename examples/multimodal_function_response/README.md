# Multimodal Function Responses

Demonstrates multimodal function responses in ADK-Rust — tools returning images, audio, or file references alongside JSON to Gemini 3 models.

## What it shows

1. **Chart tool** — returns a PNG image + JSON metadata. Gemini 3 receives the image inside the `functionResponse` and describes what it sees.
2. **Document tool** — returns a file URI reference + JSON metadata. Gemini 3 receives the file pointer and summarizes the document.

## How it works

Tools return JSON with `inline_data` and/or `file_data` arrays:

```rust
Ok(json!({
    "response": { "title": "Q4 Chart", "chart_type": "bar" },
    "inline_data": [{
        "mime_type": "image/png",
        "data": png_bytes  // Vec<u8>
    }]
}))
```

The framework automatically:
1. Detects `inline_data`/`file_data` via `FunctionResponseData::from_tool_result()`
2. Base64-encodes inline data in the conversion layer
3. Nests the parts inside the `functionResponse` wire object (matching the Gemini API format)

The resulting wire format:
```json
{
  "functionResponse": {
    "name": "generate_chart",
    "response": { "title": "Q4 Chart", "chart_type": "bar" },
    "parts": [{
      "inlineData": { "mimeType": "image/png", "data": "<base64>" }
    }]
  }
}
```

## Usage

```bash
export GOOGLE_API_KEY=your-key-here
cargo run --manifest-path examples/multimodal_function_response/Cargo.toml
```

Requires a Gemini 3 model (default: `gemini-3.5-flash-lite`). Override with `GEMINI_MODEL`.

## Key APIs

```rust
// Tool returns JSON with multimodal data — framework handles the rest
Ok(json!({
    "response": { "status": "ok" },
    "inline_data": [{ "mime_type": "image/png", "data": png_bytes }]
}))

// Or with file references
Ok(json!({
    "response": { "status": "ok" },
    "file_data": [{ "mime_type": "application/pdf", "file_uri": "gs://bucket/file.pdf" }]
}))

// Direct construction (for framework-level use)
let frd = FunctionResponseData::with_inline_data("tool", json, vec![inline_part]);
let frd = FunctionResponseData::with_file_data("tool", json, vec![file_part]);
let frd = FunctionResponseData::with_multimodal("tool", json, inline_parts, file_parts);
```
