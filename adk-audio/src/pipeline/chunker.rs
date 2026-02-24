//! Sentence-chunked streaming for voice agent pipelines.

/// Buffers LLM tokens and emits complete sentences at delimiter boundaries.
///
/// This reduces time-to-first-audio by sending each sentence to TTS
/// as soon as it's complete, rather than waiting for the full response.
pub struct SentenceChunker {
    buffer: String,
    delimiters: Vec<char>,
}

impl SentenceChunker {
    /// Create a new chunker with default delimiters (`.!?;\n`).
    pub fn new() -> Self {
        Self { buffer: String::new(), delimiters: vec!['.', '!', '?', ';', '\n'] }
    }

    /// Push a token and return any complete sentences.
    pub fn push(&mut self, token: &str) -> Vec<String> {
        self.buffer.push_str(token);
        let mut sentences = Vec::new();
        while let Some(pos) = self.buffer.chars().position(|c| self.delimiters.contains(&c)) {
            let byte_pos = self
                .buffer
                .char_indices()
                .nth(pos + 1)
                .map(|(i, _)| i)
                .unwrap_or(self.buffer.len());
            let sentence: String = self.buffer.drain(..byte_pos).collect();
            let trimmed = sentence.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
            }
        }
        sentences
    }

    /// Flush the remaining buffer as a final sentence.
    pub fn flush(&mut self) -> Option<String> {
        let remaining = self.buffer.trim().to_string();
        self.buffer.clear();
        if remaining.is_empty() { None } else { Some(remaining) }
    }
}

impl Default for SentenceChunker {
    fn default() -> Self {
        Self::new()
    }
}
