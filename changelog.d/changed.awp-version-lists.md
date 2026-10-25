- **`AWP-Version` accepts a list of versions** (`adk-awp`, spec 3.5 §7.2): the 1.x middleware
  answers `AWP-Version: 2.0, 1.0` in 1.0 instead of refusing it with `400`; a request that
  names no 1.x version still gets `406`.
