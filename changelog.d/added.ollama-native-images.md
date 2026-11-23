- **Images reach Ollama vision models** (`adk-model`, `ollama`): inline PNG, JPEG and GIF parts go
  out as the message's native `images` instead of text, so `LlmDescriber::local` over
  `OllamaModel` is a second local describer for `adk-vision` (spec 3.7 X19).
