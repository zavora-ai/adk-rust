# AI Agent Guide (ADK-Rust)

This workspace is optimized for `devenv`. AI agents **MUST** use the following high-performance workflow.

## � Commands
Use these shorthand scripts instead of raw `cargo` to ensure `sccache` wrap and workspace coverage:

| Action | Command | Description |
| :--- | :--- | :--- |
| **Check** | `devenv shell check` | Fast workspace compilation check. |
| **Test** | `devenv shell test` | Run all non-ignored tests. |
| **Lint** | `devenv shell clippy` | Clippy with `-D warnings` (zero tolerance). |
| **Format** | `devenv shell fmt` | Enforce Rust Edition 2024 style. |

## 🛠️ Toolchain Details
- **Linker (Linux)**: Exclusively uses `wild` via `link-arg=--ld-path=wild` in `.cargo/config.toml`.
- **Caching**: `sccache` is enabled. **Incremental compilation is DISABLED** (`incremental = false`) for cache compatibility.
- **Protobuf**: `PROTOC` is pre-configured in `devenv.nix`. Always rely on the environment variable.

## ⚠️ Requirements
- **System Deps**: If `pkg-config` fails (e.g., `glib`, `libva`), add them to `packages` in `devenv.nix`.
- **Environment**: Always run within `devenv shell`. `direnv` handles this automatically if installed.
- **Hooks**: Pre-commit hooks enforce formatting and linting. Always `fmt` before pushing.
