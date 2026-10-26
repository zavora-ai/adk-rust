- **`adk-rust up` serves signed webhooks** (`adk-cli`; spec 3.5 §10 X11): the hooks
  `[[kernel.webhooks]]` lists in the user config (`name`, and `secret` as `env:VAR` or
  `file:PATH`) are served at `POST /hooks/{name}` through a `WebhookIngress` on the kernel's
  run store and kill switch, so `adk-rust wake --webhook <name>` sleepers and the partitions'
  triggers wake on a verified delivery, once per `webhook-id`. `HarnessLauncher` gains
  `with_webhook_ingress`. Without the table nothing changes.
