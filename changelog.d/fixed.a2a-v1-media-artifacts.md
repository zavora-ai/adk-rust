- **A2A v1 artifacts carry the agent's files and images** (`adk-server`): only an agent's
  text reached the client, so inline data and file references were lost. They are now
  recorded as a second artifact beside the text, raw bytes with their media type or a file
  URL, on send, stream and `GetTask` alike (`v1::convert::media_part_to_wire`). Function
  calls and responses, thinking and provider items stay inside the agent, as adk-python
  keeps them out of artifacts.
