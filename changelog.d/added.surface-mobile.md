- **The phone** (`adk-surface` feature `mobile`, `adk-cli` feature `mobile`; spec 3.3 §5.4,
  §8.2, §10 K2): the web home installs on a phone as an app (manifest, service worker, icons)
  and pairs with a phone-kind code from **Add a phone or app**, which shows a QR code, the link
  into the app and the code; the device token is kept in IndexedDB. Each waiting decision wakes
  the person's paired phones with Web Push from their own kernel — encrypted, VAPID-signed with
  the kernel's `push.key`, a body of `{"v":1,"id":…}` only, held through quiet time, sent once
  across replicas, recorded as `surface.notify`. A phone answers by touch
  (`POST /api/surface/decisions/{id}/answer`, `POST /api/surface/stop`); a high-risk answer
  goes through **Confirm it's you** with a passkey on the phone or, in the web home, on the
  computer (webauthn-rs; links OpenSSL), bound to the decision, version, answer and edited
  arguments. `adk-rust up --home-url <https URL>` serves it, with `--push-key` for a key
  replicas share. The `scripted_phone` example is a complete server.
