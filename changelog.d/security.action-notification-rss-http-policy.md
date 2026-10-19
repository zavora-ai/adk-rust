- **Notification and RSS action nodes follow the `HttpActionPolicy`** (`adk-graph`): the
  webhook URL of a notification node and the feed URL of an RSS node are interpolated from
  workflow state, but both nodes sent their requests with a plain `reqwest::Client`, so a
  host or scheme allowlist set with `ActionNodeExecutor::with_http_policy` did not stop them
  from reaching internal services, and redirects were followed unchecked. Both now check the
  URL and every redirect against the executor's policy, as the HTTP node does
  (`notification::execute_notification_with_policy`, `rss::execute_rss_with_policy`). The
  notification node logged its raw webhook URL template at debug level; it now logs the host
  only, since a Slack, Discord, or Teams webhook URL is a credential. Policy refusals now
  name the "HTTP action policy" instead of "HTTP node".
