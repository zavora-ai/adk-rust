- **`adk-rust site …` and `adk-rust awp …`** (`adk-cli`, feature `website`; spec 3.5 §8.1):
  `site build`, `publish`, `rollback`, `log`, `content new`, `schedule`, `correct`,
  `withdraw`, `revert`, `batches`, `feeds`, `migrate` and `freeze` run an AWP 2.0 site's
  pipeline on its content repository, publishing signed releases to a `DirectoryHost`; every
  change is confirmed at the terminal or with `--yes`, and a charter approval chain for
  publication refuses a lone publish (A-I23). `awp verify <domain>` runs the verification
  procedure with `GET` only and names the broken link; `awp conformance <url>` runs the
  conformance kit; `awp keys rotate|delegate` sign the key set and contract with the root key
  file at a terminal only; `awp migrate-config business.toml` writes site data and a draft
  charter. See [Sites and AWP from the command line](docs/official_docs/deployment/site-cli.md).
