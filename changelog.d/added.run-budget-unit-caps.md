- **Per-unit run caps and frame metering** (`adk-core`): `RunBudget::max_units(unit, max)`
  caps a unit — images, frames, pages — across a run. A call billed in units declares the most
  it can use with `UsageCall::with_units`; `ModelCallScope::meter` holds that amount before the
  call and refuses a call that would pass the cap with `budget.units`, naming the unit and the
  cap, before any request is sent. The call commits what it used and returns the rest of its
  hold; a failed or abandoned call returns all of it. Caps narrow, carve into sub-agent slices
  and serialize as `maxUnits`. `UsageUnit::Frames` counts video frames, and
  `UnitRate::with_minimum_per_request` bills every request that uses a unit for at least a
  minimum, as `sam-3.1` video bills at least 50 frames per request.
