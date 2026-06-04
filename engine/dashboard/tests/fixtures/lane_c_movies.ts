/**
 * @module Stub summary for /Users/stuart/parallel_development/flapjack_dev/jun04_pm_3_dashboard_first_impression_polish/flapjack_dev/engine/dashboard/tests/fixtures/lane_c_movies.ts.
 */
import fs from 'node:fs';
import path from 'node:path';
import type { APIRequestContext } from '@playwright/test';
import { addDocuments, deleteIndex, searchIndex, type SearchIndexResponse } from './index-api-helpers';

export const MOVIES_INDEX = 'movies';
const LANE_C_BASELINE_ROOT = path.resolve(process.cwd(), '../../docs/live-state/jun04_pm_lane_c_baseline');

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
  validateMovieCorpus(MOVIES);

  await deleteIndex(request, MOVIES_INDEX);
  await addDocuments(request, MOVIES_INDEX, [...MOVIES]);

  const finalResponse = await waitForMoviesReady(request);
  writeSeedVerification(finalResponse);
  return finalResponse;
}

export function resolveLaneCBundleDir(
  candidate: string | undefined,
  baselineRoot: string = LANE_C_BASELINE_ROOT,
): string | null {
  if (!fs.existsSync(baselineRoot)) {
    return null;
  }

  const resolvedBaselineRoot = path.resolve(baselineRoot);
  const canonicalBaselineRoot = fs.realpathSync.native(baselineRoot);
  if (fs.lstatSync(canonicalBaselineRoot).isSymbolicLink()) {
    throw new Error(`Lane C baseline root must not be a symlink: ${canonicalBaselineRoot}`);
  }

  if (!candidate) {
    return findLatestLaneCBundleDir(canonicalBaselineRoot);
  }

  const rawResolvedCandidate = path.resolve(candidate);
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

async function waitForMoviesReady(request: APIRequestContext): Promise<SearchIndexResponse> {
  const deadline = Date.now() + 15_000;
  let lastResponse: SearchIndexResponse | null = null;

  while (Date.now() < deadline) {
    lastResponse = await searchIndex(request, MOVIES_INDEX, '', { hitsPerPage: 1 });
    if (lastResponse.nbHits === MOVIES.length) {
      return lastResponse;
    }

    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  throw new Error(`movies seed did not reach ${MOVIES.length} hits; last response: ${JSON.stringify(lastResponse)}`);
}

function writeSeedVerification(response: SearchIndexResponse): void {
  const bundleDir = resolveLaneCBundleDir(process.env.LANE_C_BUNDLE_DIR);
  if (!bundleDir) {
    return;
  }

  fs.mkdirSync(bundleDir, { recursive: true });
  fs.writeFileSync(
    path.join(bundleDir, 'movies_seed_verify.json'),
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
