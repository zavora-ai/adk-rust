- **The website runtime of the harness** (`adk-harness`, feature `website`; spec 3.5 §7.1,
  §7.3, §7.8.2, §7.10, §7.11, §7.18): `Preset::Website` and `website_back_office` /
  `website_public` build the agents of an AWP 2.0 website in its public and back-office
  partitions. The back-office toolset drafts, edits, schedules, corrects, withdraws and
  reverts content, takes media, previews, publishes and rolls back through `adk-site`, reads
  the host, runs the launch gate and `awp verify`, and raises charter-commit and promotion
  requests as decision cards; the launch is held for the owner in every mode. Public agents
  hold only `search_site`, `emit_signal` (screened, bounded, untrusted fields), `adk-drivers`'
  `reply_in_thread` and the declared capabilities. `LadderCompiler` compiles each job's
  autonomy level to a mode, a policy read at every call, within-budget rules and standing
  rules an owner's decision creates (`promote_job`); `AuthoringRouter` lets only paired
  devices author content; `SiteSigning` signs crawls, `awp verify` and, opted in through
  `HarnessAgentBuilder::web_signer`, `web_fetch` with Web Bot Auth; `OnboardingStage` gives
  the onboarding plan's steps, and interview answers are `Known` only from the owner's own
  words. `wallet_pay` joins the payments pack's payment tools.
