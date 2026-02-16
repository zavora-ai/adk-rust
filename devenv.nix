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

{ pkgs, lib, config, ... }:

{
  # --------------------------------------------------------------------------
  # Core Configuration
  # --------------------------------------------------------------------------
  name = "adk-rust";

  # Enable Cachix binary cache
  cachix.pull = [ "devenv" ];

  # Load .env file automatically
  dotenv.enable = builtins.pathExists ./.env;

  # --------------------------------------------------------------------------
  # Core Languages
  # --------------------------------------------------------------------------
  languages.rust = {
    enable = true;
    channel = "stable";
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
    libva

    # Protobuf (for gRPC codegen if needed)
    protobuf

    # Frontend tooling (ADK Studio UI)
    nodePackages.pnpm

    # Utilities
    just               # Modern make alternative (optional)
    git
    jq
    curl
  ]
  ++ lib.optionals pkgs.stdenv.isLinux [
    # Linux-only: faster linking, perf tools
    valgrind
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
  };

  # --------------------------------------------------------------------------
  # Task System & Scripts
  # --------------------------------------------------------------------------
  tasks = {
    "ci:test" = {
      description = "Run full workspace checks.";
      exec = "cargo check && cargo test";
    };
  };

  scripts = {
    fmt.exec = "cargo fmt --all";
    check.exec = "RUSTC_WRAPPER=sccache cargo check --workspace";
    test.exec = "RUSTC_WRAPPER=sccache cargo test --workspace";
    clippy.exec = "RUSTC_WRAPPER=sccache cargo clippy --workspace -- -D warnings";
  };

  # --------------------------------------------------------------------------
  # Test & Shell Hooks
  # --------------------------------------------------------------------------
  enterTest = "test"; # Runs the 'test' script above

  # --------------------------------------------------------------------------
  # Quality Gates (Git-Hooks)
  # --------------------------------------------------------------------------
  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy.enable = true;
    shellcheck.enable = true;
  };

  # --------------------------------------------------------------------------
  # Shell hooks — run on entering the dev shell
  # --------------------------------------------------------------------------
  enterShell = ''
    echo "🚀 Welcome to the ADK-Rust Development Environment!"
    echo "   Rust:    $(rustc --version)"
    echo "   Cargo:   $(cargo --version)"
    echo "   sccache: $(sccache --version 2>/dev/null || echo 'not found')"
    echo "   Node:    $(node --version)"
    echo ""
    echo "💡 Run 'devenv tasks list' or use the scripts: fmt, check, test, clippy."
  '';
}
