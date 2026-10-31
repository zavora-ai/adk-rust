- **`HttpActionPolicy` refuses non-public addresses unless `allow_private_networks` is set**
  (`adk-graph`): a workflow whose HTTP, notification or RSS node calls a service on a
  private network, or on the same host, fails with a policy refusal until its executor's
  policy adds `.allow_private_networks()`, ideally beside `allow_hosts` naming the service.
  An allowlisted host is refused at a non-public address too. Under the default policy the
  nodes no longer use the system proxies; `allow_private_networks` uses them again.
