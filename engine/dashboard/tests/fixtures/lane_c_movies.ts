/**
 * @module Stub summary for /Users/stuart/parallel_development/flapjack_dev/jun05_am_3_dashboard_polish_round_2/flapjack_dev/engine/dashboard/tests/fixtures/lane_c_movies.ts.
 */
import fs from 'node:fs';
import path from 'node:path';
import type { APIRequestContext } from '@playwright/test';
import { addDocuments, deleteIndex, searchIndex, type SearchIndexResponse } from './index-api-helpers';

export const MOVIES_INDEX = 'movies';
// Keep document-action coverage on the audit-owned corpus so parallel overview audits
// never observe a transient test-only index.
export const MOVIES_DOCUMENT_ACTIONS_INDEX = MOVIES_INDEX;
const LANE_C_LIVE_STATE_ROOT = path.resolve(process.cwd(), '../../docs/live-state');
const LANE_C_REPO_ROOT = path.resolve(LANE_C_LIVE_STATE_ROOT, '../..');
const LANE_C_BASELINE_ROOT = path.join(LANE_C_LIVE_STATE_ROOT, 'jun04_pm_lane_c_baseline');
const LANE_C_BASELINE_ROOT_PATTERN = /^jun\d{2}_(am|pm)_lane_c_baseline$/;
const MOVIES_SEED_VERIFY_FILE = 'movies_seed_verify.json';
const MOVIES_SEED_LOCK_NAME = 'movies_seed.lock';
const MOVIES_SEED_LOCK_TIMEOUT_MS = 30_000;
const MOVIES_SEED_LOCK_POLL_MS = 100;
const STALE_MOVIES_SEED_LOCK_MS = 120_000;

type MovieSeedTarget = {
  indexName: string;
  verificationFileName: string;
  reuseReadyCorpus: boolean;
};

const MOVIE_TITLES = [
  'North Station',
  'Signal at Dawn',
  'The Archive Room',
  'River Glass',
  'Midnight Dispatch',
  'Foundry Lights',
  'The Quiet Orbit',
  'Paper Horizon',
  'Copper Harbor',
  'Last Train West',
  'Juniper Street',
  'The Blue Ledger',
  'Winter Relay',
  'Beacon House',
  'The Granite Key',
  'Harbor Static',
  'Silver Junction',
  'The Orchard Map',
  'Second Weather',
  'Echo Valley',
  'The Lantern Code',
  'Civic Moon',
  'Amber Current',
  'The Relay Desk',
  'Stone Circuit',
  'Clearwater Signal',
  'The Copper Window',
  'Field Notes',
  'Parallel Skies',
  'The Morning Index',
  'Redwood Measure',
  'The Tidal Room',
  'Northbound Pattern',
  'Granite Echoes',
  'The Signal Garden',
  'Fifth Meridian',
  'Blue Harbor Line',
  'The Night Catalog',
  'Station Eleven Forty',
  'The Civic Frame',
  'Golden Circuit',
  'The River Archive',
  'Quiet Coordinates',
  'Lighthouse Query',
  'The Search Party',
  'Terminal Frost',
  'Westward Index',
  'The Marble Signal',
  'Open Channel',
  'Final Facet',
] as const;

const GENRES = ['Drama', 'Mystery', 'Science Fiction', 'Thriller', 'Adventure'] as const;
const RATINGS = ['G', 'PG', 'PG-13', 'R'] as const;

export type LaneCMovie = {
  objectID: string;
  title: string;
  director: string;
  genre: (typeof GENRES)[number];
  year: number;
  rating: (typeof RATINGS)[number];
  runtime_minutes: number;
  popularity: number;
  cast: string[];
  synopsis: string;
};

export const MOVIES: readonly LaneCMovie[] = MOVIE_TITLES.map((title, index) => {
  const movieNumber = index + 1;

  return {
    objectID: `movie_${String(movieNumber).padStart(3, '0')}`,
    title,
    director: `Director ${String.fromCharCode(65 + (index % 10))}`,
    genre: GENRES[index % GENRES.length],
    year: 1980 + index,
    rating: RATINGS[index % RATINGS.length],
    runtime_minutes: 82 + ((index * 7) % 58),
    popularity: 1000 - index * 13,
    cast: [`Actor ${movieNumber}A`, `Actor ${movieNumber}B`],
    synopsis: `${title} follows a deterministic audit fixture through route, search, and detail-ready dashboard states.`,
  };
});

export function validateMovieCorpus(movies: readonly LaneCMovie[]): void {
  if (movies.length !== 50) {
    throw new Error(`Lane C movie corpus must contain exactly 50 documents, got ${movies.length}`);
  }

  const objectIDs = movies.map((movie) => movie.objectID);
  const uniqueObjectIDs = new Set(objectIDs);
  if (uniqueObjectIDs.size !== objectIDs.length) {
    throw new Error('Lane C movie corpus contains duplicate objectID values');
  }
}

export async function seedMoviesIndex(request: APIRequestContext): Promise<SearchIndexResponse> {
  return withMovieSeedLock(() => seedMovieCorpus(request, {
    indexName: MOVIES_INDEX,
    verificationFileName: MOVIES_SEED_VERIFY_FILE,
    reuseReadyCorpus: true,
  }));
}

export async function seedMoviesDocumentActionsIndex(
  request: APIRequestContext,
): Promise<SearchIndexResponse> {
  return seedMoviesIndex(request);
}

export function resolveLaneCBundleDir(
  candidate: string | undefined,
  baselineRoot: string = resolveLaneCBaselineRoot(candidate),
): string | null {
  if (!fs.existsSync(baselineRoot)) {
    return null;
  }

  const resolvedBaselineRoot = path.resolve(baselineRoot);
  if (fs.lstatSync(resolvedBaselineRoot).isSymbolicLink()) {
    throw new Error(`Lane C baseline root must not be a symlink: ${resolvedBaselineRoot}`);
  }

  const canonicalBaselineRoot = fs.realpathSync.native(baselineRoot);

  if (!candidate) {
    return findLatestLaneCBundleDir(canonicalBaselineRoot);
  }

  const rawResolvedCandidate = resolveLaneCRepoRelativePath(candidate);
  const rawRelativeCandidate = path.relative(resolvedBaselineRoot, rawResolvedCandidate);
  if (
    rawRelativeCandidate.length === 0 ||
    rawRelativeCandidate === '.' ||
    rawRelativeCandidate.startsWith('..') ||
    path.isAbsolute(rawRelativeCandidate)
  ) {
    throw new Error(
      `LANE_C_BUNDLE_DIR must stay inside ${canonicalBaselineRoot}, got ${rawResolvedCandidate}`,
    );
  }

  rejectSymlinkedBundlePath(rawResolvedCandidate, resolvedBaselineRoot);

  const resolvedCandidate = canonicalizeBundleCandidatePath(rawResolvedCandidate);
  const relativeCandidate = path.relative(canonicalBaselineRoot, resolvedCandidate);
  if (
    relativeCandidate.length === 0 ||
    relativeCandidate === '.' ||
    relativeCandidate.startsWith('..') ||
    path.isAbsolute(relativeCandidate)
  ) {
    throw new Error(
      `LANE_C_BUNDLE_DIR must stay inside ${canonicalBaselineRoot}, got ${resolvedCandidate}`,
    );
  }

  rejectSymlinkedBundlePath(resolvedCandidate, canonicalBaselineRoot);
  return resolvedCandidate;
}

function resolveLaneCBaselineRoot(candidate: string | undefined): string {
  if (!candidate) {
    return LANE_C_BASELINE_ROOT;
  }

  const rawResolvedCandidate = resolveLaneCRepoRelativePath(candidate);
  const candidateBaselineRoot = path.dirname(rawResolvedCandidate);
  const baselineName = path.basename(candidateBaselineRoot);
  const liveStateRelative = path.relative(LANE_C_LIVE_STATE_ROOT, candidateBaselineRoot);
  if (
    LANE_C_BASELINE_ROOT_PATTERN.test(baselineName) &&
    liveStateRelative.length > 0 &&
    !liveStateRelative.startsWith('..') &&
    !path.isAbsolute(liveStateRelative)
  ) {
    return candidateBaselineRoot;
  }

  return LANE_C_BASELINE_ROOT;
}

function resolveLaneCRepoRelativePath(candidate: string): string {
  return path.isAbsolute(candidate)
    ? path.resolve(candidate)
    : path.resolve(LANE_C_REPO_ROOT, candidate);
}

async function seedMovieCorpus(
  request: APIRequestContext,
  target: MovieSeedTarget,
): Promise<SearchIndexResponse> {
  validateMovieCorpus(MOVIES);

  const existingResponse = target.reuseReadyCorpus
    ? await readReadyMovieCorpus(request, target.indexName)
    : null;
  if (existingResponse) {
    writeSeedVerification(existingResponse, target.verificationFileName);
    return existingResponse;
  }

  await deleteIndex(request, target.indexName);
  await addDocuments(request, target.indexName, [...MOVIES]);

  const finalResponse = await waitForMoviesReady(request, target.indexName);
  writeSeedVerification(finalResponse, target.verificationFileName);
  return finalResponse;
}

async function withMovieSeedLock<T>(seed: () => Promise<T>): Promise<T> {
  const lockDir = resolveMovieSeedLockDir();
  const deadline = Date.now() + MOVIES_SEED_LOCK_TIMEOUT_MS;

  while (Date.now() <= deadline) {
    try {
      fs.mkdirSync(lockDir);
      try {
        return await seed();
      } finally {
        fs.rmSync(lockDir, { force: true, recursive: true });
      }
    } catch (error) {
      const code = error && typeof error === 'object' && 'code' in error
        ? String(error.code)
        : '';
      if (code !== 'EEXIST') {
        throw error;
      }

      removeStaleMovieSeedLock(lockDir);
      await new Promise((resolve) => setTimeout(resolve, MOVIES_SEED_LOCK_POLL_MS));
    }
  }

  throw new Error(`Timed out waiting for Lane C movies seed lock: ${lockDir}`);
}

function resolveMovieSeedLockDir(): string {
  const bundleDir = resolveLaneCBundleDir(process.env.LANE_C_BUNDLE_DIR);
  const lockRoot = bundleDir ?? path.join(process.env.TMPDIR || '/tmp', 'flapjack_lane_c_movies');

  fs.mkdirSync(lockRoot, { recursive: true });
  return path.join(lockRoot, MOVIES_SEED_LOCK_NAME);
}

function removeStaleMovieSeedLock(lockDir: string): void {
  try {
    const ageMs = Date.now() - fs.statSync(lockDir).mtimeMs;
    if (ageMs > STALE_MOVIES_SEED_LOCK_MS) {
      fs.rmSync(lockDir, { force: true, recursive: true });
    }
  } catch {
    // Another worker can remove the lock between the mkdir failure and stale-lock probe.
  }
}

async function readReadyMovieCorpus(
  request: APIRequestContext,
  indexName: string,
): Promise<SearchIndexResponse | null> {
  try {
    const response = await searchIndex(request, indexName, '', { hitsPerPage: MOVIES.length });
    if (response.nbHits !== MOVIES.length) {
      return null;
    }

    const actualMoviesByObjectID = movieHitsByObjectIDFromResponse(response);
    if (!actualMoviesByObjectID || actualMoviesByObjectID.size !== MOVIES.length) {
      return null;
    }

    for (const movie of MOVIES) {
      const actualMovie = actualMoviesByObjectID.get(movie.objectID);
      if (!actualMovie || !hasCanonicalMovieFields(actualMovie, movie)) {
        return null;
      }
    }

    return response;
  } catch {
    return null;
  }
}

function movieHitsByObjectIDFromResponse(
  response: SearchIndexResponse,
): Map<string, Record<string, unknown>> | null {
  if (!Array.isArray(response.hits)) {
    return null;
  }

  const hitsByObjectID = new Map<string, Record<string, unknown>>();
  for (const hit of response.hits) {
    if (!hit || typeof hit !== 'object') {
      return null;
    }

    const movieHit = hit as Record<string, unknown>;
    const objectID = movieHit.objectID;
    if (typeof objectID !== 'string' || hitsByObjectID.has(objectID)) {
      return null;
    }

    hitsByObjectID.set(objectID, movieHit);
  }

  return hitsByObjectID;
}

function hasCanonicalMovieFields(
  actualMovie: Record<string, unknown>,
  expectedMovie: LaneCMovie,
): boolean {
  return (Object.keys(expectedMovie) as Array<keyof LaneCMovie>).every((fieldName) => (
    movieFieldMatches(actualMovie[fieldName], expectedMovie[fieldName])
  ));
}

function movieFieldMatches(
  actualValue: unknown,
  expectedValue: LaneCMovie[keyof LaneCMovie],
): boolean {
  if (Array.isArray(expectedValue)) {
    return Array.isArray(actualValue) &&
      actualValue.length === expectedValue.length &&
      actualValue.every((value, index) => value === expectedValue[index]);
  }

  return actualValue === expectedValue;
}

async function waitForMoviesReady(
  request: APIRequestContext,
  indexName: string,
): Promise<SearchIndexResponse> {
  const deadline = Date.now() + 15_000;
  let lastResponse: SearchIndexResponse | null = null;

  while (Date.now() < deadline) {
    lastResponse = await readReadyMovieCorpus(request, indexName);
    if (lastResponse) {
      return lastResponse;
    }

    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  throw new Error(
    `movies seed did not reach ${MOVIES.length} hits; last response: ${JSON.stringify(lastResponse)}`,
  );
}

function writeSeedVerification(response: SearchIndexResponse, verificationFileName: string): void {
  const bundleDir = resolveLaneCBundleDir(process.env.LANE_C_BUNDLE_DIR);
  if (!bundleDir) {
    return;
  }

  fs.mkdirSync(bundleDir, { recursive: true });
  fs.writeFileSync(
    path.join(bundleDir, verificationFileName),
    `${JSON.stringify(response, null, 2)}\n`,
    'utf8',
  );
}

function rejectSymlinkedBundlePath(targetPath: string, baselineRoot: string): void {
  let currentPath = baselineRoot;
  const relativeTarget = path.relative(baselineRoot, targetPath);

  for (const segment of relativeTarget.split(path.sep)) {
    if (!segment || segment === '.') {
      continue;
    }

    currentPath = path.join(currentPath, segment);
    if (!fs.existsSync(currentPath)) {
      return;
    }

    if (fs.lstatSync(currentPath).isSymbolicLink()) {
      throw new Error(`LANE_C_BUNDLE_DIR must not traverse symlinks: ${currentPath}`);
    }
  }
}

function canonicalizeBundleCandidatePath(candidatePath: string): string {
  if (fs.existsSync(candidatePath)) {
    return fs.realpathSync.native(candidatePath);
  }

  let existingParent = path.dirname(candidatePath);
  let suffix = path.basename(candidatePath);

  while (!fs.existsSync(existingParent)) {
    const segment = path.basename(existingParent);
    existingParent = path.dirname(existingParent);
    suffix = path.join(segment, suffix);
  }

  const canonicalParent = fs.realpathSync.native(existingParent);
  return path.join(canonicalParent, suffix);
}

function findLatestLaneCBundleDir(baselineRoot: string = LANE_C_BASELINE_ROOT): string | null {
  if (!fs.existsSync(baselineRoot)) {
    return null;
  }

  const bundleNames = fs.readdirSync(baselineRoot)
    .filter((name) => fs.statSync(path.join(baselineRoot, name)).isDirectory())
    .sort();

  const latestBundleName = bundleNames.at(-1);
  if (!latestBundleName) {
    return null;
  }

  const latestBundleDir = path.join(baselineRoot, latestBundleName);
  rejectSymlinkedBundlePath(latestBundleDir, baselineRoot);
  return latestBundleDir;
}
