# ADK-Rust Makefile
# Common build commands for development

.PHONY: help setup check-env cache-stats cache-clear build build-all build-release test clippy fmt clean \
	examples build-openai build-anthropic build-ollama build-mistralrs build-mistralrs-metal \
	build-mistralrs-cuda docs

# Default target
help:
	@echo "ADK-Rust Build Commands"
	@echo "======================="
	@echo ""
	@echo "Setup:"
	@echo "  make setup        - Install/check dev tools (scripts/setup-dev.sh)"
	@echo "  make check-env    - Check what's installed without changing anything"
	@echo ""
	@echo "Basic Commands:"
	@echo "  make build        - Build all workspace crates (default features)"
	@echo "  make build-all    - Build the umbrella crate with the full tier"
	@echo "  make test         - Run the workspace test suite (nextest)"
	@echo "  make clippy       - Run the lint gate (-D warnings)"
	@echo "  make fmt          - Format code"
	@echo "  make clean        - Clean build artifacts"
	@echo ""
	@echo "Examples:"
	@echo "  make examples     - Compile every standalone example crate"
	@echo ""
	@echo "mistral.rs (local LLM inference, GPU features opt-in):"
	@echo "  make build-mistralrs       - Build adk-mistralrs (CPU-only)"
	@echo "  make build-mistralrs-metal - Build with Metal GPU (macOS)"
	@echo "  make build-mistralrs-cuda  - Build with CUDA GPU (requires toolkit)"
	@echo ""
	@echo "Feature-Specific Builds:"
	@echo "  make build-openai     - Build with OpenAI support"
	@echo "  make build-anthropic  - Build with Anthropic support"
	@echo "  make build-ollama     - Build with Ollama support"
	@echo ""
	@echo "Cache:"
	@echo "  make cache-stats  - Show sccache hit/miss statistics"
	@echo "  make cache-clear  - Clear sccache and cargo caches"
	@echo ""
	@echo "Documentation:"
	@echo "  make docs         - Generate documentation"

# ---------------------------------------------------------------------------
# Setup & environment
# ---------------------------------------------------------------------------

# Install or check dev tools
setup:
	@./scripts/setup-dev.sh

# Check what's installed without changing anything
check-env:
	@./scripts/setup-dev.sh --check

# Show sccache statistics
cache-stats:
	@sccache --show-stats 2>/dev/null || echo "sccache not installed. Run: make setup"

# Clear all caches
cache-clear:
	@sccache --stop-server 2>/dev/null || true
	cargo clean
	@echo "Caches cleared."

# ---------------------------------------------------------------------------
# Build commands
# ---------------------------------------------------------------------------

# Build all workspace crates (CPU-only, safe for all systems)
build:
	cargo build --workspace

# Build the umbrella crate's full tier; --all-features would turn on adk-mistralrs's cuda and metal
build-all:
	cargo build -p adk-rust --features full

# Build with release optimizations
build-release:
	cargo build --workspace --release

# Run the workspace test suite — the test gate
test:
	cargo nextest run --workspace

# Run the lint gate
clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# Format code
fmt:
	cargo fmt --all

# Clean build artifacts
clean:
	cargo clean

# Compile every standalone example; each is its own workspace, outside the root one
examples:
	bash scripts/check-examples-compile.sh

# Feature-specific builds
build-openai:
	cargo build -p adk-model --features "openai"

build-anthropic:
	cargo build -p adk-model --features "anthropic"

build-ollama:
	cargo build -p adk-model --features "ollama"

# mistral.rs builds (workspace member, GPU features opt-in)
build-mistralrs:
	cargo build -p adk-mistralrs

build-mistralrs-metal:
	cargo build -p adk-mistralrs --features "metal"

build-mistralrs-cuda:
	@echo "Note: Requires NVIDIA CUDA toolkit installed"
	cargo build -p adk-mistralrs --features "cuda"

# Generate documentation
docs:
	cargo doc --workspace --no-deps --open
