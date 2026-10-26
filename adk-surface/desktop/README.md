# adk-desktop

The ADK-Rust desktop tray (spec 3.3 §5.5): the web home in a desktop window plus a tray icon
with the decisions badge and a panel — running state, spending against the daily limit,
decisions, what is working now and next, a quick message box, **Open home** and **Stop all**.

**Stability: Experimental.** An unpublished app in its own Cargo workspace, excluded from the
root workspace so Tauri never enters the library crates' `Cargo.lock`. There is no `desktop`
feature on `adk-surface`, and no installer. See
[the desktop tray](../../docs/official_docs/surfaces/desktop.md) for what it shows and may
decide.

| Path | What it is |
|---|---|
| `core/` | `adk-desktop-core`: which kernel to reach, its status as a badge, where links go, where the panel opens. No Tauri, tested without a display |
| `app/` | `adk-desktop`: the Tauri 2 shell. Its windows load the kernel's `/ui/` and `/ui/tray`; no capability names a remote page |
| `probe/` | The shell's bundled pages, built into `dist/`: "Can't reach your agents" and the WebKitGTK probe (a terminal on the DOM and WebGL renderers, a 3,000-line diff) |

## Run

```bash
adk-rust up
npm ci --prefix adk-surface/desktop/probe
npm run build --prefix adk-surface/desktop/probe
cargo run --manifest-path adk-surface/desktop/app/Cargo.toml --bin adk-desktop -- \
    --server http://127.0.0.1:8080
```

Pass `--token` (or set `ADK_SERVE_TOKEN`) for a kernel started with `--auth-token`. On Linux,
build with Tauri's prerequisites installed (`libwebkit2gtk-4.1-dev`,
`libayatana-appindicator3-dev`, `librsvg2-dev`, `libxdo-dev`, `libssl-dev`).

## Check

```bash
cargo fmt --manifest-path adk-surface/desktop/Cargo.toml --all -- --check
cargo clippy --manifest-path adk-surface/desktop/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path adk-surface/desktop/Cargo.toml --workspace
```

`.github/workflows/desktop.yml` runs these, then drives the shell on WebKitGTK through
`tauri-driver` under Xvfb, and compiles it on macOS and Windows. It is not part of the
merge gate.

The WebKitGTK tests (`app/tests/webkitgtk.rs`, run through `e2e.sh`) also need
`tauri-driver` (`cargo install tauri-driver --locked`), `xvfb`, `dbus-x11` and
`WebKitWebDriver`: the `webkitgtk-webdriver` package on Ubuntu 26.04 and later,
`webkit2gtk-driver` on earlier releases.
