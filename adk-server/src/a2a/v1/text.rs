use adk_core::Event;

#[derive(Default)]
pub(super) struct ResponseText {
    chunks: Vec<(String, String)>,
}

impl ResponseText {
    pub fn push(&mut self, event: &Event) -> Option<(String, bool)> {
        let text: String = event
            .content()
            .into_iter()
            .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
            .collect();
        let snapshot = !event.llm_response.partial
            && event
                .llm_response
                .provider_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("content_complete"))
                .and_then(serde_json::Value::as_bool)
                == Some(true);
        if snapshot && let Some(index) = self.chunks.iter().position(|(id, _)| id == &event.id) {
            let emitted: String = self
                .chunks
                .iter()
                .filter(|(id, _)| id == &event.id)
                .map(|(_, text)| text.as_str())
                .collect();
            let suffix = text.strip_prefix(&emitted).map(str::to_owned);
            // Replace only this model response, preserving earlier tool rounds.
            self.chunks.retain(|(id, _)| id != &event.id);
            self.chunks.insert(index, (event.id.clone(), text));
            return Some(match suffix {
                Some(suffix) => (suffix, true),
                None => (self.text(), false),
            });
        }
        if text.is_empty() {
            return None;
        }
        self.chunks.push((event.id.clone(), text.clone()));
        Some((text, true))
    }

    pub fn text(&self) -> String {
        self.chunks.iter().map(|(_, text)| text.as_str()).collect()
    }
}
