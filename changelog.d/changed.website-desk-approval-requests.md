- **The web desk decides the site's approval requests** (`adk-website`, `adk-harness`): an
  editor release waiting for its chain shows the votes of its approval request, keeps the
  request id in `data/desk.json` across a restart, and publishes once the request is approved
  on any surface — the desk, the back office, the kernel's inbox or a chat card. Charter
  signing goes through `SiteRuntime::request_charter` and `sign_charter`, so the desk and the
  back office sign one record; promoting a job answers its waiting promotion request. A
  freeze holds a release a person approves in the desk: the served kernel and its sites share
  one kill switch mirrored over the governance store (`SiteStores::governance`). A new gate,
  `website_replicas_tests`, runs the release chain across two runtimes over one SQLite or
  PostgreSQL database.
