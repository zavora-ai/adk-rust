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

    # Ensure pkg-config can find glib and dbus
    PKG_CONFIG_PATH = "${pkgs.glib.dev}/lib/pkgconfig:${pkgs.dbus.dev}/lib/pkgconfig:$PKG_CONFIG_PATH";
  };

  # --------------------------------------------------------------------------
  # Task System & Scripts
  # --------------------------------------------------------------------------
  tasks = {
    "ci:test" = {
      description = "Run full workspace quality gates.";
      exec = "cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo nextest run --workspace";
    };
  };

  scripts = {
    ws-fmt.exec = "cargo fmt --all $@";
    ws-check.exec = "RUSTC_WRAPPER=sccache cargo check --workspace $@";
    ws-test.exec = "RUSTC_WRAPPER=sccache cargo nextest run --workspace $@";
    ws-test-ci.exec = "RUSTC_WRAPPER=sccache cargo nextest run --workspace --profile ci $@";
    ws-test-slow.exec = "RUSTC_WRAPPER=sccache cargo test --workspace $@";
    ws-clippy.exec = "RUSTC_WRAPPER=sccache cargo clippy --workspace $@ -- -D warnings";
    ws-summary.exec = ''
      if [ -n "$GITHUB_STEP_SUMMARY" ]; then
        echo "## 🚀 CI Summary" >> "$GITHUB_STEP_SUMMARY"

        # 1. Sccache Stats
        echo "### 🏎️ Sccache Performance" >> "$GITHUB_STEP_SUMMARY"
        if command -v sccache >/dev/null; then
          STATS=$(sccache --show-stats --stats-format=json)
          HITS=$(echo "$STATS" | jq -r '.stats.cache_hits.counts | to_entries | map(.value) | add // 0')
          MISSES=$(echo "$STATS" | jq -r '.stats.cache_misses.counts | to_entries | map(.value) | add // 0')
          TOTAL=$((HITS + MISSES))
          if [ "$TOTAL" -gt 0 ]; then
            HIT_RATE=$(awk "BEGIN {printf \"%.2f\", $HITS * 100 / $TOTAL}")
            echo "- **Cache Hit Rate:** $HIT_RATE%" >> "$GITHUB_STEP_SUMMARY"
            echo "- **Hits:** $HITS" >> "$GITHUB_STEP_SUMMARY"
            echo "- **Misses:** $MISSES" >> "$GITHUB_STEP_SUMMARY"
          else
            echo "No cache activity recorded or sccache not initialized." >> "$GITHUB_STEP_SUMMARY"
          fi
        fi

        # 2. Clippy Warnings
        if [ -f "clippy.json" ]; then
          echo "### 🔍 Clippy Lints" >> "$GITHUB_STEP_SUMMARY"
          WARNINGS=$(grep -c '"level":"warning"' clippy.json || echo 0)
          ERRORS=$(grep -c '"level":"error"' clippy.json || echo 0)
          echo "- **Errors:** $ERRORS" >> "$GITHUB_STEP_SUMMARY"
          echo "- **Warnings:** $WARNINGS" >> "$GITHUB_STEP_SUMMARY"
        else
          echo "💡 To include clippy stats, run: devenv shell ws-clippy --message-format=json | tee clippy.json"
        fi

        # 3. Test Results
        if [ -f "test.log" ]; then
          echo "### 🧪 Test Results" >> "$GITHUB_STEP_SUMMARY"
          # nextest summary line: "N tests run: N passed, N skipped"
          if grep -q "tests run:" test.log; then
            RUN=$(grep "tests run:" test.log | grep -oP "\d+(?= tests run)" || echo 0)
            PASSED=$(grep "tests run:" test.log | grep -oP "\d+(?= passed)" || echo 0)
            SKIPPED=$(grep "tests run:" test.log | grep -oP "\d+(?= skipped)" || echo 0)
            FAILED=$(grep "tests run:" test.log | grep -oP "\d+(?= failed)" || echo 0)
            echo "- **Run:** ''${RUN:-0}" >> "$GITHUB_STEP_SUMMARY"
            echo "- **Passed:** ''${PASSED:-0}" >> "$GITHUB_STEP_SUMMARY"
            echo "- **Failed:** ''${FAILED:-0}" >> "$GITHUB_STEP_SUMMARY"
            echo "- **Skipped:** ''${SKIPPED:-0}" >> "$GITHUB_STEP_SUMMARY"
          else
            # Fallback: cargo test output
            PASSED=$(grep -oP "\d+(?= passed)" test.log | awk '{sum += $1} END {print sum}')
            FAILED=$(grep -oP "\d+(?= failed)" test.log | awk '{sum += $1} END {print sum}')
            echo "- **Passed:** ''${PASSED:-0}" >> "$GITHUB_STEP_SUMMARY"
            echo "- **Failed:** ''${FAILED:-0}" >> "$GITHUB_STEP_SUMMARY"
          fi
        else
          echo "💡 To include test stats, run: devenv shell ws-test-ci | tee test.log"
        fi
      else
        echo "GITHUB_STEP_SUMMARY not set, skipping summary generation."
      fi
    '';
  };

  # --------------------------------------------------------------------------
  # Test & Shell Hooks
  # --------------------------------------------------------------------------
  enterTest = "ws-test-ci"; # Runs nextest with CI profile

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
    echo "💡 Run 'devenv tasks list' or use the scripts: ws-fmt, ws-check, ws-test, ws-clippy."
  '';
}
