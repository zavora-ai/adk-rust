- **`EventActions` has a `steering` field** (`adk-core`): the `SteeringMarker` of a message
  `Runner::steer` delivered. A struct literal that names every field needs it or
  `..Default::default()`; it is omitted from serialized events when `None`.
