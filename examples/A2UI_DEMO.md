# A2UI v0.9 Demo

This demo showcases the complete A2UI v0.9 implementation with:
- ✅ Nested component format validation
- ✅ Component helper functions
- ✅ A2UI-specific prompt guidance
- ✅ React client with A2UI converter
- ✅ Real-time UI rendering

## Quick Start

```bash
# Start both UI server and React client
./start_a2ui_demo.sh

# Or manually:
# Terminal 1: Start UI server
cargo run --example ui_server

# Terminal 2: Start React client
cd examples/ui_react_client
npm run dev
```

Then open http://localhost:5173 in your browser.

## Try These Prompts

- "Create a welcome screen"
- "Show me a dashboard with stats"
- "Create a login form"
- "Build a product card"

## Architecture

### Backend (Rust)
- **ui_server** - HTTP server with SSE streaming
- **A2UI Tools** - `render_screen`, `render_page`, `render_kit`
- **Component Helpers** - `text()`, `column()`, `row()`, `button()`, etc.
- **Validator** - Validates nested A2UI v0.9 format

### Frontend (React)
- **A2UI Converter** - Transforms nested A2UI to flat format
- **Renderer** - Renders 28 component types
- **SSE Client** - Streams UI updates in real-time

## A2UI v0.9 Format

**Nested Structure (Backend):**
```json
{
  "id": "title",
  "component": {
    "Text": {
      "text": { "literalString": "Hello" },
      "variant": "h1"
    }
  }
}
```

**Flat Structure (Frontend):**
```json
{
  "type": "text",
  "id": "title",
  "content": "Hello",
  "variant": "h1"
}
```

The converter handles this transformation automatically.

## Files

### Backend
- `adk-ui/src/a2ui/validator.rs` - Schema validation
- `adk-ui/src/a2ui/components.rs` - Helper functions
- `adk-ui/src/a2ui/prompts.rs` - LLM prompt
- `adk-ui/src/tools/render_screen.rs` - Screen rendering tool
- `examples/ui_server/main.rs` - HTTP server

### Frontend
- `examples/ui_react_client/src/adk-ui-renderer/a2ui-converter.ts` - Format converter
- `examples/ui_react_client/src/adk-ui-renderer/Renderer.tsx` - Component renderer
- `examples/ui_react_client/src/App.tsx` - Main app with SSE

## Testing

```bash
# Run all adk-ui tests
cargo test -p adk-ui

# Test specific tool
cargo test -p adk-ui render_screen_emits_jsonl

# Run standalone demo
cargo run --example a2ui_demo
```

## Status

✅ **Complete** - All 39 tests passing, full A2UI v0.9 support
