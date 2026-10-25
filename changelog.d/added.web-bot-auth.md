- **Web Bot Auth** (`adk-auth`, feature `web-bot-auth`): `adk_auth::web_bot_auth` implements
  `draft-ietf-webbotauth-httpsig-protocol-00` over HTTP Message Signatures (RFC 9421) with
  Ed25519. `HttpSignatureVerifier` verifies a request against the key directory its
  `Signature-Agent` names, requires the draft's covered components, accepts `created` and
  `expires` within 60 s of skew, and returns a `VerifiedRequester` whose id is
  `wba:{directory origin}#{key thumbprint}`; an unsigned request yields no identity.
  `HttpSignatureSigner` signs `http::Request` and `reqwest::Request`. `HttpKeyDirectorySource`
  fetches `/.well-known/http-message-signatures-directory` over https to public addresses
  only, without redirects, under a timeout and size limits; directories are cached for their
  HTTP cache lifetime within 60 s–24 h. `KeyDirectory` builds the directory a site publishes,
  with its possession proof. RFC 9421 Appendix B.2.6 and the draft's Appendix E.2 vectors
  reproduce byte for byte. Spec 3.5 §7.3.
