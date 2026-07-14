# openai-vision-detail

Offline validation for [issue #395](https://github.com/zavora-ai/adk-rust/issues/395):
`adk-model` must never send `"detail": null` on OpenAI image parts.

## The bug

`async-openai` 0.33's `ImageUrl.detail` has no `#[serde(skip_serializing_if)]`,
so a `None` serializes as an explicit `"detail": null`. The official OpenAI API
tolerates it, but stricter OpenAI-compatible gateways validate `detail` against
`{auto, low, high}` and reject `null` with HTTP 400:

```
image_url.detail
  Input should be 'auto', 'low' or 'high' [type=literal_error, input_value=None]
```

The fix makes `adk-model` emit the API default `"auto"` instead of `None`.

## What this example does

1. Starts a mock **strict gateway** on `localhost` that rejects any request
   whose `image_url.detail` is `null` and accepts `auto`/`low`/`high`.
2. Points the real `OpenAIClient` at that gateway and sends a vision request
   containing both an inline (`data:`) image and a URL image.
3. Prints the `detail` values the gateway received and asserts they are `"auto"`.

No API key or network access is required — the gateway is local.

## Run

```bash
cargo run -p openai-vision-detail
```

Expected output ends with:

```
PASS: no `"detail": null` was sent; strict gateway accepted the vision request.
```

Before the fix, the gateway would reject the request with HTTP 400, and the
example would exit with an error.
