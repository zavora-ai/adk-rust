- **`adk-rust website new|status|review|charter|jobs`** (`adk-cli`, feature `website`; spec
  3.5 §7.17, §7.11, §7.21, §8.1): `website new [dir] [--agency]` scaffolds a site project,
  starts its kernel and agent edge (`adk_website::deploy::serve`, or `serve_agency`), serves
  the website views of the web home beside them on loopback, prints the setup code and the
  owner's key steps, and opens onboarding. `status`, `review` and `charter` read the site's
  stores; `jobs promote|demote` change a job's autonomy level as the owner's decision,
  granting or revoking its standing rules (A-I13).
