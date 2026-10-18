# Artifacts, Eval, and Memory Validation

Validates artifact versioning and naming, evaluation verdicts, and keyword memory search on the
stack an application uses: an `LlmAgent` and `Runner` on a live model saving and loading artifacts
through tools, an `adk-eval` run graded by a live LLM judge, and memory recall through a tool over
`MemoryServiceAdapter`. Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence, and the
process exits non-zero when any scenario fails.

Faults a live model cannot produce on demand use scripted pieces, and each row is marked:

- **live** — the scenario drives a live model (and, for eval, a live judge).
- **scripted** — a direct concurrent harness, a writer process killed mid-save, hand-written
  directories in the earlier on-disk layout, or direct service calls.

The GCS scenarios point the real `GcsArtifactService` at a scripted GCS JSON API on 127.0.0.1 that
honours the `ifGenerationMatch=0` upload precondition the way GCS does (HTTP 412 for an existing
object).

## Scenarios

| Scenario | Finding | Mode | Passes when |
|----------|---------|------|-------------|
| `artifact-parallel-saves-file` | SS8 | live | The model makes four overlapping `save_note` calls in one response on `FileArtifactService`: four distinct versions are returned and stored, and each loads its own content |
| `artifact-parallel-saves-gcs` | SS8 | live | The same on `GcsArtifactService` against the scripted bucket |
| `artifact-concurrent-saves` | SS8 | scripted | 16 simultaneous saves per round on the in-memory, file, and GCS backends (200, 20, and 5 rounds): no version is lost or handed out twice |
| `artifact-name-collision` | SS10 | live | The model saves `alpha` as `a:b` and `beta` as `a__b` on `FileArtifactService`: each name loads its own text and both are listed |
| `artifact-legacy-layout` | SS10 | scripted | Directories written in the earlier layout (`:` as `__`, user scope under `_user_scoped_`) load, list under their original names, and take a new version as version 3 |
| `artifact-crash-mid-write` | SS18 | scripted | A child process saving a 160 MiB artifact is killed as soon as its file appears: the latest version still loads completely |
| `artifact-reserved-session` | SS21 | live + scripted | The model saves `draft.txt` in session `user` (in-memory, live), and the same save runs on the file backend (sessions `user` and `_user_scoped_`) and GCS (session `user`): no other session lists `draft.txt` |
| `eval-live-judged-case` | SS9 | live | A case the live agent answers and the live judge grades with `semantic_match_score` passes |
| `eval-zero-cases` | SS9 | live | A test file with no cases is rejected or does not report `all_passed()` |
| `eval-zero-turns` | SS9 | live | A case with no turns is rejected or fails |
| `eval-no-criteria` | SS9 | live | An evaluator with no criteria is rejected or fails the case |
| `eval-nothing-judged` | SS9 | live | `semantic_match_score` on a turn without an expected response does not pass the case |
| `eval-zero-concurrency` | SS9 | live | `evaluate_cases_parallel` with concurrency 0 returns an error within 20 s |
| `memory-punctuation-recall` | SS20 | live | The model recalls the hometown stored as `Hometown: Nairobi.` through a `recall` tool and answers `Nairobi` |
| `memory-ranked-top-k` | SS20 | scripted | In 20 stores of 31 entries matching `rust`, the one entry matching all three query words ranks first |
| `memory-image-only-entry` | SS20 | scripted | An image-only memory entry is stored and returned by `list_recent` |

## Results

`gemini-3.5-flash-lite` for the agent and the judge. `origin/main` at `c1a4cfa3`; the branch is
`fix/artifacts-eval-memory`.

| Scenario | main | branch |
|----------|------|--------|
| `artifact-parallel-saves-file` | FAIL: versions `{1}` returned, `[1]` stored | PASS: `{1, 2, 3, 4}` returned and stored |
| `artifact-parallel-saves-gcs` | FAIL: versions `{0}` returned, `[0]` stored | PASS: `{0, 1, 2, 3}` returned and stored |
| `artifact-concurrent-saves` | FAIL: 2090 of 3200 in-memory, 288 of 320 file, and 75 of 80 GCS versions lost (counts vary by run) | PASS: none lost |
| `artifact-name-collision` | FAIL: `a:b` loads `beta`; list `["a:b"]` | PASS: `alpha` and `beta`; list `["a:b", "a__b"]` |
| `artifact-legacy-layout` | PASS | PASS |
| `artifact-crash-mid-write` | FAIL: the truncated latest version fails to parse | PASS: the earlier version loads |
| `artifact-reserved-session` | FAIL: in-memory, file (`_user_scoped_`), and GCS list `draft.txt` in another session | PASS: `user` rejected; nothing leaks |
| `eval-live-judged-case` | PASS | PASS |
| `eval-zero-cases` | FAIL: `all_passed()` with 0 of 0 cases | PASS: `ConfigError` |
| `eval-zero-turns` | FAIL: passes with no scores | PASS: `ConfigError` |
| `eval-no-criteria` | FAIL: passes with no scores | PASS: `ConfigError` |
| `eval-nothing-judged` | FAIL: passes with no scores | PASS: fails with `not_evaluated` |
| `eval-zero-concurrency` | FAIL: had not returned after 20 s | PASS: `ConfigError` |
| `memory-punctuation-recall` | FAIL: `recall("hometown")` returns nothing | PASS: returns `Hometown: Nairobi.` |
| `memory-ranked-top-k` | FAIL: ranked first in 0 of 20 stores | PASS: 20 of 20 |
| `memory-image-only-entry` | FAIL: 1 entry listed | PASS: image and text listed |

The live rows fail the same way on main with `gpt-5.6-terra` and `claude-sonnet-4-6`, and every
scenario passes on the branch with all three providers.

## Running

```bash
cp examples/artifacts_eval_memory/.env.example examples/artifacts_eval_memory/.env  # add keys
cargo run --manifest-path examples/artifacts_eval_memory/Cargo.toml -- --provider gemini
```

| Flag | Values | Default |
|------|--------|---------|
| `--provider` | `gemini` (`gemini-3.5-flash-lite`), `openai` (`gpt-5.6-terra`), `anthropic` (`claude-sonnet-4-6`) | `gemini` |
| `--judge` | the same values | the `--provider` value |
| `--scenario` | a comma-separated list of scenario names, or `all` | `all` |

Live scenarios report `SKIP` when the provider's API key is not set; scripted scenarios always run.
Set `ADK_ENV_FILE` to load keys from another dotenv file; its values win over the environment.
