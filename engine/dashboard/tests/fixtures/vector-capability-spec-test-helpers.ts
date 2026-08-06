import fs from "node:fs";
import path from "node:path";
import { analyzeVectorCapabilityStructure } from "./vector-capability-spec-structure";
import { resolveRelativeModuleSource } from "./spec-module-sources";

const E2E_FULL_DIR = path.resolve(__dirname, "../e2e-ui/full");

export const NAVIGATION_VECTOR_ENABLED_TEST =
  "chat tab visible and navigable when NeuralSearch mode is enabled";
export const P20_PROOF_TEST =
  "set search mode to Neural Search and verify persistence";
export const P20_SIBLING_TEST =
  "displays search mode and embedders sections with seeded data";
export const P20_CONTROL_GATE = `      if (
        P20_TEXT_ONLY_NEGATIVE_CONTROL &&
        testInfo.title === P20_TEXT_ONLY_CONTROL_TEST_TITLE
      ) {`;

export const P20_NEGATIVE_CONTROL_MODULE = {
  filePath: "/specs/p20_negative_control.ts",
  source: `
    export const P20_TEXT_ONLY_CONTROL_TEST_TITLE =
      'set search mode to Neural Search and verify persistence';
    export function isP20TextOnlyNegativeControl() {
      return process.env.P20_TEXT_ONLY_NEGATIVE_CONTROL === '1';
    }
  `,
};

export const API_HELPERS_MODULE = {
  filePath: path.join(__dirname, "api-helpers.ts"),
  source: `
    export async function skipWhenVectorSearchDisabled() {}
    export async function isVectorSearchEnabled() {}
    export async function configureEmbedder() {}
    export async function addDocumentsWithVectors() {}
    export async function setChatReadySettings() {}
    export async function waitForEmbedder() {}
    export async function waitForSearchableObjectIds() {}
  `,
};

export const API_HELPERS_IMPORT = `
  import {
    addDocumentsWithVectors,
    configureEmbedder,
    isVectorSearchEnabled,
    setChatReadySettings,
    skipWhenVectorSearchDisabled,
    waitForEmbedder,
    waitForSearchableObjectIds,
  } from './api-helpers';
`;

export function readSpec(fileName: string): string {
  return fs.readFileSync(path.join(E2E_FULL_DIR, fileName), "utf8");
}

// Live specs are analyzed through their real import graph so helper bodies that live in
// fixture modules are inspected exactly as in-file helpers are.
export function analyzeSpec(
  fileName: string,
  mutate: (source: string) => string = (source) => source,
) {
  const filePath = path.join(E2E_FULL_DIR, fileName);
  return analyzeVectorCapabilityStructure(mutate(readSpec(fileName)), {
    filePath,
    resolveModuleSource: resolveRelativeModuleSource,
  });
}

export function analyzeWithApiHelpers(source: string) {
  return analyzeVectorCapabilityStructure(`${API_HELPERS_IMPORT}\n${source}`, {
    filePath: "/specs/browser-spec.ts",
    resolveModuleSource: (specifier) =>
      specifier === "./api-helpers" ? API_HELPERS_MODULE : undefined,
  });
}
