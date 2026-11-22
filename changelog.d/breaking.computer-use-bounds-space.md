- **`Bounds` state their coordinate space and device pixel ratio** (`adk-computer-use`):
  `Bounds` gains `space: CoordinateSpace` and `device_pixel_ratio`, and `TargetEvidence`
  gains `device_pixel_ratio`; struct literals add the fields. Bounds without them on the
  wire read as logical points at ratio 1. `TargetEvidence::check_screen_binding` refuses
  bounds from a screenshot whose ratio is missing or differs (spec 3.7 X17, V-I14).
- **`Overlay::Mark` has a `scale`** (`adk-vision`): the badge size relative to the base, the
  device pixel ratio for marks on a high-density screenshot; struct literals add `scale: 1.0`.
