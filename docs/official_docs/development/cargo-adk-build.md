# cargo adk build

## Overview

The `cargo adk build` command compiles your ADK-Rust agent project without deploying it. This gives you a fast way to verify that your agent compiles correctly — catching dependency issues, type errors, and configuration problems — before committing to a full deployment cycle.

Use `cargo adk build` as part of your local development workflow and CI pipelines to validate changes early, without needing platform credentials or network access.

## Command Syntax

```bash
cargo adk build [OPTIONS]
```

The command wraps `cargo build --release` by default, targeting your agent project. On success, it reports the build profile, target directory, and binary size.

## Flags and Options

| Flag | Description | Default |
|------|-------------|---------|
| `--manifest-path <PATH>` | Path to the `Cargo.toml` file. Useful when building from outside the project directory. | Current directory |
| `--debug` | Build in debug mode instead of release. Faster compilation but unoptimized binary. | Release mode |

### Examples

```bash
# Build in release mode (default)
cargo adk build

# Build in debug mode for faster iteration
cargo adk build --debug

# Build a project at a specific path
cargo adk build --manifest-path /path/to/my-agent/Cargo.toml
```

## Build vs Deploy

`cargo adk build` and `cargo adk deploy` serve different purposes in the agent development lifecycle:

| Aspect | `cargo adk build` | `cargo adk deploy` |
|--------|-------------------|-------------------|
| **Purpose** | Compile and verify | Compile, bundle, and push to platform |
| **Network required** | No | Yes (platform server) |
| **Authentication** | None | Token required (`--token` or `ADK_DEPLOY_TOKEN`) |
| **Output** | Local binary in `target/` | Deployment bundle uploaded to platform |
| **Build profile** | Release (or debug with `--debug`) | Always release |
| **Secrets handling** | None | Uploads secrets from `.env` |
| **Manifest required** | Only `Cargo.toml` | `Cargo.toml` + `adk-deploy.toml` |
| **Use case** | Local dev, CI checks | Production deployment |

### When to use each

- **`cargo adk build`** — Use during development to verify compilation, in CI pipelines as a gate before merge, or to check that dependency updates don't break your agent.
- **`cargo adk deploy`** — Use when you're ready to ship your agent to the ADK platform. Deploy includes a build step internally (skippable with `--skip-build`).

## Usage Examples

### Successful build

```bash
$ cargo adk build
   Compiling adk-core v0.9.2
   Compiling adk-agent v0.9.2
   Compiling my-agent v0.1.0 (/home/user/my-agent)
    Finished `release` profile [optimized] target(s) in 42.3s
✅ Build successful
   profile: release
   target:  target/release
   binary:  target/release/my-agent (12.4 MB)
```

### Debug build for faster iteration

```bash
$ cargo adk build --debug
   Compiling my-agent v0.1.0 (/home/user/my-agent)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.1s
✅ Build successful
   profile: debug
   target:  target/debug
   binary:  target/debug/my-agent (45.2 MB)
```

### CI pipeline integration

```yaml
# .github/workflows/ci.yml
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install cargo-adk
      - run: cargo adk build
```

## Error Scenarios and Resolution

### Missing dependencies

**Symptom:** Compilation fails with unresolved import errors.

```
error[E0432]: unresolved import `adk_tool`
 --> src/main.rs:3:5
  |
3 | use adk_tool::FunctionTool;
  |     ^^^^^^^^ use of undeclared crate or module `adk_tool`
```

**Resolution:** Add the missing crate to your `Cargo.toml` dependencies:

```toml
[dependencies]
adk-tool = { version = "0.9.2", features = ["mcp"] }
```

### Invalid project structure

**Symptom:** `cargo adk build` cannot find `Cargo.toml`.

```
Error: failed to run cargo build: No such file or directory (os error 2)
```

**Resolution:** Run the command from your project root, or specify the manifest path:

```bash
cargo adk build --manifest-path ./my-agent/Cargo.toml
```

### Feature flag conflicts

**Symptom:** Build fails due to incompatible feature combinations.

```
error: the package `my-agent` depends on `adk-realtime`, with features:
       `openai-webrtc` but `openai-webrtc` is not a feature of `adk-realtime`
```

**Resolution:** Check that your feature flags match the available features for the ADK version you're using. Run `cargo doc -p adk-realtime --open` to see available features, or consult the [crate documentation](https://docs.rs/adk-realtime).

### Outdated lock file

**Symptom:** Version resolution errors after upgrading ADK crates.

```
error: failed to select a version for the requirement `adk-core = "^0.9.2"`
```

**Resolution:** Update your lock file:

```bash
cargo update
cargo adk build
```

### Build environment issues

**Symptom:** Linker errors or missing system libraries.

```
error: linker `cc` not found
```

**Resolution:** Install the required build toolchain for your platform:

```bash
# Ubuntu/Debian
sudo apt install build-essential pkg-config libssl-dev

# macOS
xcode-select --install

# Fedora/RHEL
sudo dnf install gcc openssl-devel
```

### Out of memory during compilation

**Symptom:** Build process killed or panics with allocation failure.

**Resolution:** Reduce parallelism or use a machine with more RAM:

```bash
# Limit parallel compilation jobs
CARGO_BUILD_JOBS=2 cargo adk build

# Or set in .cargo/config.toml
# [build]
# jobs = 2
```
