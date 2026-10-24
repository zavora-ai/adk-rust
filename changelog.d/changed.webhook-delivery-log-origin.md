- **Webhook delivery logs name the endpoint's origin** (`adk-core`): `WebhookDelivery` no
  longer writes the endpoint's path or query to its logs, nor the URL inside a connection
  error, as either may carry a token. `DeadLetter::url` keeps the full URL.
