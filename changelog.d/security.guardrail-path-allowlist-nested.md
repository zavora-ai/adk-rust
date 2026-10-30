- **`PathAllowList` checks the keys it names at any depth** (`adk-guardrail`): it read
  only the top-level argument, so a path nested under another key — `{"config": {"path":
  "/etc/shadow"}}` — reached the tool unchecked when `allow_missing()` was set, or beside a
  permitted top-level path. Every occurrence of a configured name in a nested object or an
  array is now checked, and the call is denied if any holds a path outside the allowed roots
  or a value other than a path string or an array of path strings. A path found at any depth
  satisfies the missing-argument check, so a tool that nests its path no longer needs
  `allow_missing()`. A denial names the occurrence by JSON pointer. The guardrail docs now
  state that the check is a preflight with a time-of-check/time-of-use race, and that a tool
  opening files across a trust boundary must open them relative to a trusted directory
  handle, as the `adk-devtools` file tools do.
