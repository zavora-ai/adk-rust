- **The CLI escapes terminal control characters** (`adk-cli`): `StreamPrinter` printed model
  text, thinking, tool names, file-data and embedded-resource URIs, and think-block content
  raw, and the console printed stream errors raw, so an `ESC` or C1 sequence from a model or
  tool could rewrite the terminal or set its clipboard. Control characters other than
  newline and tab are now printed as `\u{..}` escapes. `StreamPrinter::with_writer` prints
  to any writer.
