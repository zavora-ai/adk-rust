- **AWP public visitors converse under random visitor ids** (`adk-server`): a public caller
  converses as the `VisitorId` its `AWP-Subject` token, signed link, or `__Host-adk_visitor`
  cookie carries, else a new random one returned in `AWP-Subject`; ids are no longer derived from
  the address, so sessions under `public-…` ids are not reached. Consent subjects are visitor ids
  (`v:…`) instead of `subj_…`, and tokens issued before are refused. Visitor runs act for
  `SubjectKind::Visitor` through a consent gate over the receipts. `EdgeCaller` gains a `visitor`
  field, and `BeaconStore` gains the required `forget_visitor`.
