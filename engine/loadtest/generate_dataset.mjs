#!/usr/bin/env node

import { mkdir, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { baseProducts } from "../dashboard/tour/product-seed-data.mjs";
import { listBatchFiles } from "./import_benchmark.mjs";

const DEFAULT_COUNT = 100000;
const DEFAULT_BATCH_SIZE = 1000;
const DEFAULT_PROFILE = "standard";
const RECORD_PROFILES = new Set(["compact", "standard"]);
const DEFAULT_OUTPUT_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "data");
const REQUIRED_FIELDS = [
  "name",
  "description",
  "brand",
  "category",
  "subcategory",
  "price",
  "rating",
  "reviewCount",
  "inStock",
  "tags",
  "color",
  "releaseYear",
  "_geoloc",
];
const REQUIRED_STRING_FIELDS = ["name", "description", "brand", "category", "subcategory", "color"];
const REQUIRED_NUMERIC_FIELDS = ["price", "rating", "reviewCount", "releaseYear"];

const CATEGORY_POOL = [
  "Laptops",
  "Tablets",
  "Smartphones",
  "Audio",
  "Accessories",
  "Wearables",
  "Gaming",
  "Monitors",
  "Networking",
  "Home Office",
  "Smart Home",
  "Storage",
];

const BRAND_POOL = [
  "Apple",
  "Dell",
  "Lenovo",
  "ASUS",
  "HP",
  "Samsung",
  "Microsoft",
  "Google",
  "Sony",
  "Bose",
  "Logitech",
  "Razer",
  "Acer",
  "MSI",
  "Alienware",
  "LG",
  "Panasonic",
  "Huawei",
  "Xiaomi",
  "OnePlus",
  "Anker",
  "Belkin",
  "Sennheiser",
  "Jabra",
  "Corsair",
  "SteelSeries",
  "Garmin",
  "Sonos",
  "HyperX",
  "TP-Link",
  "Netgear",
  "Kingston",
];

const SUBCATEGORY_POOL = [
  "Professional",
  "Business",
  "Gaming",
  "Budget",
  "Ultrabook",
  "Creator",
  "Convertible",
  "Noise Cancelling",
  "Mechanical",
  "Wireless",
  "Portable",
  "Desktop",
  "Flagship",
  "Midrange",
  "Entry",
  "Performance",
  "Streaming",
  "Travel",
  "Enterprise",
  "Student",
];

const COLOR_POOL = [
  "Space Black",
  "Midnight Blue",
  "Arctic White",
  "Titan Gray",
  "Graphite",
  "Forest Green",
  "Sunset Orange",
  "Silver",
  "Rose Gold",
  "Matte Black",
  "Pearl White",
  "Ocean Blue",
  "Carbon",
  "Crimson",
  "Emerald",
  "Champagne",
  "Copper",
  "Sapphire",
  "Sand",
  "Lime",
  "Violet",
  "Amber",
  "Teal",
  "Platinum",
];

const TAG_POOL = [
  "portable",
  "business",
  "creative",
  "battery-life",
  "high-performance",
  "gaming",
  "wireless",
  "bluetooth",
  "premium",
  "budget-friendly",
  "compact",
  "noise-cancelling",
  "usb-c",
  "touchscreen",
  "4k",
  "oled",
  "retina",
  "rgb",
  "ergonomic",
  "office",
  "travel",
  "hybrid-work",
  "streaming",
  "productivity",
  "lightweight",
  "student",
  "creator",
  "conference",
  "ai-ready",
  "multi-device",
];

function parsePositiveInteger(rawValue, flagName) {
  const parsed = Number(rawValue);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`Invalid ${flagName} value "${rawValue}". Expected a positive integer.`);
  }
  return parsed;
}

function parseNonNegativeInteger(rawValue, flagName) {
  const parsed = Number(rawValue);
  if (!Number.isInteger(parsed) || parsed < 0) {
    throw new Error(`Invalid ${flagName} value "${rawValue}". Expected a non-negative integer.`);
  }
  return parsed;
}

function parseProfile(rawValue) {
  if (!RECORD_PROFILES.has(rawValue)) {
    throw new Error(
      `Invalid --profile value "${rawValue}". Expected one of: compact, standard.`,
    );
  }
  return rawValue;
}

export function parseCliArgs(argv) {
  const parsed = {
    count: DEFAULT_COUNT,
    batchSize: DEFAULT_BATCH_SIZE,
    profile: DEFAULT_PROFILE,
    startAt: 0,
    outputDir: DEFAULT_OUTPUT_DIR,
    validate: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--count") {
      parsed.count = parsePositiveInteger(argv[index + 1], "--count");
      index += 1;
      continue;
    }
    if (argument === "--batch-size") {
      parsed.batchSize = parsePositiveInteger(argv[index + 1], "--batch-size");
      index += 1;
      continue;
    }
    if (argument === "--output-dir") {
      const rawPath = argv[index + 1];
      if (!rawPath) {
        throw new Error("Missing value for --output-dir.");
      }
      parsed.outputDir = path.resolve(rawPath);
      index += 1;
      continue;
    }
    if (argument === "--profile") {
      parsed.profile = parseProfile(argv[index + 1]);
      index += 1;
      continue;
    }
    if (argument === "--start-at") {
      parsed.startAt = parseNonNegativeInteger(argv[index + 1], "--start-at");
      index += 1;
      continue;
    }
    if (argument === "--validate") {
      parsed.validate = true;
      continue;
    }
    throw new Error(`Unknown argument "${argument}".`);
  }

  return parsed;
}

function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value));
}

function round(value, decimals) {
  const factor = 10 ** decimals;
  return Math.round(value * factor) / factor;
}

function createObjectId(documentNumber, totalCount) {
  const idWidth = Math.max(6, String(totalCount).length);
  return `bench-${String(documentNumber).padStart(idWidth, "0")}`;
}

function rotatePool(pool, globalIndex, baseIndex, step) {
  const poolIndex = (globalIndex * step + baseIndex) % pool.length;
  return pool[poolIndex];
}

function mutateNumericPrice(basePrice, globalIndex, baseIndex) {
  const adjustmentBasis = ((globalIndex * 7 + baseIndex * 13) % 45) - 18;
  const adjustedPrice = basePrice + adjustmentBasis * 4.25;
  return round(Math.max(19, adjustedPrice), 2);
}

function mutateNumericRating(baseRating, globalIndex, baseIndex) {
  const step = ((globalIndex + baseIndex * 3) % 11) - 5;
  return round(clamp(baseRating + step * 0.08, 1, 5), 1);
}

function mutateReviewCount(baseReviewCount, globalIndex, baseIndex) {
  return baseReviewCount + ((globalIndex * 53 + baseIndex * 97) % 20000);
}

function mutateReleaseYear(baseReleaseYear, globalIndex, baseIndex) {
  const offset = (globalIndex * 3 + baseIndex * 5) % 9;
  return baseReleaseYear - 2 + offset;
}

function buildTags(baseTags, globalIndex, baseIndex) {
  const tagSet = new Set(baseTags ?? []);
  tagSet.add(rotatePool(TAG_POOL, globalIndex, baseIndex, 7));
  tagSet.add(rotatePool(TAG_POOL, globalIndex, baseIndex, 11));
  tagSet.add(`series-${(globalIndex + baseIndex) % 128}`);
  return Array.from(tagSet);
}

export function createVariantDocument(baseProduct, variantIndex, documentNumber, totalCount) {
  const baseIndex = documentNumber % baseProducts.length;
  const nameSuffix = `Edition ${variantIndex + 1}`;
  const descriptionSuffix = `Deterministic benchmark variant ${variantIndex + 1}.`;
  const latOffset = (((documentNumber * 17 + baseIndex) % 41) - 20) * 0.00045;
  const lngOffset = (((documentNumber * 23 + baseIndex) % 41) - 20) * 0.00045;

  return {
    objectID: createObjectId(documentNumber + 1, totalCount),
    name: `${baseProduct.name} ${nameSuffix}`,
    description: `${baseProduct.description} ${descriptionSuffix}`,
    brand: rotatePool(BRAND_POOL, documentNumber, baseIndex, 5),
    category: rotatePool(CATEGORY_POOL, documentNumber, baseIndex, 3),
    subcategory: rotatePool(SUBCATEGORY_POOL, documentNumber, baseIndex, 7),
    price: mutateNumericPrice(baseProduct.price, documentNumber, baseIndex),
    rating: mutateNumericRating(baseProduct.rating, documentNumber, baseIndex),
    reviewCount: mutateReviewCount(baseProduct.reviewCount, documentNumber, baseIndex),
    inStock: (documentNumber + baseIndex) % 6 !== 0,
    tags: buildTags(baseProduct.tags, documentNumber, baseIndex),
    color: rotatePool(COLOR_POOL, documentNumber, baseIndex, 9),
    releaseYear: mutateReleaseYear(baseProduct.releaseYear, documentNumber, baseIndex),
    _geoloc: {
      lat: round(clamp(baseProduct._geo.lat + latOffset, -90, 90), 6),
      lng: round(clamp(baseProduct._geo.lng + lngOffset, -180, 180), 6),
    },
  };
}

export function createCompactDocument(baseProduct, documentNumber, totalCount) {
  const baseIndex = documentNumber % baseProducts.length;
  const sequence = String(documentNumber + 1).padStart(Math.max(6, String(totalCount).length), "0");
  const donorTypes = ["individual", "organization", "foundation"];
  return {
    objectID: createObjectId(documentNumber + 1, totalCount),
    name: `Donor record ${sequence} ${baseProduct.name}`,
    brand: rotatePool(BRAND_POOL, documentNumber, baseIndex, 5),
    category: rotatePool(CATEGORY_POOL, documentNumber, baseIndex, 3),
    donorType: donorTypes[(documentNumber + baseIndex) % donorTypes.length],
    region: `region-${(documentNumber + baseIndex) % 16}`,
    tags: Array.from(new Set([
      rotatePool(TAG_POOL, documentNumber, baseIndex, 7),
      rotatePool(TAG_POOL, documentNumber, baseIndex, 11),
      `cohort-${(documentNumber + baseIndex) % 32}`,
    ])),
  };
}

function buildDocument(profile, documentNumber, totalCount) {
  const baseIndex = documentNumber % baseProducts.length;
  const variantIndex = Math.floor(documentNumber / baseProducts.length);
  const baseProduct = baseProducts[baseIndex];
  return profile === "compact"
    ? createCompactDocument(baseProduct, documentNumber, totalCount)
    : createVariantDocument(baseProduct, variantIndex, documentNumber, totalCount);
}

async function clearExistingBatchFiles(outputDir) {
  const entries = await readdir(outputDir);
  const staleBatchFiles = entries.filter((entry) => /^batch_\d+\.json$/.test(entry));
  await Promise.all(staleBatchFiles.map((fileName) => rm(path.join(outputDir, fileName))));
}

function computeFileSizeStats(fileSizes) {
  if (fileSizes.length === 0) {
    return { minBytes: 0, maxBytes: 0, avgBytes: 0 };
  }
  const minBytes = Math.min(...fileSizes);
  const maxBytes = Math.max(...fileSizes);
  const avgBytes = Math.round(fileSizes.reduce((sum, size) => sum + size, 0) / fileSizes.length);
  return { minBytes, maxBytes, avgBytes };
}

export async function generateDataset({
  count = DEFAULT_COUNT,
  batchSize = DEFAULT_BATCH_SIZE,
  profile = DEFAULT_PROFILE,
  startAt = 0,
  outputDir = DEFAULT_OUTPUT_DIR,
  printSummary = true,
} = {}) {
  const validatedCount = parsePositiveInteger(count, "count");
  const validatedBatchSize = parsePositiveInteger(batchSize, "batch-size");
  const validatedProfile = parseProfile(profile);
  const validatedStartAt = parseNonNegativeInteger(startAt, "start-at");
  await mkdir(outputDir, { recursive: true });
  await clearExistingBatchFiles(outputDir);

  const totalCount = validatedStartAt + validatedCount;
  const batchCount = Math.ceil(validatedCount / validatedBatchSize);
  const fileNameWidth = Math.max(3, String(batchCount).length);
  const fileSizes = [];

  for (let batchIndex = 0; batchIndex < batchCount; batchIndex += 1) {
    const start = batchIndex * validatedBatchSize;
    const end = Math.min(start + validatedBatchSize, validatedCount);
    const requests = [];
    for (let localIndex = start; localIndex < end; localIndex += 1) {
      requests.push({
        action: "addObject",
        body: buildDocument(
          validatedProfile,
          validatedStartAt + localIndex,
          totalCount,
        ),
      });
    }
    const payload = { requests };
    const fileName = `batch_${String(batchIndex + 1).padStart(fileNameWidth, "0")}.json`;
    const filePath = path.join(outputDir, fileName);
    await writeFile(filePath, `${JSON.stringify(payload, null, 2)}\n`, "utf8");
    const fileStats = await stat(filePath);
    fileSizes.push(fileStats.size);
  }

  const sizeStats = computeFileSizeStats(fileSizes);
  const summary = {
    totalDocs: validatedCount,
    batchCount,
    batchSize: validatedBatchSize,
    profile: validatedProfile,
    startAt: validatedStartAt,
    outputDir: path.resolve(outputDir),
    sizeStats,
  };

  if (printSummary) {
    console.log(`Generated ${summary.totalDocs} documents`);
    console.log(`Batch files: ${summary.batchCount}`);
    console.log(`Record profile: ${summary.profile}`);
    console.log(`Starting offset: ${summary.startAt}`);
    console.log(`Output directory: ${summary.outputDir}`);
    console.log(
      `File sizes (bytes): min=${sizeStats.minBytes}, max=${sizeStats.maxBytes}, avg=${sizeStats.avgBytes}`,
    );
  }

  return summary;
}

function makeRangeTracker() {
  return { min: Number.POSITIVE_INFINITY, max: Number.NEGATIVE_INFINITY };
}

function recordRange(tracker, value) {
  tracker.min = Math.min(tracker.min, value);
  tracker.max = Math.max(tracker.max, value);
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validateDocumentFieldTypes(document, fileName, index, errors) {
  for (const fieldName of REQUIRED_STRING_FIELDS) {
    if (typeof document[fieldName] !== "string" || document[fieldName].length === 0) {
      errors.push(`${fileName}: request ${index} field ${fieldName} must be a non-empty string`);
    }
  }

  for (const fieldName of REQUIRED_NUMERIC_FIELDS) {
    if (typeof document[fieldName] !== "number" || !Number.isFinite(document[fieldName])) {
      errors.push(`${fileName}: request ${index} field ${fieldName} must be a finite number`);
    }
  }

  if (typeof document.inStock !== "boolean") {
    errors.push(`${fileName}: request ${index} field inStock must be a boolean`);
  }

  if (!Array.isArray(document.tags)) {
    errors.push(`${fileName}: request ${index} tags is not an array`);
  } else if (document.tags.some((tag) => typeof tag !== "string" || tag.length === 0)) {
    errors.push(`${fileName}: request ${index} tags must contain only non-empty strings`);
  }

  if (!Object.hasOwn(document, "_geoloc")) {
    errors.push(`${fileName}: request ${index} missing _geoloc`);
    return;
  }

  if (!isPlainObject(document._geoloc)) {
    errors.push(`${fileName}: request ${index} _geoloc must be an object`);
    return;
  }

  const { lat, lng } = document._geoloc;
  if (typeof lat !== "number" || lat < -90 || lat > 90) {
    errors.push(`${fileName}: request ${index} _geoloc.lat out of range`);
  }
  if (typeof lng !== "number" || lng < -180 || lng > 180) {
    errors.push(`${fileName}: request ${index} _geoloc.lng out of range`);
  }
}

export async function validateGeneratedBatches({
  outputDir = DEFAULT_OUTPUT_DIR,
  expectedCount = DEFAULT_COUNT,
  profile = DEFAULT_PROFILE,
} = {}) {
  const validatedProfile = parseProfile(profile);
  const errors = [];
  let discoveryError = null;
  let batchFiles = [];

  try {
    batchFiles = (await listBatchFiles(outputDir)).map((filePath) => ({
      fileName: path.basename(filePath),
      filePath,
    }));
  } catch (error) {
    discoveryError = error;
    errors.push(error instanceof Error ? error.message : String(error));
  }

  if (batchFiles.length === 0 && !discoveryError) {
    errors.push(`No batch files found in ${outputDir}`);
  }

  const objectIds = new Set();
  let totalDocs = 0;
  const facetDiversity = {
    brand: new Set(),
    category: new Set(),
    subcategory: new Set(),
    color: new Set(),
    tags: new Set(),
  };
  const numericSpread = {
    price: makeRangeTracker(),
    rating: makeRangeTracker(),
    reviewCount: makeRangeTracker(),
    releaseYear: makeRangeTracker(),
  };

  for (const { fileName, filePath } of batchFiles) {
    const content = await readFile(filePath, "utf8");
    let payload;
    try {
      payload = JSON.parse(content);
    } catch (error) {
      errors.push(`Invalid JSON in ${fileName}: ${error.message}`);
      continue;
    }

    if (!payload || !Array.isArray(payload.requests)) {
      errors.push(`${fileName}: payload missing requests array`);
      continue;
    }

    for (const [index, request] of payload.requests.entries()) {
      if (!isPlainObject(request) || request.action !== "addObject" || !isPlainObject(request.body)) {
        errors.push(`${fileName}: request ${index} is not a valid addObject operation`);
        continue;
      }
      totalDocs += 1;

      const document = request.body;
      const objectID = document.objectID;
      if (typeof objectID !== "string" || objectID.length === 0) {
        errors.push(`${fileName}: request ${index} has missing objectID`);
      } else if (objectIds.has(objectID)) {
        errors.push(`${fileName}: request ${index} Duplicate objectID "${objectID}"`);
      } else {
        objectIds.add(objectID);
      }

      if (Object.hasOwn(document, "_geo")) {
        errors.push(`${fileName}: request ${index} has unexpected _geo`);
      }

      const requiredFields = validatedProfile === "compact"
        ? ["name", "brand", "category", "tags"]
        : REQUIRED_FIELDS;
      for (const fieldName of requiredFields) {
        if (!Object.hasOwn(document, fieldName)) {
          errors.push(`${fileName}: request ${index} missing field ${fieldName}`);
        }
      }

      if (validatedProfile === "compact") {
        for (const fieldName of ["name", "brand", "category"]) {
          if (typeof document[fieldName] !== "string" || document[fieldName].length === 0) {
            errors.push(`${fileName}: request ${index} field ${fieldName} must be a non-empty string`);
          }
        }
        if (
          !Array.isArray(document.tags) ||
          document.tags.length < 2 ||
          document.tags.some((tag) => typeof tag !== "string" || tag.length === 0)
        ) {
          errors.push(`${fileName}: request ${index} tags must contain at least two values`);
        }
      } else {
        validateDocumentFieldTypes(document, fileName, index, errors);
      }

      if (typeof document.brand === "string") {
        facetDiversity.brand.add(document.brand);
      }
      if (typeof document.category === "string") {
        facetDiversity.category.add(document.category);
      }
      if (typeof document.subcategory === "string") {
        facetDiversity.subcategory.add(document.subcategory);
      }
      if (typeof document.color === "string") {
        facetDiversity.color.add(document.color);
      }
      if (Array.isArray(document.tags)) {
        for (const tag of document.tags) {
          if (typeof tag === "string") {
            facetDiversity.tags.add(tag);
          }
        }
      }

      for (const fieldName of REQUIRED_NUMERIC_FIELDS) {
        if (typeof document[fieldName] === "number" && Number.isFinite(document[fieldName])) {
          recordRange(numericSpread[fieldName], document[fieldName]);
        }
      }
    }
  }

  if (totalDocs !== expectedCount) {
    errors.push(`Expected ${expectedCount} documents but found ${totalDocs}`);
  }

  const requiredFacetNames = validatedProfile === "compact"
    ? ["brand", "category", "tags"]
    : Object.keys(facetDiversity);
  for (const facetName of requiredFacetNames) {
    const values = facetDiversity[facetName];
    if (values.size <= 1) {
      errors.push(`Facet diversity check failed for ${facetName}: found ${values.size} distinct values`);
    }
  }

  if (validatedProfile === "standard") {
    for (const [fieldName, range] of Object.entries(numericSpread)) {
      if (!Number.isFinite(range.min) || !Number.isFinite(range.max)) {
        errors.push(`Numeric spread check failed for ${fieldName}: no numeric values`);
        continue;
      }
      if (range.min === range.max) {
        errors.push(`Numeric spread check failed for ${fieldName}: values are identical (${range.min})`);
      }
    }
  }

  return {
    isValid: errors.length === 0,
    totalDocs,
    batchCount: batchFiles.length,
    uniqueObjectIDs: objectIds.size,
    facetDiversity: Object.fromEntries(
      requiredFacetNames.map((facetName) => [facetName, facetDiversity[facetName].size]),
    ),
    numericSpread: validatedProfile === "standard"
      ? Object.fromEntries(
          Object.entries(numericSpread).map(([fieldName, range]) => [fieldName, { min: range.min, max: range.max }]),
        )
      : {},
    errors,
  };
}

async function runCli() {
  const options = parseCliArgs(process.argv.slice(2));
  if (options.validate) {
    const report = await validateGeneratedBatches({
      outputDir: options.outputDir,
      expectedCount: options.count,
      profile: options.profile,
    });
    console.log(`Validated ${report.totalDocs} documents across ${report.batchCount} batch files`);
    console.log(`Unique objectIDs: ${report.uniqueObjectIDs}`);
    console.log(`Facet diversity: ${JSON.stringify(report.facetDiversity)}`);
    console.log(`Numeric spread: ${JSON.stringify(report.numericSpread)}`);
    if (!report.isValid) {
      for (const error of report.errors) {
        console.error(`- ${error}`);
      }
      process.exitCode = 1;
    }
    return;
  }

  await generateDataset({
    count: options.count,
    batchSize: options.batchSize,
    profile: options.profile,
    startAt: options.startAt,
    outputDir: options.outputDir,
    printSummary: true,
  });
}

const currentFilePath = fileURLToPath(import.meta.url);
const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === currentFilePath) {
  runCli().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
