import assert from "node:assert/strict";

export function validateCachePrefixMeasurement(cachePrefix) {
  assert.deepEqual(Object.keys(cachePrefix).sort(), ["legacy80Cap", "measurementKind", "messageCountBoundaries", "providerCacheHitRateMeasured", "tokenBudgetOnly"]);
  assert.equal(cachePrefix.measurementKind, "syntheticPreparedPromptPrefixes");
  assert.equal(cachePrefix.providerCacheHitRateMeasured, false);
  assert.deepEqual(cachePrefix.legacy80Cap, {
    commonPrefixMessages: 0, commonPrefixBytes: 0, commonPrefixTokens: 0,
    retainedMessagesAt80: 80, retainedMessagesAt81: 80, compactionCount: 0,
  });
  assert(Array.isArray(cachePrefix.messageCountBoundaries), "current Core cache gate must emit boundary measurements");
  assert.deepEqual(cachePrefix.messageCountBoundaries.map((point) => point.messageCountBefore), [80, 4096, 10000]);
  for (const point of cachePrefix.messageCountBoundaries) {
    assert.deepEqual(Object.keys(point).sort(), ["commonPrefixBytes", "commonPrefixMessages", "commonPrefixTokens", "compactionCount", "messageCountAfter", "messageCountBefore", "requestBuildElapsedMs", "retainedMessagesAfter", "retainedMessagesBefore"]);
    assert.equal(point.messageCountAfter, point.messageCountBefore + 1);
    assert.equal(point.retainedMessagesBefore, point.messageCountBefore);
    assert.equal(point.retainedMessagesAfter, point.messageCountAfter);
    assert.equal(point.commonPrefixMessages, point.messageCountBefore);
    assert.equal(point.compactionCount, 0);
    for (const name of ["commonPrefixBytes", "commonPrefixTokens"]) {
      assert(Number.isSafeInteger(point[name]) && point[name] > 0, `${name} must be a positive safe integer`);
    }
    assert(Number.isFinite(point.requestBuildElapsedMs) && point.requestBuildElapsedMs >= 0,
      "request-build timing must be finite and nonnegative; it is diagnostic, not a latency SLO");
  }
  const firstBoundary = cachePrefix.messageCountBoundaries[0];
  assert.deepEqual(cachePrefix.tokenBudgetOnly, {
    commonPrefixMessages: firstBoundary.commonPrefixMessages,
    commonPrefixBytes: firstBoundary.commonPrefixBytes,
    commonPrefixTokens: firstBoundary.commonPrefixTokens,
    retainedMessagesAt80: firstBoundary.retainedMessagesBefore,
    retainedMessagesAt81: firstBoundary.retainedMessagesAfter,
    compactionCount: firstBoundary.compactionCount,
  });
  return cachePrefix;
}

export const postgresSources = [
  "Cargo.lock",
  "Cargo.toml",
  "core/Cargo.lock",
  "core/Cargo.toml",
  "core/packages/core/Cargo.toml",
  "core/packages/core/src/extension/composition.rs",
  "core/packages/core/src/runtime/canonical_json.rs",
  "core/packages/core/src/runtime/driver.rs",
  "core/packages/core/src/session.rs",
  "core/packages/core/src/session/wire.rs",
  "packages/runtime_server/Cargo.toml",
  "packages/runtime_server/src/postgres_store.rs",
  "packages/runtime_server/src/postgres_store/external_context.rs",
  "packages/runtime_server/src/postgres_store/integration_tests.rs",
  "packages/runtime_server/src/postgres_store/reliability.rs",
  "packages/runtime_server/src/postgres_store/runtime.rs",
  "packages/runtime_server/src/postgres_store/schema.rs",
  "packages/runtime_server/src/postgres_store/transactions.rs",
  "packages/runtime_server/src/postgres_store/turn_supplement.rs",
];

export function validatePostgresMeasurement(measurement) {
  assert.deepEqual(Object.keys(measurement).sort(), ["curve", "gate", "measurement", "workload"]);
  assert.equal(measurement.gate, "postgres_manifest_database_growth");
  assert.equal(measurement.measurement, "actual_postgres_rows_and_payload_bytes_excludes_indexes_mvcc_relation_overhead");
  assert.equal(measurement.workload, "early_runtime_context_replaced_and_two_tail_observations_appended_per_round");
  assert(Array.isArray(measurement.curve), "actual PostgreSQL curve must be an array");
  assert.deepEqual(measurement.curve.map((point) => point.rounds), [20, 81, 512, 2046]);
  for (const point of measurement.curve) {
    assert.deepEqual(Object.keys(point).sort(), ["commitPayloadBytes", "commitRows", "contentBytes", "eventRootBytes", "manifestBytes", "manifestNodes", "manifestRefs", "observationCount", "physicalPayloadBytes", "physicalRows", "rounds", "uniqueContents"]);
    for (const [name, value] of Object.entries(point)) {
      assert(Number.isSafeInteger(value) && value > 0, `actual PostgreSQL ${name} must be a positive safe integer`);
    }
    assert.equal(point.manifestNodes, point.rounds);
    assert.equal(point.manifestRefs, point.rounds * 3 + 2);
    assert.equal(point.uniqueContents, point.rounds * 3 + 2);
    assert.equal(point.observationCount, point.rounds * 2 + 3);
    assert.equal(point.commitRows, point.rounds);
    assert.equal(point.physicalRows, point.manifestNodes + point.uniqueContents + point.commitRows * 2);
    assert.equal(point.physicalPayloadBytes, point.manifestBytes + point.contentBytes + point.eventRootBytes + point.commitPayloadBytes);
  }
  const small = measurement.curve[1];
  const large = measurement.curve[3];
  assert(large.physicalPayloadBytes / large.rounds < small.physicalPayloadBytes / small.rounds * 1.15,
    "actual PostgreSQL payload bytes per round must remain within the 15% growth gate");
  return measurement;
}

export function validateCapturedPostgres(captured, currentHashes) {
  assert.deepEqual(Object.keys(captured).sort(), ["completedAt", "exitCode", "failedTests", "measurement", "passedTests", "schema", "sourceHashes", "status", "suite"]);
  assert.equal(captured.schema, "centaeris.performance_repair_pg_gate.v1");
  assert.equal(captured.status, "passed");
  assert.equal(captured.suite, "postgres_store::integration_tests");
  assert.equal(captured.exitCode, 0);
  assert.equal(captured.failedTests, 0);
  assert.equal(captured.passedTests, 7, "captured PostgreSQL suite must have passed all integration gates");
  assert.equal(typeof captured.completedAt, "string", "captured PostgreSQL completion time must be an ISO string");
  assert(Number.isFinite(Date.parse(captured.completedAt)), "captured PostgreSQL completion time is required");
  assert.equal(new Date(captured.completedAt).toISOString(), captured.completedAt, "completion time must use canonical UTC ISO format");
  assert.deepEqual(Object.keys(captured.sourceHashes).sort(), postgresSources);
  assert.deepEqual(Object.keys(currentHashes).sort(), postgresSources);
  for (const source of postgresSources) {
    assert.match(captured.sourceHashes[source], /^[0-9a-f]{64}$/);
    assert.equal(captured.sourceHashes[source], currentHashes[source], `captured PostgreSQL artifact does not match current source: ${source}`);
  }
  validatePostgresMeasurement(captured.measurement);
  return captured;
}
