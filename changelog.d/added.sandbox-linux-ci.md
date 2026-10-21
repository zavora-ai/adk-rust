- **bubblewrap enforcement is proven in CI** (`adk-sandbox`, `adk-harness`): the `Sandbox
  Linux` workflow installs bubblewrap on `ubuntu-24.04`, lifts the AppArmor restriction on
  unprivileged user namespaces, and runs the sandbox and harness shell suites with
  `ADK_SANDBOX_REQUIRE_ENFORCER=1`, under which a test that cannot start the OS sandbox fails
  instead of skipping. It runs on every push to `main`, on a pull request that touches the
  sandbox or the harness shell, and on demand.
