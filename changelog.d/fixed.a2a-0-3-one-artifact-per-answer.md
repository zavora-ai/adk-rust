- **An A2A 0.3 task holds one artifact per answer** (`adk-server`): a streamed answer reached
  `message/send` and `tasks/get` as one artifact per chunk, all with the same `artifactId`,
  and a last one with empty text. Updates marked `append` now extend the artifact they name,
  and `message/stream` sends no update with an empty text part.
