- **The OpenAI Responses client records `provider_error_code` by the shared rule**
  (`adk-model`): an HTTP error read its code from the error object's `code` only, so a body
  with `"code": null` and a `type`, or a Google-style body with a numeric `code` and a
  `status`, carried no `provider_error_code`. The code is now the first Google
  `ErrorInfo.reason`, then the string `code`, `type`, or `status`, as on the other clients.
