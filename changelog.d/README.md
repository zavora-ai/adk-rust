# Changelog fragments

Each pull request records its user-facing change here as one small file instead of
editing `CHANGELOG.md`. Two pull requests that both append to the same `[Unreleased]`
lines conflict on every rebase; two fragment files never do. The release preparation
pull request assembles the fragments into `CHANGELOG.md`.

## Writing a fragment

1. Name the file `<section>.<slug>.md`, where `<section>` is the changelog heading
   the entry belongs under and `<slug>` is a short lowercase kebab-case name.

   | Section | Use for |
   |---------|---------|
   | `breaking` | API or behaviour changes that require caller changes |
   | `security` | fixes to a vulnerability or hardening of a trust boundary |
   | `added` | new public APIs, features, backends, or examples |
   | `changed` | changed behaviour that stays compatible |
   | `fixed` | bug fixes |

2. Write the entry exactly as it will appear in `CHANGELOG.md`: one or more Markdown
   list items, continuation lines indented two spaces, crate names in backticks, bold
   lead for the headline.

```markdown
- **Redis session keys encode `%` and `:`** (`adk-session`): ids containing either
  character no longer collide with other sessions. Ids without them keep their keys.
```

3. Check it with `bash scripts/changelog-assemble.sh --check`. CI runs the same check.

## Assembling

`bash scripts/changelog-assemble.sh` moves every fragment under `## [Unreleased]`,
grouped by section in the order Breaking, Security, Added, Changed, Fixed, and deletes
the fragment files. `--into <version>` targets the dated release-candidate heading
instead. See CONTRIBUTING.md ("Release Process").
