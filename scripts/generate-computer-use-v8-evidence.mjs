#!/usr/bin/env node

import { createHash } from 'node:crypto'
import { execFile } from 'node:child_process'
import { readFile, rename, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { promisify } from 'node:util'
import process from 'node:process'

const exec = promisify(execFile)
const root = resolve(new URL('..', import.meta.url).pathname)
const valueFlag = name => { const index = process.argv.indexOf(name); return index < 0 ? undefined : process.argv[index + 1] }
const subjectVersion = valueFlag('--subject-version')
if (!subjectVersion) throw new TypeError('--subject-version is required and must match computer-use-mcp')
const commands = [
  ['cargo', ['test', '-p', 'adk-computer-use']],
  ['cargo', ['test', '-p', 'adk-tool', '--features', 'mcp', '--lib', 'mcp_result_preserves_structured_text_and_image_content']],
]
let output = ''
for (const [command, args] of commands) {
  const result = await exec(command, args, { cwd: root, maxBuffer: 32 * 1024 * 1024 })
  output += `${result.stdout}\n${result.stderr}\n`
}
const required = {
  'graph.parallel_one_executor': 'graph_parallelizes_observation_and_has_one_executor_effect',
  'graph.approval_digest_binding': 'approval_resume_rejects_a_changed_action_digest_before_mutation',
  'graph.policy_digest_binding': 'approval_resume_rejects_a_changed_policy_digest_before_mutation',
  'graph.pre_effect_crash': 'graph_retry_after_pre_effect_crash_executes_exactly_once',
  'graph.post_commit_crash': 'graph_retry_after_post_commit_crash_does_not_duplicate_mutation',
  'auth.verified_identity': 'verified_identity_must_match_v8_principal_and_tenant',
  'eval.no_duplicate_mutation': 'evaluator_rejects_duplicate_unleased_unverified_mutation',
  'mcp.multimodal_image': 'mcp_result_preserves_structured_text_and_image_content',
  'wire.postcondition_roundtrip': 'types_round_trip_canonical_v8_fixtures',
}
for (const [assertion, testName] of Object.entries(required)) {
  if (!output.includes(testName)) throw new Error(`required evaluation test did not run: ${assertion} (${testName})`)
}
const sourcePaths = [
  'adk-computer-use/Cargo.toml',
  'adk-computer-use/fixtures/v8/adk-evaluation-receipt.schema.json',
  'adk-computer-use/fixtures/v8/action-preview.json',
  'adk-computer-use/fixtures/v8/action-postcondition.schema.json',
  'adk-computer-use/fixtures/v8/session-deletion.json',
  'adk-computer-use/src/auth.rs', 'adk-computer-use/src/eval.rs', 'adk-computer-use/src/graph.rs',
  'adk-computer-use/src/contracts.rs', 'adk-computer-use/src/lib.rs', 'adk-computer-use/src/mcp_runtime.rs',
  'adk-computer-use/tests/evaluation.rs', 'adk-computer-use/tests/reference_graph.rs',
  'adk-computer-use/tests/wire_contracts.rs', 'adk-tool/src/mcp/toolset.rs',
  'scripts/generate-computer-use-v8-evidence.mjs',
]
const digest = bytes => `sha256:${createHash('sha256').update(bytes).digest('hex')}`
const sourceHash = createHash('sha256')
const sources = []
for (const path of sourcePaths.sort()) {
  const bytes = await readFile(resolve(root, path))
  sourceHash.update(path); sourceHash.update('\0'); sourceHash.update(bytes); sourceHash.update('\0')
  sources.push({ path, digest: digest(bytes) })
}
const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(',')}]` : value && typeof value === 'object'
  ? `{${Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([key, entry]) => `${JSON.stringify(key)}:${canonical(entry)}`).join(',')}}`
  : JSON.stringify(value)
const receipt = {
  schemaVersion: 1, protocol: 'adk-rust-computer-use-v8-evaluation', subjectVersion,
  generatedAt: new Date().toISOString(), commands: commands.map(([command, args]) => `${command} ${args.join(' ')}`),
  assertions: Object.keys(required),
  claims: { testsPassed: true, authBound: true, multimodalEvidence: true, duplicateMutations: 0, crashPointsCovered: 2, testCount: [...output.matchAll(/test .+ \.\.\. ok/g)].length },
  sources, sourceDigest: `sha256:${sourceHash.digest('hex')}`, outputDigest: digest(output), receiptDigest: '',
}
receipt.receiptDigest = digest(canonical(receipt))
const rendered = `${JSON.stringify(receipt, null, 2)}\n`
const outputPath = valueFlag('--output')
if (outputPath) {
  const path = resolve(process.cwd(), outputPath); const temporary = `${path}.${process.pid}.tmp`
  await writeFile(temporary, rendered); await rename(temporary, path)
}
const mirrorOutputPath = valueFlag('--mirror-output')
if (mirrorOutputPath) {
  const path = resolve(process.cwd(), mirrorOutputPath); const temporary = `${path}.${process.pid}.tmp`
  await writeFile(temporary, rendered); await rename(temporary, path)
}
console.log(JSON.stringify(receipt, null, process.argv.includes('--compact') ? 0 : 2))
