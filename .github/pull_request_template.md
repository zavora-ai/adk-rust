## What

Brief description of the change.

## Why

Link to the issue this addresses: Fixes #___

## How

Summary of the approach taken.

## PR Checklist

### Quality Gates (all required)

- [ ] `devenv shell fmt` — code is formatted (Edition 2024)
- [ ] `devenv shell clippy` — zero warnings (-D warnings)
- [ ] `devenv shell test` — all non-ignored tests pass
- [ ] `devenv shell check` — fast workspace compilation check

### Code Quality

- [ ] New code has tests (unit, integration, or property tests as appropriate)
- [ ] Public APIs have rustdoc comments with `# Example` sections
- [ ] No `println!`/`eprintln!` in library code (use `tracing` instead)
- [ ] No hardcoded secrets, API keys, or local paths

### Hygiene

- [ ] No local development artifacts (`.env`, `.DS_Store`, IDE configs, build dirs)
- [ ] No unrelated changes mixed in (formatting, refactoring, other features)
- [ ] Branch naming follows convention (`feat/`, `fix/`, `docs/`, etc.)
- [ ] Commit messages follow conventional format (`feat:`, `fix:`, `docs:`, etc.)
- [ ] PR targets `main` branch

### Documentation (if applicable)

- [ ] CHANGELOG.md updated for user-facing changes
- [ ] README updated if crate capabilities changed
- [ ] Examples added or updated for new features
