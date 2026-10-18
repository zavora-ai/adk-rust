- **Model-supplied skill names stay in the agent's project** (`adk-skill`): with a
  registry client, `SkillToolset`'s `load_skill` and `load_skill_resource` load only a name
  that passes `validate_skill_name`, or the client's own `{parent}/skills/{name}`. A full
  resource name in another project, or a name with `/` or `..`, reached that project's
  registry endpoint with the agent's credentials, and the skill's `allowed-tools` could
  enable host tools. `SkillRegistryClient` also rejects skill and revision names that are
  not a bare ID or a well-formed resource name, before any request.
