import { resolveCoreSource, coreFile } from './core-source.mjs';
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { postgresSources, validateCachePrefixMeasurement, validateCapturedPostgres, validatePostgresMeasurement } from "./performance-eval-artifact.mjs";

const workspace = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const core = resolveCoreSource({ allowLocal: false });
const reference = JSON.parse(readFileSync(resolve(workspace, "docs/eval/fixtures/performance-repair-v1.json"), "utf8"));
assert.equal(reference.schema, "centaeris.performance_repair_eval.v1");

const args = process.argv.slice(2);
const postgresArtifactIndex = args.indexOf("--postgres-artifact");
assert(!(args.includes("--postgres") && postgresArtifactIndex !== -1), "choose a fresh PostgreSQL run or its captured artifact, not both");
let capturedPostgres;
if (postgresArtifactIndex !== -1) {
  assert(args[postgresArtifactIndex + 1], "--postgres-artifact requires a path");
  const currentHashes = Object.fromEntries(postgresSources.map((source) => [source,
    createHash("sha256").update(readFileSync(source.startsWith("core/") ? coreFile(source.slice(5)) : resolve(workspace, source))).digest("hex")]));
  capturedPostgres = validateCapturedPostgres(JSON.parse(readFileSync(resolve(args[postgresArtifactIndex + 1]), "utf8")), currentHashes);
}

function gate(cwd, packageName, filter, extra = []) {
  const args = ["test", "--locked", "-p", packageName, filter, "--", "--nocapture", "--test-threads=1", ...extra];
  process.stderr.write(`Running current gate: cargo ${args.join(" ")}\n`);
  const result = spawnSync("cargo", args, { cwd, env: { ...process.env, CARGO_BUILD_JOBS: "1", CARGO_TARGET_DIR: process.env.CARGO_TARGET_DIR ?? resolve(workspace, "target") }, encoding: "utf8", maxBuffer: 32 * 1024 * 1024 });
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  if (result.error || result.status !== 0) {
    process.stderr.write(output);
    throw result.error ?? new Error(`Gate failed with exit ${result.status}: ${filter}`);
  }
  return output;
}

function observationArtifacts(output) {
  return [...output.matchAll(/RUNTIME_01_ARTIFACT (\{[^\r\n]+\})/g)].map((match) => JSON.parse(match[1]));
}

const toolOutput = gate(core, "centaeris-core", "runtime::tests::query_loop_tool_continuation_twenty_round_deterministic_eval", ["--exact"]);
const tool = toolOutput.match(/CORE_01_20_ROUND compliant=(\d+) duplicateToolRounds=(\d+)/);
assert(tool, "current tool continuation gate must emit its measurements");
const toolContinuation = { source: "currentFocusedGate", executedThisInvocation: true, measurementKind: "deterministicPromptProjection", rounds: 20, tailSafeRounds: Number(tool[1]), userRoleTailSignals: Number(tool[2]) };
assert.equal(toolContinuation.tailSafeRounds, 20);
assert.equal(toolContinuation.userRoleTailSignals, 0);

const cacheOutput = gate(core, "centaeris-core", "runtime::tests::query_loop_cache_prefix_stays_reusable_across_legacy_80_message_threshold", ["--exact"]);
const cache = cacheOutput.match(/query_loop_cache_80_threshold_metrics\r?\n(\{[^\r\n]+\})/);
assert(cache, "current Core cache gate must emit structured measurements");
const cachePrefix = validateCachePrefixMeasurement(JSON.parse(cache[1]));

const local = [
  ...observationArtifacts(gate(core, "centaeris-runtime", "message_log::tests::local_model_request_cas_round_trips_rewrites_copies_and_deletes_with_quantified_growth", ["--exact"])),
  ...observationArtifacts(gate(core, "centaeris-runtime", "message_log::observation_cas::tests::observation_manifest_growth_is_linear_with_early_changes_through_4095_observations", ["--exact", "--ignored"])),
];
const postgres = [
  ...observationArtifacts(gate(workspace, "runtime_server", "postgres_store::runtime::tests::twenty_round_model_request_storage_amplification_is_quantified", ["--exact"])),
  ...observationArtifacts(gate(workspace, "runtime_server", "postgres_store::runtime::tests::model_observation_manifest_growth_and_validation_through_4095_observations", ["--exact", "--ignored"])),
];
const modelObservations = [...local, ...postgres].map((artifact) => ({ ...artifact, executedThisInvocation: true }));
assert.deepEqual(modelObservations.map((artifact) => artifact.gate).sort(),
  ["local_storage_lifecycle_20_round", "local_manifest_linear_growth", "postgres_storage_projection_20_round", "postgres_manifest_linear_growth"].sort(),
  "current gates must emit exactly the four known artifacts");
if (args.includes("--postgres")) {
  assert.equal(process.env.CENTAERIS_ALLOW_POSTGRES_TEST_RESET, "1", "--postgres requires an explicitly authorized disposable test database");
  assert(process.env.CENTAERIS_TEST_POSTGRES_URL, "--postgres requires CENTAERIS_TEST_POSTGRES_URL");
  const databaseArtifacts = observationArtifacts(gate(workspace, "runtime_server", "postgres_store::integration_tests::postgres_model_request_batch_deduplicates_and_hydrates_observations", ["--exact", "--ignored"]));
  assert.equal(databaseArtifacts.length, 1, "actual PostgreSQL gate must emit exactly one measured artifact");
  validatePostgresMeasurement(databaseArtifacts[0]);
  modelObservations.push(...databaseArtifacts.map((artifact) => ({ ...artifact, executedThisInvocation: true })));
}
if (capturedPostgres) {
  const captured = capturedPostgres;
  modelObservations.push({ ...captured.measurement, executedThisInvocation: false, completedAt: captured.completedAt,
    sourceHashes: captured.sourceHashes, source: "validatedSameSourceCapturedIntegrationGate" });
}

for (const artifact of modelObservations.filter((item) => item.curve)) {
  assert.deepEqual(artifact.curve.map((point) => point.rounds), [20, 81, 512, 2046]);
  for (const point of artifact.curve) {
    assert.equal(point.manifestNodes, point.rounds);
    assert.equal(point.manifestRefs, point.rounds * 3 + 2);
    assert.equal(point.uniqueContents, point.rounds * 3 + 2);
    if (point.submittedCasContentBytes !== undefined) {
      assert.equal(point.submittedCasContentBytes, point.contentBytes ?? point.casBytes - point.manifestBytes);
    }
  }
  const small = artifact.curve[1];
  const large = artifact.curve.at(-1);
  assert.equal(large.observationCount, 4095);
  const payloadBytes = (point) => point.physicalPayloadBytes ?? point.physicalBytes;
  assert(payloadBytes(large) * small.rounds * 100 < payloadBytes(small) * large.rounds * 115,
    `${artifact.gate}: payload bytes per round must remain within 15% as integer widths grow`);
}

const report = {
  schema: reference.schema,
  generatedAt: new Date().toISOString(),
  measurementPolicy: "Core/local/serialization findings come from gates executed by this invocation. Actual PostgreSQL is either run now or explicitly loaded from a captured passing suite with matching source hashes and its own completion time. Serialized projections are not database I/O. No SQL statement/round-trip count, provider hit rate, or latency percentile is inferred.",
  toolContinuation,
  cachePrefix: { source: "currentFocusedGate", executedThisInvocation: true, ...cachePrefix, providerCachedInputTokens: null, providerMeasurementSampleCount: 0 },
  modelObservations,
  auditedAndSyntheticReference: {
    executedThisInvocation: false,
    note: "Reference-only inputs, not re-measured by this invocation. Re-run their separate gates before release; never mix them with observation storage or report these fixtures as production latency.",
    toolBaseline: reference.toolContinuation.baseline,
    mcpStartup: reference.mcpStartup,
    recoverySnapshots: reference.recoverySnapshots,
    workerScheduling: reference.workerScheduling,
    browserRender: reference.browserRender,
  },
};
const json = `${JSON.stringify(report, null, 2)}\n`;
const outputIndex = args.indexOf("--output");
if (outputIndex !== -1) {
  assert(args[outputIndex + 1], "--output requires an artifact path");
  const outputPath = resolve(args[outputIndex + 1]);
  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, json);
}
process.stdout.write(json);
