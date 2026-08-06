import fs from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";

const FIXTURES_DIR = __dirname;
const VECTOR_CAPABILITY_FILE_PREFIX = "vector-capability-";
const MAX_FILE_LINES = 800;

function vectorCapabilityFixtureFiles(): string[] {
  return fs
    .readdirSync(FIXTURES_DIR)
    .filter((fileName) => fileName.startsWith(VECTOR_CAPABILITY_FILE_PREFIX))
    .filter((fileName) => fileName.endsWith(".ts"));
}

function filesDefining(functionName: string): string[] {
  const declaration = new RegExp(`\\bfunction ${functionName}\\(`);
  return vectorCapabilityFixtureFiles().filter((fileName) =>
    declaration.test(
      fs.readFileSync(path.join(FIXTURES_DIR, fileName), "utf8"),
    ),
  );
}

describe("vector capability analyzer module structure", () => {
  it("keeps each analyzer function in one canonical module", () => {
    expect(filesDefining("collectCallableDefinitions")).toEqual([
      "vector-capability-definitions.ts",
    ]);
    expect(filesDefining("collectConstantDefinitions")).toEqual([
      "vector-capability-constants.ts",
    ]);
    expect(filesDefining("containsHealthCapabilityOverride")).toEqual([
      "vector-capability-health-override.ts",
    ]);
  });

  it("has no competing legacy extraction module", () => {
    expect(
      fs.existsSync(
        path.join(FIXTURES_DIR, "vector_capability_definitions.ts"),
      ),
    ).toBe(false);
  });

  it("keeps vector capability fixture modules below the hard file-size limit", () => {
    const oversizedFiles = vectorCapabilityFixtureFiles().filter((fileName) => {
      const source = fs.readFileSync(path.join(FIXTURES_DIR, fileName), "utf8");
      return source.split("\n").length > MAX_FILE_LINES;
    });

    expect(oversizedFiles).toEqual([]);
  });
});
