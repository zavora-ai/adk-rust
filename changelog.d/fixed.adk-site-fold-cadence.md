- **Stream folds keep their five-minute cadence** (`adk-site`, spec 3.5 §5 S19): `StreamLane::due`
  counts `fold_interval` boundaries from the stream's opening instead of from the last fold's
  time, so a fold taken late by a slow poll no longer pushes the next one back. The nightly
  24-hour soak took 69 folds for a 360-minute live blog; it now takes one per boundary and one at
  the close. `ADK_SITE_S19_PRODUCTS` runs the full-day S19 over a smaller catalogue.
