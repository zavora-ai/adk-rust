- **`AgUiRunInput` has a `resume` field** (`adk-server`): a struct literal that names every
  field sets `resume: None` or uses `..Default::default()`; deserialized run inputs need no
  change (spec 3.3 §10 S2).
