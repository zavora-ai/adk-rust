- **`adk-rust site build` checks migration parity** (`adk-cli`, `website`): `site migrate`
  records the inventory beside its report, and `site build` and the first `site publish` run
  the launch gate's migration parity check over it with the owner's design choice;
  `site build --legacy URL` compares the looks with a legacy view (spec 3.5 §7.8.1).
