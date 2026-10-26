- **A served site's proactive runs wait out the owner's quiet hours** (`adk-website`):
  `deploy::serve` gives the kernel the charter's quiet hours, so the back office's
  signal-woken and scheduled runs start after them; an agency's kernel holds none, since its
  sites' hours may differ. `Served::kernel` returns the kernel (spec 3.5 §7.21, §5 S10).
- **Photos that arrive after the Steward scheduled the draft** (`adk-website`):
  `channel::OwnerChannel::attach` adds them to the scheduled item in the content store, which
  is not live yet, instead of failing on the draft the scheduling closed (spec 3.5 §5 S18).
