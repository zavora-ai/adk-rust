- **One address classifier for every egress check** (`adk-core`, `adk-server`, `adk-auth`,
  `adk-site`, `adk-graph`, `adk-harness`, `adk-browser`): `adk_core::egress::classify` replaces
  the copies in the A2A push sender, AWP event delivery, Web Bot Auth directories, website
  migration, graph HTTP actions, the harness `web_fetch` tool and browser navigation. Each now
  refuses 6to4 addresses embedding a non-public IPv4 address, Teredo and local-use NAT64,
  `240.0.0.0/4`, `198.18.0.0/15`, `192.0.0.0/24`, and the Azure WireServer (`168.63.129.16`),
  AWS ECS and EKS credential endpoints and Alibaba metadata address; cloud metadata endpoints
  stay refused under `EgressPolicy::allow_private_networks()`. Website migration now refuses a
  name whose answer mixes public and non-public addresses instead of dropping the non-public
  ones. `adk_core::webhook_delivery::{EgressPolicy, is_non_public, PublicOnlyResolver}` move to
  `adk_core::egress` (`PublicOnlyResolver` becomes `EgressResolver`), and
  `adk_harness::web::is_private_ip` is replaced by `adk_core::egress::is_non_public`. Refs #1281.
