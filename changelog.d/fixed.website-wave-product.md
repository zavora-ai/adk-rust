- **A team member runs under its own policy** (`adk-agent`): `TeamMemberAgent::configure_run`
  applies the member's own `configure_run` before the team's transfer targets and delegation
  tools, so a `HarnessAgent` member of a compiled `TeamSpec` runs governed instead of refusing
  its run (spec 3.5 §7.19).
- **The website desk after a restart** (`adk-website`): the device the setup code paired is
  saved with the desk, so a restarted site still admits it for onboarding (A-I20).
- **The weekly review's channel summary in plain words** (`adk-website`):
  `WeeklyReview::summary` sends sentences such as "The freshness job earned more freedom; it is
  your decision whether to give it" instead of "1 promotion offer(s)" (spec 3.5 §7.21).
- **The website template's edge signs with the delegated keys** (`cargo-adk`): the edge reads
  `data/keys/card.jwk` (and `receipt.jwk`), made on its first start, instead of a new key per
  process, so `adk-rust awp keys delegate --scope card,receipt` covers what it signs (spec 3.5
  D-11).
- **Composing a weekly review leaves its alarms waiting** (`adk-website`):
  `ReviewComposer::compose` reads the alarms that wait for the review and `send` clears the
  ones the sent review carried, so composing on the web home no longer takes them from the
  weekly digest (spec 3.5 §7.21).
- **An agency's sites under `adk-rust website`** (`adk-website`, `adk-cli`): `status`,
  `review`, `charter` and `jobs` on a site listed in an `agency.toml` read the agency's
  `data/kernel.db`, where the served sites run (`ProjectPaths::kernel_database`), and
  promotions grant their rules there (A-I22). `website new` prints the site's own name.
