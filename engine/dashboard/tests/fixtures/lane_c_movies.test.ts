/* @vitest-environment node */
/**
 * Contract: seeding the Lane C movie corpus must not write into the tracked evidence
 * baseline unless LANE_C_BUNDLE_DIR explicitly names a bundle.
 *
 * Before this contract existed, a default `./s/test --dashboard-full` rewrote the
 * tracked `movies_seed_verify.json` — the seeder reached it through
 * `resolveLaneCBundleDir`'s newest-bundle fallback, and `tests/e2e-ui/full/search.spec.ts`
 * pulls the seeder into the default composition. The rewrite always differed because
 * the file records `processingTimeMS`, `serverTimeMS` and `serverUsed`, so every full
 * run left the shared clone dirty and `batman land` exited 1 for every worker in it.
 *
 * The assertions below are on the real filesystem rather than on a spy, because the
 * contract is "the tracked tree is unchanged" and a call-count assertion cannot fail
 * for the defect that actually mattered.
 */
import fs from "node:fs";
import path from "node:path";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from "vitest";
import {
  MOVIES,
  resolveExplicitLaneCBundleDir,
  resolveLaneCBundleDir,
  seedMoviesIndex,
} from "./lane_c_movies";
import { buildMovieSeedRequest } from "./lane_c_movies_mock_backend";

const BASELINE_ROOT = path.resolve(
  process.cwd(),
  "../../docs/live-state/jun04_pm_lane_c_baseline",
);
const TRACKED_BUNDLE = "20260604T191244Z";
const VERIFY_PATH = path.join(
  BASELINE_ROOT,
  TRACKED_BUNDLE,
  "movies_seed_verify.json",
);
const IS_PRIVATE_DEV_CHECKOUT = fs.existsSync(
  path.resolve(process.cwd(), "../../.debbie.toml"),
);

let synthesizedPublicBundle = false;

beforeAll(() => {
  if (fs.existsSync(VERIFY_PATH)) {
    return;
  }

  if (IS_PRIVATE_DEV_CHECKOUT) {
    throw new Error(`${VERIFY_PATH} must exist for this test to mean anything`);
  }

  // Public mirrors intentionally omit private live-state evidence. Supply the
  // smallest real-filesystem specimen so the write-boundary contract remains
  // fail-capable there instead of depending on a private sync artifact.
  fs.mkdirSync(path.dirname(VERIFY_PATH), { recursive: true });
  fs.writeFileSync(VERIFY_PATH, '{"hits":[]}\n');
  synthesizedPublicBundle = true;
});

afterAll(() => {
  if (synthesizedPublicBundle) {
    fs.rmSync(BASELINE_ROOT, { recursive: true, force: true });
  }
});

/** Every path under `dir`, relative and sorted, so the snapshot is order-stable. */
function listTree(dir: string): string[] {
  const entries: string[] = [];
  const walk = (current: string): void => {
    for (const name of fs.readdirSync(current).sort()) {
      const absolute = path.join(current, name);
      entries.push(path.relative(dir, absolute));
      if (fs.statSync(absolute).isDirectory()) {
        walk(absolute);
      }
    }
  };
  walk(dir);
  return entries;
}

let originalBundleDir: string | undefined;

beforeEach(() => {
  originalBundleDir = process.env.LANE_C_BUNDLE_DIR;
  delete process.env.LANE_C_BUNDLE_DIR;
});

afterEach(() => {
  if (originalBundleDir === undefined) {
    delete process.env.LANE_C_BUNDLE_DIR;
  } else {
    process.env.LANE_C_BUNDLE_DIR = originalBundleDir;
  }
});

describe("resolveExplicitLaneCBundleDir", () => {
  it("returns null when LANE_C_BUNDLE_DIR is absent, so writes have no destination", () => {
    expect(resolveExplicitLaneCBundleDir()).toBeNull();
  });

  it("resolves the named bundle when LANE_C_BUNDLE_DIR is set", () => {
    const candidate = `docs/live-state/jun04_pm_lane_c_baseline/${TRACKED_BUNDLE}`;
    process.env.LANE_C_BUNDLE_DIR = candidate;

    // realpath, because the resolver canonicalises through /tmp-style symlinks.
    expect(resolveExplicitLaneCBundleDir()).toBe(
      fs.realpathSync.native(path.join(BASELINE_ROOT, TRACKED_BUNDLE)),
    );
  });

  it("is strictly narrower than the read resolver, which still falls back", () => {
    // The fallback is deliberately kept for reads — this is the exact difference that
    // turned a read helper into an accidental default write destination.
    expect(resolveLaneCBundleDir(undefined)).not.toBeNull();
    expect(resolveExplicitLaneCBundleDir(undefined)).toBeNull();
  });
});

describe("seedMoviesIndex evidence writes", () => {
  it("leaves the tracked baseline byte-identical when LANE_C_BUNDLE_DIR is absent", async () => {
    // Guard the guard: if the tracked specimen ever disappears this test would pass
    // vacuously, so fail loudly instead.
    expect(
      fs.existsSync(VERIFY_PATH),
      `${VERIFY_PATH} must exist for this test to mean anything`,
    ).toBe(true);

    const treeBefore = listTree(BASELINE_ROOT);
    const verifyBefore = fs.readFileSync(VERIFY_PATH);

    const backend = buildMovieSeedRequest([...MOVIES]);
    const response = await seedMoviesIndex(backend.request);

    // The seed itself must still have done its job — otherwise "nothing was written"
    // would be satisfied by the seeder simply not running.
    expect(response.hits).toEqual([...MOVIES]);
    expect(backend.deleteCount()).toBe(0);

    expect(listTree(BASELINE_ROOT)).toEqual(treeBefore);
    expect(fs.readFileSync(VERIFY_PATH).equals(verifyBefore)).toBe(true);
  });
});
