#!/usr/bin/env node

import { baseProducts, seedSettings } from "./product-seed-data.mjs";
import { summarizeBatchLatencies } from "./import_benchmark.mjs";

export const QUERY_TYPES = Object.freeze([
  "text",
  "typo",
  "multi_word",
  "facet",
  "filter",
  "geo",
  "highlight",
]);

export const SEARCHABLE_ATTRIBUTES = Object.freeze([...seedSettings.searchableAttributes]);

const DEFAULT_SAMPLES_PER_TYPE = 6;
const DEFAULT_FACET_LIMIT = 25;
const DEFAULT_AROUND_RADIUS = 50000;
const CLI_USAGE = "Usage: search_benchmark.mjs --catalog|--artifact [options]";

const TEXT_TERMS = Object.freeze([
  { query: "MacB", params: { queryType: "prefixLast" } },
  { query: "Dell XPS", params: { queryType: "prefixNone" } },
  { query: "Sony WH", params: { queryType: "prefixLast" } },
  { query: "Razer BlackWidow", params: { queryType: "prefixNone" } },
]);

const TYPO_TERMS = Object.freeze([
  "Aple MacBok",
  "Samsng Galaxi",
  "Lenvo ThinkPad",
  "Logitec MX Mster",
  "Bose QuiteComfort",
]);

const MULTI_WORD_TERMS = Object.freeze([
  "apple professional laptop creative",
  "wireless noise cancelling audio",
  "gaming mechanical keyboard rgb",
  "business ultrabook durable battery",
  "creator monitor color accurate",
]);

const FACET_QUERIES = Object.freeze([
  { query: "wireless", facets: ["category", "brand", "tags"] },
  { query: "professional", facets: ["brand", "subcategory", "color"] },
  { query: "gaming", facets: ["category", "subcategory", "tags"] },
]);

const FILTER_QUERIES = Object.freeze([
  { query: "laptop", filters: "price >= 500 AND price <= 2500 AND inStock:true AND releaseYear:2024" },
  { query: "audio", filters: "price >= 150 AND price <= 500 AND inStock:true AND releaseYear:2023" },
  { query: "monitor", filters: "price > 500 AND inStock:false OR releaseYear:2024" },
]);

const HIGHLIGHT_TERMS = Object.freeze([
  "wireless audio",
  "professional laptop",
  "gaming monitor",
]);

const GEO_COORDINATES = Object.freeze(
  baseProducts
    .slice(0, 8)
    .map((product) => `${product._geo.lat},${product._geo.lng}`),
);

function pickForIteration(values, iterationIndex) {
  return values[iterationIndex % values.length];
}

function indexSeed(indexName) {
  const chars = String(indexName ?? "");
  let seed = 0;
  for (const char of chars) {
    seed = (seed + char.charCodeAt(0)) % 104729;
  }
  return seed;
}

function withCommonParams(params) {
  return {
    hitsPerPage: 20,
    ...params,
  };
}

function buildCatalogEntry(query, params = {}) {
  return {
    query,
    params: withCommonParams(params),
  };
}

const QUERY_ENTRY_BUILDERS = Object.freeze({
  text(iterationIndex) {
    const selection = pickForIteration(TEXT_TERMS, iterationIndex);
    return buildCatalogEntry(selection.query, selection.params);
  },
  typo(iterationIndex) {
    return buildCatalogEntry(
      pickForIteration(TYPO_TERMS, iterationIndex),
      { typoTolerance: true },
    );
  },
  multi_word(iterationIndex) {
    return buildCatalogEntry(pickForIteration(MULTI_WORD_TERMS, iterationIndex));
  },
  facet(iterationIndex) {
    const selection = pickForIteration(FACET_QUERIES, iterationIndex);
    return buildCatalogEntry(selection.query, {
      facets: selection.facets,
      maxValuesPerFacet: DEFAULT_FACET_LIMIT,
    });
  },
  filter(iterationIndex) {
    const selection = pickForIteration(FILTER_QUERIES, iterationIndex);
    return buildCatalogEntry(selection.query, {
      filters: selection.filters,
    });
  },
  geo(iterationIndex) {
    return buildCatalogEntry("", {
      aroundLatLng: pickForIteration(GEO_COORDINATES, iterationIndex),
      aroundRadius: DEFAULT_AROUND_RADIUS,
    });
  },
  highlight(iterationIndex) {
    return buildCatalogEntry(pickForIteration(HIGHLIGHT_TERMS, iterationIndex), {
      attributesToHighlight: SEARCHABLE_ATTRIBUTES,
    });
  },
});

export function buildQueryCatalog({ indexName = "benchmark_100k", samplesPerType = DEFAULT_SAMPLES_PER_TYPE } = {}) {
  const totalSamples = Number.isInteger(samplesPerType) && samplesPerType > 0
    ? samplesPerType
    : DEFAULT_SAMPLES_PER_TYPE;
  const deterministicOffset = indexSeed(indexName);
  const catalog = Object.fromEntries(QUERY_TYPES.map((queryType) => [queryType, []]));

  for (let iterationIndex = 0; iterationIndex < totalSamples; iterationIndex += 1) {
    const deterministicIndex = deterministicOffset + iterationIndex;
    for (const queryType of QUERY_TYPES) {
      catalog[queryType].push(QUERY_ENTRY_BUILDERS[queryType](deterministicIndex));
    }
  }

  return catalog;
}

function normalizeLatencies(perTypeLatencies, queryType) {
  const latencies = perTypeLatencies[queryType];
  if (!Array.isArray(latencies)) {
    return [];
  }
  return latencies.filter((value) => Number.isFinite(value) && value >= 0);
}

function summarizeSearchLatencies(latencies) {
  const summary = summarizeBatchLatencies(latencies);
  const sorted = [...latencies].sort((left, right) => left - right);
  const p50Index = Math.max(0, Math.ceil(sorted.length * 0.5) - 1);
  return {
    ...summary,
    p50: sorted.length > 0 ? sorted[p50Index] : 0,
  };
}

export function buildSearchResultArtifact({
  docCount,
  wallClockMs,
  indexName,
  perTypeLatencies,
}) {
  const queryTypes = {};
  const allLatencies = [];

  for (const queryType of QUERY_TYPES) {
    const latencies = normalizeLatencies(perTypeLatencies ?? {}, queryType);
    queryTypes[queryType] = summarizeSearchLatencies(latencies);
    allLatencies.push(...latencies);
  }

  return {
    timestamp: new Date().toISOString(),
    indexName,
    docCount,
    wallClockMs,
    queryTypes,
    overall: summarizeSearchLatencies(allLatencies),
  };
}

function parseCliArgs(argv) {
  const parsed = {
    mode: null,
    indexName: "benchmark_100k",
    samplesPerType: DEFAULT_SAMPLES_PER_TYPE,
    docCount: 0,
    wallClockMs: 0,
    perTypeLatencies: {},
  };

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--catalog") {
      parsed.mode = "catalog";
      continue;
    }
    if (argument === "--artifact") {
      parsed.mode = "artifact";
      continue;
    }
    if (argument === "--help" || argument === "-h") {
      parsed.mode = "help";
      continue;
    }
    if (argument === "--index-name") {
      parsed.indexName = argv[index + 1] ?? parsed.indexName;
      index += 1;
      continue;
    }
    if (argument === "--samples-per-type") {
      const numericValue = Number(argv[index + 1]);
      if (Number.isInteger(numericValue) && numericValue > 0) {
        parsed.samplesPerType = numericValue;
      }
      index += 1;
      continue;
    }
    if (argument === "--doc-count") {
      parsed.docCount = Number(argv[index + 1]);
      index += 1;
      continue;
    }
    if (argument === "--wall-clock-ms") {
      parsed.wallClockMs = Number(argv[index + 1]);
      index += 1;
      continue;
    }
    if (argument === "--per-type-latencies") {
      parsed.perTypeLatencies = JSON.parse(argv[index + 1] ?? "{}");
      index += 1;
      continue;
    }
    throw new Error(`Unknown argument: ${argument}`);
  }

  return parsed;
}

function runCli() {
  const options = parseCliArgs(process.argv.slice(2));
  if (options.mode === "help") {
    process.stdout.write(`${CLI_USAGE}\n`);
    return;
  }
  if (options.mode === "catalog") {
    const catalog = buildQueryCatalog({
      indexName: options.indexName,
      samplesPerType: options.samplesPerType,
    });
    process.stdout.write(`${JSON.stringify(catalog)}\n`);
    return;
  }

  if (options.mode === "artifact") {
    const artifact = buildSearchResultArtifact({
      docCount: Number(options.docCount),
      wallClockMs: Number(options.wallClockMs),
      indexName: options.indexName,
      perTypeLatencies: options.perTypeLatencies,
    });
    process.stdout.write(`${JSON.stringify(artifact, null, 2)}\n`);
    return;
  }

  throw new Error(CLI_USAGE);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  runCli();
}
