- **The Anthropic text editor's `insert` inserts after `insert_line`** (`adk-tool`):
  `AnthropicTextEditorTool20250124`, `…20250429` and `…20250728` refused `insert_line: 0` and
  put the text before line N, where Anthropic's tool inserts after line N and reads 0 as the
  start of the file. A trailing newline in `insert_text` no longer adds a blank line, and the
  tool's argument errors (`old_str` not found, a line out of range) reach the model as
  `invalid_input` rejections with their message instead of `tool.internal (tool.legacy)`.
