# cargo adk build

## Overview

The `cargo adk build` command compiles your ADK-Rust agent project without deploying it. This gives you a fast way to verify that your agent compiles correctly — catching dependency issues, type errors, and configuration problems — before committing to a full deployment cycle.

Use `cargo adk build` as part of your local development workflow and CI pipelines to validate changes early, without platform credentials or a connection to the ADK platform.

## Command Syntax

```bash
cargo adk build [OPTIONS]
```

The command runs `cargo build --release` by default (`cargo build` with `--debug`), targeting your agent project. On success, it reports the build profile, the target directory, and the size of each executable in it. The target directory reported is `target/<profile>` next to the manifest; a `CARGO_TARGET_DIR` or workspace target directory is not reflected.

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
| **Platform server** | Not contacted | Required (`--server`, default `http://127.0.0.1:8090`) |
| **Authentication** | None | `--token` or `ADK_DEPLOY_TOKEN`, cached credentials, or an ephemeral login |
| **Output** | Local binary in `target/` | Deployment bundle uploaded to platform |
| **Build profile** | Release (or debug with `--debug`) | Always release (`--skip-build` uses the existing binary) |
| **Secrets handling** | None | Uploads the secrets `adk-deploy.toml` declares, read from `.env` |
| **Manifest required** | Only `Cargo.toml` | `Cargo.toml` + `adk-deploy.toml` |
| **Use case** | Local dev, CI checks | Production deployment |

### When to use each

- **`cargo adk build`** — Use during development to verify compilation, in CI pipelines as a gate before merge, or to check that dependency updates don't break your agent.
- **`cargo adk deploy`** — Use when you're ready to ship your agent to the ADK platform. Deploy includes a build step internally (skippable with `--skip-build`). `--dry-run` validates without pushing, but still authenticates against the platform server.

## Usage Examples

### Successful build

```bash
$ cargo adk build
   Compiling adk-core v3.0.0
   Compiling adk-agent v3.0.0
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
      - uses: actions/checkout@v7
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
adk-tool = { version = "3.0.0", features = ["mcp"] }
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

### Unknown feature flags

**Symptom:** Build fails because a dependency does not have a requested feature.

```
error: failed to select a version for `adk-rust`.
    ... required by package `my-agent v0.1.0 (/home/user/my-agent)`
versions that meet the requirements `^3.0.0` are: 3.0.0

package `my-agent` depends on `adk-rust` with feature `webrtc` but `adk-rust` does not have that feature.
```

**Resolution:** Check that your feature flags match the available features for the ADK version you're using. Cargo lists the available features after the error; the umbrella crate's features are described in the [crate documentation](https://docs.rs/adk-rust).

### Outdated lock file

**Symptom:** Version resolution errors after upgrading ADK crates.

```
error: failed to select a version for the requirement `adk-core = "^3.0.0"`
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
