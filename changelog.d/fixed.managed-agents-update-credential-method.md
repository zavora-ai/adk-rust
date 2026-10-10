- **Credential updates use `POST`** (`adk-anthropic`): `ManagedAgentsClient::update_credential`
  sent `PATCH`, which the API answers with HTTP 405.
