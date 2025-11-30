# Agent Development Guide

This document provides guidelines for developing agents and applications using the gemini-rust library.

## Logging

The gemini-rust library uses structured logging with the `tracing` crate for comprehensive observability.

### Setup

Initialize tracing in your main function:

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    // Your code here
}
```

### Conventions

**Message formatting**: Use lowercase messages
```rust
info!("starting content generation");  // ✓
info!("Starting content generation");  // ✗
```

**Field naming**: Use dot notation and descriptive boolean flags. Group related fields with common prefixes.
```rust
info!(status.code = 200, file.size = 1024, tools.present = true, "request completed");
```

**Value formatting**:
- Strings: `field = value`
- Errors: `error = %err`
- Complex types: `field = ?value`
- Optional types: Use native support (`field = optional_value`). Do not use `.unwrap_or("none")` or similar constructs; `tracing` handles `Option` types automatically.
- Optional enums: Convert to `Option<String>` for better queryability. For enums deriving `strum::AsRefStr`, use the more readable `field = enum_opt.as_ref().map(AsRef::<str>::as_ref)`. For other types, use `field = enum_opt.as_ref().map(|t| format!("{t:?}"))`. This transforms `Some(MyEnum::Variant)` into `Some("Variant")`, which is easier to search in logs.

**Span placeholders**: Define fields in `#[instrument]` and populate with `Span::current().record()`
```rust
#[instrument(skip_all, fields(model, status.code))]
async fn process(&self) -> Result<(), Error> {
    Span::current().record("model", self.model.as_str());
    debug!("processing request");
    // ... operation
    Span::current().record("status.code", 200);
}
```

**Instrumentation patterns**: Use descriptive field names with logical grouping:
```rust
#[instrument(skip_all, fields(
    model,
    messages.parts.count = request.contents.len(),
    tools.present = request.tools.is_some(),
    system.instruction.present = request.system_instruction.is_some(),
    cached.content.present = request.cached_content.is_some(),
    task.type = request.task_type.as_ref().map(|t| format!("{:?}", t)),
    task.title = request.title,
    task.output.dimensionality = request.output_dimensionality,
    batch.size = request.requests.len(),
    batch.display_name = request.batch.display_name,
    operation.name = name,
    page.size = page_size,
    page.token.present = page_token.is_some(),
    file.name = name,
    file.size = file_bytes.len(),
    mime.type = mime_type.to_string(),
    file.display_name = display_name.as_deref(),
))]
```

**Log levels**: `debug!` for details, `info!` for status, `warn!` for issues, `error!` for failures.

### Examples

```rust
info!(batch_name = "my-batch", requests.count = 10, "batch started");
error!(error = %err, model = "gemini-2.5-flash", "generation failed");

// Content generation instrumentation
#[instrument(skip_all, fields(
    model,
    messages.parts.count = request.contents.len(),
    tools.present = request.tools.is_some(),
    system.instruction.present = request.system_instruction.is_some(),
    cached.content.present = request.cached_content.is_some(),
))]

// Embedding instrumentation with enum handling
#[instrument(skip_all, fields(
    model,
    task.type = request.task_type.as_ref().map(|t| format!("{:?}", t)),
    task.title = request.title,
    task.output.dimensionality = request.output_dimensionality,
))]

// File operations instrumentation
#[instrument(skip_all, fields(
    file.name = name,
))]

// Batch operations instrumentation
#[instrument(skip_all, fields(
    operation.name = name,
))]

// Pagination instrumentation
#[instrument(skip_all, fields(
    page.size = page_size,
    page.token.present = page_token.is_some(),
))]
```