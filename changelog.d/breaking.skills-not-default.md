- **`skills` is no longer a default feature** (`adk-agent`, `adk-runner`): a project that
  depends on either crate directly no longer compiles `adk-skill` and its YAML stack unless it
  asks for skills, which takes 22 packages off `adk-agent`'s default tree and 23 off
  `adk-runner`'s. The skill builder methods (`with_skills`, `with_auto_skills`,
  `with_skill_policy`, …) and `Runner::with_skill_injector` / `with_auto_skills_mut` need
  `features = ["skills"]` on the crate that provides them. `adk-rust`'s `skills` feature, in
  `standard` and above, now also turns on `adk-runner/skills`; `minimal` still compiles no
  skills.
