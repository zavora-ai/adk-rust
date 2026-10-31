- **Graph HTTP, notification and RSS nodes reach public addresses only by default**
  (`adk-graph`): a URL from workflow state can no longer reach loopback, private,
  link-local (the cloud metadata endpoint included), CGNAT or unique-local addresses, or
  `localhost`. Address literals are checked on the request and on every redirect, and host
  names when the client connects, through `adk-core`'s public-only resolver, now public as
  `adk_core::webhook_delivery::PublicOnlyResolver`.
