# harness_eval

The `adk-harness` eval suite (spec 3.1 §7.13) on a scripted or a live model, checked
against a `BaselineStore` baseline. The cases live in
`adk-harness/tests/eval/cases.rs`, which this example includes, so the scripted CI test
and the live nightly run evaluate the same scenarios.

| Case | Mode | Pass condition |
|---|---|---|
| `fix_failing_test` | Auto | `sh test.sh` exits 0 after the run |
| `plan_mode_writes_nothing` | Plan | the workspace is byte-for-byte unchanged |
| `goal_until` | Auto | `counter.txt` reaches 2 within 4 turns |
| `capability_mistyped_command` | Ask | `/aprovals` is answered with `/approvals` |
| `capability_resume` | Ask | "Can I pick this up later?" is answered with `--continue` or `--session` |
| `capability_deny_rule` | Ask | "Stop it editing migrations/" is answered with `rules add`, `edit_file` and `--deny` |

The three `capability_*` cases are the rubric case of spec 3.1 §7.25: the harness runs with
the `adk-rust` commands as its surface, and the answer passes when it names the commands the
`<harness>` block lists and the workspace is unchanged. They join the `passed` metric under
their own ids; a live baseline compares only the cases it lists.

## Run

```bash
# Scripted model, against adk-harness/tests/eval/baseline.scripted.json
cargo run --manifest-path examples/harness_eval/Cargo.toml

# Live models, against baseline.live.<provider>.json (the `passed` metric)
cargo run --manifest-path examples/harness_eval/Cargo.toml -- --live openai      # gpt-5.6-terra
cargo run --manifest-path examples/harness_eval/Cargo.toml -- --live anthropic   # claude-sonnet-5-5
cargo run --manifest-path examples/harness_eval/Cargo.toml -- --live gemini      # gemini-3.5-flash-lite

# Refresh a live baseline
cargo run --manifest-path examples/harness_eval/Cargo.toml -- --live openai --save-baseline
```

Keys come from the environment or `.env` (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
`GOOGLE_API_KEY`); `--env-file <path>` loads a file that overrides the environment. The
process exits non-zero on a regression; the nightly `harness-eval-live` job runs it for
each provider whose key is configured.
