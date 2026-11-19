- **`UnitRate` is `#[non_exhaustive]` and `UsageUnit` gains `Frames`** (`adk-core`):
  `UnitRate` has a new `minimum_per_request` field, so it is built with `UnitRate::new`,
  `rounded_down_to` and `with_minimum_per_request` rather than a struct literal; its wire form
  is unchanged when no minimum is set. `UsageUnit::ALL` is now `[UsageUnit; 9]`, and
  `BudgetLimit` gains `Units(UsageUnit)`.

  Migration: replace a `UnitRate { .. }` literal with `UnitRate::new(unit, micro_usd, per)`
  and its builder methods; a fixed-size binding of `UsageUnit::ALL` takes nine units.
