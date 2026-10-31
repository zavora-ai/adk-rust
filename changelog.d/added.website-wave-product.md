- **Leads, nurture and the warm handoff** (`adk-website`): `leads::LeadScorer` scores a lead
  from its consent, qualifying answers and the nurture it received, against a handoff
  threshold (70); `leads::Leads` answers the email outbox's `ConsentCheck` from the edge's
  consent receipts and keeps one handoff per lead; `leads::SqliteLeadRecords` keeps the
  contact book beside the edge's leads in `data/edge.db`; `leads::nurture_tools` refuses a
  sequence or newsletter send inside the owner's quiet hours and rescores the recipient's
  leads on each send; `alarms::handoff_tool_for` hands a lead off only once its score reaches
  the threshold. The charter's public terms and the edge ask for the `nurture` consent scope
  where the kind nurtures its leads (spec 3.5 §5 S8, §7.20).
- **Upkeep proposals and the publish delay** (`adk-website`): `upkeep::UpkeepCheck` reads the
  edge's `Cargo.lock` against an `AdvisorySource` (`AdvisoryDatabase` over a local RustSec
  advisory-db clone, `StaticAdvisories`) and the certificate's expiry from a
  `CertificateSource` (`OriginCertificate`, `StaticCertificate`), and proposes each upgrade or
  renewal once as a decision card for the owner; it runs as the hosting job's daily watch,
  `jobs::WATCH_WORKFLOW`. `publish_delay::PublishDelay` keeps a page the site drafts on its
  own under `freshness`, `seo` or `aeo` off the site for the delay (24 hours by default) as
  well as the owner's approval (spec 3.5 §5 S10, §7.20).
- **The owner's chat channel** (`adk-website`): `channel::OwnerChannel` attaches the photos a
  paired device sends to the Steward's draft through the back office's media intake, and the
  owner confirms or rewrites their alt text from the same thread (spec 3.5 §5 S18, A-I24).
- **The served back office** (`adk-website`): with a model, `deploy::serve` hosts the
  back-office agents on the kernel — the Steward's target runs the whole back office as one
  team, `BuiltTeam::team_runner`; each agent runs its jobs' processes, `BuiltTeam::agent_runner`
  — under the owner's standing rules in `data/kernel.db` (`SiteStores::rules`), with held
  calls in the owner's approval inbox; the edge's signals wake those processes
  (`edge::adapters::KernelSignals::wake`). `SiteOptions` gains `email`, `publish_delay`,
  `advisories`, `certificate` and `wallet` (spec 3.5 §7.19, §7.20, §7.22).
- **Review quotes in the running site** (`adk-website`, `adk-site`): `quotes::ReviewQuotes`
  keeps the reviews discovery found with the owner's approvals in `data/reviews.json`, and
  `quotes::QuotedReviews` checks every release's quotes of them in the launch gate; the
  onboarding view asks the owner about each attributed review. `adk_site::gate::review_findings`
  is public (spec 3.5 §5 S2).
- **The desk keeps its state** (`adk-website`): `ui::DeskStore` (`FileDeskStore`,
  `InMemoryDeskStore`) saves the paired device, the onboarding answers, the charter under
  review and the editor's drafts after every command; `WebsiteDesk::restore` reads them back.
  `ReviewComposer` is `Clone`, so `adk-rust website new`'s home composes the weekly review
  (spec 3.5 §8.2, §7.21).
- **Team delegates for a harness coordinator** (`adk-harness`):
  `HarnessAgentBuilder::team_delegates` classes the delegation tools an `adk-agent` `TeamSpec`
  gives its coordinator as `Delegate`, so the mode table lets it hand work over (spec 3.5
  §7.19).
