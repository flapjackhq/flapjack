import assert from "node:assert/strict";
import test from "node:test";

import {
  QUERY_TYPES,
  SEARCHABLE_ATTRIBUTES,
  buildQueryCatalog,
  buildSearchResultArtifact,
} from "./search_benchmark.mjs";

const REQUIRED_QUERY_TYPES = [
  "text",
  "typo",
  "multi_word",
  "facet",
  "filter",
  "geo",
  "highlight",
];

function assertLatencySummaryShape(summary) {
  const expectedKeys = ["count", "avg", "min", "max", "p50", "p95", "p99"].sort();
  assert.deepEqual(Object.keys(summary).sort(), expectedKeys);
  for (const key of expectedKeys) {
    assert.equal(typeof summary[key], "number", `${key} must be numeric`);
  }
}

test("QUERY_TYPES matches the seven required search benchmark categories", () => {
  assert.deepEqual([...QUERY_TYPES].sort(), [...REQUIRED_QUERY_TYPES].sort());
});

test("buildQueryCatalog returns deterministic catalog with valid request bodies for all query types", () => {
  const firstCatalog = buildQueryCatalog({ indexName: "benchmark_100k" });
  const secondCatalog = buildQueryCatalog({ indexName: "benchmark_100k" });

  assert.deepEqual(firstCatalog, secondCatalog, "catalog should be deterministic for same index name");

  assert.deepEqual(Object.keys(firstCatalog).sort(), [...REQUIRED_QUERY_TYPES].sort());

  for (const queryType of REQUIRED_QUERY_TYPES) {
    const entries = firstCatalog[queryType];
    assert.ok(Array.isArray(entries), `${queryType} must map to an array`);
    assert.ok(entries.length > 0, `${queryType} must include at least one query`);

    for (const entry of entries) {
      assert.equal(typeof entry.query, "string", `${queryType} query must be a string`);
      assert.equal(typeof entry.params, "object", `${queryType} params must be an object`);
      assert.ok(entry.params !== null, `${queryType} params must not be null`);
    }
  }

  const textEntries = firstCatalog.text;
  assert.ok(textEntries.some((entry) => entry.params.queryType === "prefixLast"), "text should include prefix mode");
  assert.ok(textEntries.some((entry) => entry.params.queryType === "prefixNone"), "text should include full-text mode");

  const typoEntries = firstCatalog.typo;
  const hasMisspelling = typoEntries.some((entry) => /aple|samsng|mackbook|logitec/i.test(entry.query));
  assert.ok(hasMisspelling, "typo catalog should contain deliberate misspellings");

  const multiWordEntries = firstCatalog.multi_word;
  assert.ok(
    multiWordEntries.every((entry) => entry.query.trim().split(/\s+/).length >= 3),
    "multi_word queries must be 3+ words",
  );

  const facetEntries = firstCatalog.facet;
  assert.ok(
    facetEntries.every(
      (entry) =>
        Array.isArray(entry.params.facets) &&
        entry.params.facets.length > 0 &&
        Number.isInteger(entry.params.maxValuesPerFacet) &&
        entry.params.maxValuesPerFacet > 0,
    ),
    "facet queries must set facets and maxValuesPerFacet",
  );

  const filterEntries = firstCatalog.filter;
  const filterStrings = filterEntries.map((entry) => entry.params.filters ?? "");
  assert.ok(filterStrings.some((value) => /price\s*[<>]=?/.test(value)), "filter queries must include numeric price range");
  assert.ok(filterStrings.some((value) => /inStock:(true|false)/.test(value)), "filter queries must include inStock filter");
  assert.ok(filterStrings.some((value) => /releaseYear:\d{4}/.test(value)), "filter queries must include releaseYear filter");

  const geoEntries = firstCatalog.geo;
  assert.ok(
    geoEntries.every(
      (entry) =>
        /^-?\d+(\.\d+)?,-?\d+(\.\d+)?$/.test(entry.params.aroundLatLng) &&
        Number.isInteger(entry.params.aroundRadius) &&
        entry.params.aroundRadius > 0,
    ),
    "geo queries must include aroundLatLng and aroundRadius",
  );

  const highlightEntries = firstCatalog.highlight;
  assert.ok(
    highlightEntries.every((entry) =>
      Array.isArray(entry.params.attributesToHighlight) &&
      entry.params.attributesToHighlight.length === SEARCHABLE_ATTRIBUTES.length &&
      SEARCHABLE_ATTRIBUTES.every((attr) => entry.params.attributesToHighlight.includes(attr))
    ),
    "highlight queries must include all searchable attributes in attributesToHighlight",
  );
});

test("buildSearchResultArtifact emits expected schema and per-type/overall latency summaries", () => {
  const perTypeLatencies = {
    text: [11, 17, 15],
    typo: [22, 29],
    multi_word: [35],
    facet: [18, 21],
    filter: [30, 31, 29],
    geo: [40, 42],
    highlight: [16],
  };

  const artifact = buildSearchResultArtifact({
    docCount: 100000,
    wallClockMs: 1234,
    indexName: "benchmark_100k",
    perTypeLatencies,
  });

  const expectedTopLevelKeys = ["timestamp", "indexName", "docCount", "wallClockMs", "queryTypes", "overall"].sort();
  assert.deepEqual(Object.keys(artifact).sort(), expectedTopLevelKeys);

  assert.equal(typeof artifact.timestamp, "string");
  assert.ok(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/.test(artifact.timestamp), "timestamp should be ISO-8601");
  assert.equal(artifact.indexName, "benchmark_100k");
  assert.equal(artifact.docCount, 100000);
  assert.equal(artifact.wallClockMs, 1234);

  assert.deepEqual(Object.keys(artifact.queryTypes).sort(), Object.keys(perTypeLatencies).sort());

  for (const summary of Object.values(artifact.queryTypes)) {
    assertLatencySummaryShape(summary);
  }

  assertLatencySummaryShape(artifact.overall);
  assert.equal(artifact.queryTypes.text.p50, 15);
  // Nearest-rank p95 for sorted [11, 15, 17] is the third value.
  assert.equal(artifact.queryTypes.text.p95, 17);

  const totalSamples = Object.values(perTypeLatencies).reduce((sum, latencies) => sum + latencies.length, 0);
  assert.equal(artifact.overall.count, totalSamples);
  assert.equal(artifact.overall.min, 11);
  assert.equal(artifact.overall.max, 42);
});
