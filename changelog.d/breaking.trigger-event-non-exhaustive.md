- **`TriggerEvent` is `#[non_exhaustive]`** (`adk-agent`, `ambient`): build it with
  `TriggerEvent::new(source, payload)` and `with_principal(..)` instead of a struct literal,
  so later fields arrive without another break. It carries no idempotency key; the
  `adk-kernel` wake sources derive one per event (spec 3.2 §13 Q7).
