# =============================================================================
# ADK-Rust Development Environment (devenv.nix)
# =============================================================================
# Reproducible dev environment using devenv.sh (https://devenv.sh)
#
# Setup:
#   1. Install devenv: https://devenv.sh/getting-started/
#   2. Run: devenv shell
#   3. Everything is ready — cargo, sccache, mold, cmake, protobuf, node, etc.
#
# This gives identical toolchains on Linux, macOS, and CI.
# =============================================================================

{ pkgs, lib, config, inputs, ... }:

{
  # --------------------------------------------------------------------------
  # Core Configuration
  # --------------------------------------------------------------------------
  name = "adk-rust";

  # Enable Cachix binary cache
  cachix.pull = [ "devenv" ];

  # Load .env file automatically
  dotenv.enable = builtins.pathExists ./.env;
  # Newer devenv CLIs read `dotenv.resolved` even when dotenv is disabled, while the
  # pinned modules define it only when enabled; the module's own value wins when set.
  dotenv.resolved = lib.mkDefault { };

  # --------------------------------------------------------------------------
  # Core Languages
  # --------------------------------------------------------------------------
  languages.rust = {
    enable = true;
    # Read the same file rustup reads, so a devenv shell and a plain `cargo`
    # invocation always agree. `channel`/`version` cannot be combined with this.
    toolchainFile = ./rust-toolchain.toml;
  };

  languages.javascript = {
    enable = true;
    package = pkgs.nodejs_22;
  };

  languages.nix.enable = true;

  # --------------------------------------------------------------------------
  # System packages available in the dev shell
  # --------------------------------------------------------------------------
  packages = with pkgs; [
    # Build essentials
    cmake              # Required for audiopus (openai-webrtc feature)
    pkg-config
    openssl
    coreutils

    # Fast linkers (Linux)
    mold
    pkgs.wild          # Advanced Linker
    clang
    lld

    # Compilation cache — dramatically speeds up rebuilds and CI
    sccache

    # System libraries required by livekit-webrtc
    glib
    pkgs.glib.dev
    libva
    dbus               # Required for libdbus-sys (used by keyring in adk-cli for secure credential storage)
    pkgs.dbus.dev

    # Protobuf (for gRPC codegen if needed)
    protobuf

    # Frontend tooling (ADK Studio UI)
    nodePackages.pnpm

    # Test runner
    cargo-nextest      # Parallel test runner (~10x faster than cargo test)

    # Utilities
    just               # Modern make alternative (optional)
    git
    jq
    curl

    # Quality gates — lefthook is the single git-hook manager (see lefthook.yml);
    # shellcheck backs its pre-commit shell-script gate
    lefthook
    shellcheck
  ]
  ++ lib.optionals pkgs.stdenv.isLinux [
    # Linux-only: faster linking, perf tools
    valgrind
    # ALSA headers for cpal-based audio (examples/desktop_audio via alsa-sys);
    # the pre-push examples gate fails without them. CI installs libasound2-dev.
    pkgs.alsa-lib
    pkgs.alsa-lib.dev
  ];

  # --------------------------------------------------------------------------
  # Environment variables
  # --------------------------------------------------------------------------
  env = {
    # ADK Root Reference
    ADK_RUST_ROOT = lib.mkDefault config.devenv.root;

    # cmake 4.x compat for audiopus builds
    CMAKE_POLICY_VERSION_MINIMUM = "3.5";

    # CARGO_INCREMENTAL is managed in .cargo/config.toml
    CARGO_REGISTRIES_CRATES_IO_PROTOCOL = "sparse";

    # Wild Linker incremental support
    WILD_INCREMENTAL = "1";

    # Explicitly set PROTOC for build-scripts (e.g., lance-encoding)
    PROTOC = "${pkgs.protobuf}/bin/protoc";

    # Ensure pkg-config can find glib, dbus and (on Linux) alsa
    PKG_CONFIG_PATH = "${pkgs.glib.dev}/lib/pkgconfig:${pkgs.dbus.dev}/lib/pkgconfig${lib.optionalString pkgs.stdenv.isLinux ":${pkgs.alsa-lib.dev}/lib/pkgconfig"}:$PKG_CONFIG_PATH";
  };

  # --------------------------------------------------------------------------
  # Task System & Scripts
  # --------------------------------------------------------------------------
  tasks = {
    "ci:test" = {
      description = "Run full workspace quality gates.";
      exec = "cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace";
    };
  };

  scripts = {
    ws-fmt.exec = "cargo fmt --all $@";
    ws-check.exec = "RUSTC_WRAPPER=sccache cargo check --workspace $@";
    ws-test.exec = "RUSTC_WRAPPER=sccache cargo nextest run --workspace $@";
    ws-test-ci.exec = "RUSTC_WRAPPER=sccache cargo nextest run --workspace --profile ci $@";
    ws-test-slow.exec = "RUSTC_WRAPPER=sccache cargo test --workspace $@";
    # --all-targets so tests, examples, and benches are linted too. Without it the
    # PR-tier clippy gate silently skipped every non-lib target, contradicting the
    # command documented in AGENTS.md and CONTRIBUTING.md.
    ws-clippy.exec = "RUSTC_WRAPPER=sccache cargo clippy --workspace --all-targets $@ -- -D warnings";
  };

  # --------------------------------------------------------------------------
  # Test & Shell Hooks
  # --------------------------------------------------------------------------
  enterTest = "ws-test-ci"; # Runs nextest with CI profile

  # --------------------------------------------------------------------------
  # Quality Gates (Git-Hooks)
  # --------------------------------------------------------------------------
  # Managed by lefthook (see lefthook.yml) — registered automatically in
  # enterShell below. devenv's own git-hooks integration is intentionally NOT
  # used: two hook managers would fight over .git/hooks/pre-commit, and
  # lefthook works identically for non-Nix contributors (scripts/setup-dev.sh
  # installs it outside devenv).

  # --------------------------------------------------------------------------
  # Shell hooks — run on entering the dev shell
  # --------------------------------------------------------------------------
  enterShell = ''
    # Register the lefthook-managed quality gates (idempotent; overwrites any
    # stale hooks left behind by the previous devenv git-hooks integration).
    lefthook install >/dev/null 2>&1 || true

    echo "🚀 Welcome to the ADK-Rust Development Environment!"
    echo "   Rust:    $(rustc --version)"
    echo "   Cargo:   $(cargo --version)"
    echo "   sccache: $(sccache --version 2>/dev/null || echo 'not found')"
    echo "   Node:    $(node --version)"
    echo ""
    echo "💡 Run 'devenv tasks list' or use the scripts: ws-fmt, ws-check, ws-test, ws-clippy."
  '';
}
