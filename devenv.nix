# =============================================================================
# ADK-Rust Development Environment (devenv.nix)
# =============================================================================
# Optimized for monorepo scale to fix "Argument list too long" (ARG_MAX) errors.
# Merged with upstream feature set (pnpm, just, ws-summary).
# =============================================================================

{ pkgs, lib, config, ... }:

let
  llvm = pkgs.llvmPackages_latest;
  
  # Consolidated environment to fix "Argument list too long" (ARG_MAX) errors
  # by replacing dozens of individual store paths with a single search path.
  adkBuildEnv = pkgs.buildEnv {
    name = "adk-build-env";
    paths = [
      pkgs.pkg-config
      pkgs.openssl
      pkgs.cmake
      pkgs.protobuf
      pkgs.glib
      pkgs.glib.dev
      pkgs.libva
      pkgs.libvdpau
      pkgs.libxcb
      pkgs.libx11
      pkgs.libxcursor
      pkgs.libxext
      pkgs.libxi
      pkgs.libxrender
      pkgs.libxkbcommon
      pkgs.fontconfig
      pkgs.freetype
      pkgs.pipewire
      pkgs.wayland
      pkgs.dbus
      pkgs.libcap
      pkgs.systemd
      pkgs.bzip2
      pkgs.zlib
      pkgs.lz4
      pkgs.zstd
      pkgs.snappy
      # Lower priority for xorgproto to avoid header collisions with libX11
      (lib.lowPrio pkgs.xorgproto)
    ];
  };

in {
  name = "adk-rust";

  # Enable Cachix binary cache
  cachix.pull = [ "devenv" ];

  # --------------------------------------------------------------------------
  # https://devenv.sh/languages/
  # --------------------------------------------------------------------------
  languages.rust = {
    enable = true;
    channel = "stable";
    components = [ "rustc" "cargo" "clippy" "rustfmt" "rust-analyzer" ];
  };

  languages.javascript = {
    enable = true;
    package = pkgs.nodejs_22;
  };

  languages.nix.enable = true;

  # --------------------------------------------------------------------------
  # System packages
  # --------------------------------------------------------------------------
  packages = [ 
    pkgs.git
    pkgs.jq
    pkgs.curl
    pkgs.bun
    pkgs.nodePackages.pnpm
    pkgs.just
    pkgs.sccache
    pkgs.mold
    pkgs.wild
    
    # System libraries (redundant but safe for pkg-config)
    pkgs.glib
    pkgs.glib.dev
    pkgs.libva
    
    # Core build environment
    adkBuildEnv
    
    # LLVM toolchain
    llvm.clang
    llvm.libclang
    llvm.lld
  ] ++ lib.optionals pkgs.stdenv.isLinux [
    pkgs.valgrind
  ];

  # --------------------------------------------------------------------------
  # Environment variables
  # --------------------------------------------------------------------------
  env = {
    # Centralized Build Search Paths
    CPATH = "${adkBuildEnv}/include";
    LIBRARY_PATH = "${adkBuildEnv}/lib";
    PKG_CONFIG_PATH = "${adkBuildEnv}/lib/pkgconfig";
    
    # Rust/LLVM configuration
    LIBCLANG_PATH = "${llvm.libclang.lib}/lib";
    PROTOC = "${adkBuildEnv}/bin/protoc";
    
    # Global Sccache Wrappers (C, C++, Assembly)
    CC = "sccache ${llvm.clang}/bin/clang";
    CXX = "sccache ${llvm.clang}/bin/clang++";
    
    # Provide clang with the correct include paths for C++ headers
    BINDGEN_EXTRA_CLANG_ARGS = "-I${llvm.libclang.lib}/lib/clang/${lib.getVersion llvm.clang}/include";

    # CMake Sccache Integration
    CMAKE_C_COMPILER_LAUNCHER = "sccache";
    CMAKE_CXX_COMPILER_LAUNCHER = "sccache";
    
    # Linker configuration
    LD = "lld";
    
    # Optimization
    RUSTC_WRAPPER = "sccache";
    SCCACHE_CACHE_SIZE = "50G";
    WILD_INCREMENTAL = "1";
    CMAKE_POLICY_VERSION_MINIMUM = "3.5";
    
    # ADK Root Reference
    ADK_RUST_ROOT = lib.mkDefault config.devenv.root;
    CARGO_REGISTRIES_CRATES_IO_PROTOCOL = "sparse";
  };

  # --------------------------------------------------------------------------
  # https://devenv.sh/scripts/
  # --------------------------------------------------------------------------
  scripts = {
    ws-fmt.exec = "cargo fmt --all $@";
    ws-check.exec = "cargo check --all-features $@";
    ws-test.exec = "cargo test --all-features $@";
    ws-clippy.exec = "cargo clippy --all-features -- -D warnings $@";
    
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
          PASSED=$(grep -oP "\d+(?= passed)" test.log | awk '{sum += $1} END {print sum}')
          FAILED=$(grep -oP "\d+(?= failed)" test.log | awk '{sum += $1} END {print sum}')
          echo "- **Passed:** ''${PASSED:-0}" >> "$GITHUB_STEP_SUMMARY"
          echo "- **Failed:** ''${FAILED:-0}" >> "$GITHUB_STEP_SUMMARY"
        else
          echo "💡 To include test stats, run: devenv shell ws-test | tee test.log"
        fi
      else
        echo "GITHUB_STEP_SUMMARY not set, skipping summary generation."
      fi
    '';
  };

  # --------------------------------------------------------------------------
  # Quality Gates
  # --------------------------------------------------------------------------
  enterTest = "ws-test";

  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy.enable = true;
    clippy.settings.allFeatures = true;
    shellcheck.enable = true;
  };

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
