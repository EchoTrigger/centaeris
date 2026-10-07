import assert from "node:assert/strict";
import test from "node:test";
import { postgresSources, validateCachePrefixMeasurement, validateCapturedPostgres } from "./performance-eval-artifact.mjs";

// Deliberately synthetic inputs exercise validation only; these are not measurements.
test("cache-prefix evidence rejects malformed boundaries, aliases and provider claims", () => {
  const valid = {
    measurementKind: "syntheticPreparedPromptPrefixes", providerCacheHitRateMeasured: false,
    legacy80Cap: { commonPrefixMessages: 0, commonPrefixBytes: 0, commonPrefixTokens: 0, retainedMessagesAt80: 80, retainedMessagesAt81: 80, compactionCount: 0 },
    tokenBudgetOnly: { commonPrefixMessages: 80, commonPrefixBytes: 1, commonPrefixTokens: 1, retainedMessagesAt80: 80, retainedMessagesAt81: 81, compactionCount: 0 },
    messageCountBoundaries: [80, 4096, 10000].map((messageCountBefore) => ({
      messageCountBefore, messageCountAfter: messageCountBefore + 1,
      retainedMessagesBefore: messageCountBefore, retainedMessagesAfter: messageCountBefore + 1,
      commonPrefixMessages: messageCountBefore, commonPrefixBytes: 1, commonPrefixTokens: 1,
      compactionCount: 0, requestBuildElapsedMs: 0,
    })),
  };
  assert.equal(validateCachePrefixMeasurement(valid), valid);
  const slow = structuredClone(valid);
  slow.messageCountBoundaries[2].requestBuildElapsedMs = 1e9;
  assert.equal(validateCachePrefixMeasurement(slow), slow, "timings have no absolute SLO");
  const mutations = [
    (value) => { value.tokenFirst4096Ceiling = value.tokenBudgetOnly; delete value.tokenBudgetOnly; },
    (value) => { value.extra = true; },
    (value) => { delete value.messageCountBoundaries; },
    (value) => { value.messageCountBoundaries = null; },
    (value) => { value.messageCountBoundaries = false; },
    (value) => { value.messageCountBoundaries.pop(); },
    (value) => { value.messageCountBoundaries[2].messageCountBefore = 4096; },
    (value) => { value.messageCountBoundaries[0].messageCountBefore = "80"; },
    (value) => { value.messageCountBoundaries[1].messageCountAfter = 4096; },
    (value) => { value.messageCountBoundaries[1].retainedMessagesAfter = 4096; },
    (value) => { value.messageCountBoundaries[1].retainedMessagesBefore = 4095; },
    (value) => { value.messageCountBoundaries[2].commonPrefixMessages = 4096; },
    (value) => { value.messageCountBoundaries[0].compactionCount = 1; },
    (value) => { value.messageCountBoundaries[0].compactionCount = false; },
    (value) => { value.messageCountBoundaries[0].commonPrefixBytes = 0; },
    (value) => { value.messageCountBoundaries[0].commonPrefixBytes = true; },
    (value) => { value.messageCountBoundaries[0].commonPrefixTokens = Infinity; },
    (value) => { value.messageCountBoundaries[0].commonPrefixTokens = "1"; },
    (value) => { value.messageCountBoundaries[0].requestBuildElapsedMs = -1; },
    (value) => { value.messageCountBoundaries[0].requestBuildElapsedMs = NaN; },
    (value) => { value.messageCountBoundaries[0].requestBuildElapsedMs = "1"; },
    (value) => { value.messageCountBoundaries[0].requestBuildElapsedMs = false; },
    (value) => { value.messageCountBoundaries[0].extra = 1; },
    (value) => { value.tokenBudgetOnly.retainedMessagesAt81 = 80; },
    (value) => { value.tokenBudgetOnly.extra = 1; },
    (value) => { value.providerCacheHitRateMeasured = true; },
    (value) => { value.providerCacheHitRateMeasured = 0; },
    (value) => { value.measurementKind = "providerMeasurements"; },
  ];
  for (const [index, mutate] of mutations.entries()) {
    const invalid = structuredClone(valid);
    mutate(invalid);
    assert.throws(() => validateCachePrefixMeasurement(invalid), `cache mutation ${index} must fail`);
  }
});

const hashes = Object.fromEntries(postgresSources.map((source) => [source, "0".repeat(64)]));
function fixture() {
  return {
    schema: "centaeris.performance_repair_pg_gate.v1", status: "passed",
    suite: "postgres_store::integration_tests", exitCode: 0, passedTests: 7, failedTests: 0,
    completedAt: "2026-08-31T00:00:00.000Z", sourceHashes: { ...hashes },
    measurement: {
      gate: "postgres_manifest_database_growth",
      measurement: "actual_postgres_rows_and_payload_bytes_excludes_indexes_mvcc_relation_overhead",
      workload: "early_runtime_context_replaced_and_two_tail_observations_appended_per_round",
      curve: [20, 81, 512, 2046].map((rounds) => ({
        rounds, observationCount: rounds * 2 + 3, manifestNodes: rounds,
        manifestRefs: rounds * 3 + 2, uniqueContents: rounds * 3 + 2,
        manifestBytes: rounds * 150, contentBytes: (rounds * 3 + 2) * 100,
        eventRootBytes: rounds * 120, commitPayloadBytes: rounds * 50,
        commitRows: rounds, physicalRows: rounds * 6 + 2,
        physicalPayloadBytes: rounds * 620 + 200,
      })),
    },
  };
}

test("captured PostgreSQL evidence rejects missing, mislabeled, stale or inconsistent measurements", () => {
  const valid = fixture();
  assert.equal(validateCapturedPostgres(valid, hashes), valid);
  const mutations = [
    (value) => { value.measurement.curve = null; },
    (value) => { value.measurement.curve = false; },
    (value) => { value.measurement.curve = []; },
    (value) => { value.measurement.curve.pop(); },
    (value) => { delete value.measurement.curve[0].contentBytes; },
    (value) => { value.measurement.measurement = "unmeasured_fixture"; },
    (value) => { value.measurement.workload = "different_workload"; },
    (value) => { value.measurement.gate = "unknown_gate"; },
    (value) => { value.completedAt = 0; },
    (value) => { value.completedAt = "2026-08-31"; },
    (value) => { delete value.completedAt; },
    (value) => { value.failedTests = 1; },
    (value) => { value.passedTests = 6; },
    (value) => { value.status = "failed"; },
    (value) => { value.exitCode = 1; },
    (value) => { value.sourceHashes[postgresSources[0]] = "1".repeat(64); },
    (value) => { delete value.sourceHashes[postgresSources[0]]; },
    (value) => { value.measurement.curve[0].contentBytes = -1; },
    (value) => { value.measurement.curve[0].contentBytes = 1.5; },
    (value) => { value.measurement.curve[0].contentBytes = Number.MAX_SAFE_INTEGER + 1; },
    (value) => { value.measurement.curve[0].physicalPayloadBytes += 1; },
    (value) => { value.measurement.curve[0].physicalRows += 1; },
    (value) => { value.measurement.curve[0].manifestRefs += 1; },
    (value) => { value.measurement.curve[0].extra = 1; },
    (value) => { value.extra = true; },
  ];
  for (const [index, mutate] of mutations.entries()) {
    const invalid = fixture();
    mutate(invalid);
    assert.throws(() => validateCapturedPostgres(invalid, hashes), `mutation ${index} must fail`);
  }
});
