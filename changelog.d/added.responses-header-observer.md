- **Responses response-header observer** (`adk-model`):
  `OpenAIResponsesClient::with_response_observer` takes a `ResponseObserver`
  (`Arc<dyn Fn(&http::HeaderMap) + Send + Sync>`) that receives the raw headers of every
  `POST /responses` attempt, retries and failed statuses included, before status handling or
  body reads. Background polling and compaction requests are not observed.
