/**
 */
/**
 * Analytics data seeding helpers for integration tests.
 * 
 * These helpers create real analytics data by:
 * 1. Creating an index
 * 2. Seeding documents
 * 3. Executing searches with analytics enabled
 * 4. Flushing analytics to storage
 */

import type { APIRequestContext, APIResponse } from '@playwright/test';
import { API_BASE as API, API_HEADERS as HEADERS } from './local-instance';

/**
 * TODO: Document AnalyticsSeedConfig.
 */
export interface AnalyticsSeedConfig {
  /** Index name to seed */
  indexName: string;
  /** Number of historical days to seed for analytics comparisons */
  days?: number;
  /** Number of documents to add */
  documentCount: number;
  /** Number of searches to execute */
  searchCount: number;
  /** Percentage of searches that return no results (0-1) */
  noResultRate: number;
  /** Device distribution { desktop, mobile, tablet } percentages (should sum to 1) */
  deviceDistribution: { desktop: number; mobile: number; tablet: number };
  /** Country distribution (should sum to 1) */
  countryDistribution: Record<string, number>;
}

/** Default configuration for analytics seeding */
export const DEFAULT_ANALYTICS_CONFIG: AnalyticsSeedConfig = {
  indexName: 'analytics-test',
  days: 7,
  documentCount: 100,
  searchCount: 500,
  noResultRate: 0.05,
  deviceDistribution: { desktop: 0.6, mobile: 0.3, tablet: 0.1 },
  countryDistribution: { US: 0.45, GB: 0.2, DE: 0.15, CA: 0.1, FR: 0.1 },
};

export interface AnalyticsSeedPayload {
  index: string;
  days: number;
  searchCount: number;
  noResultRate: number;
  deviceDistribution: AnalyticsSeedConfig['deviceDistribution'];
  countryDistribution: AnalyticsSeedConfig['countryDistribution'];
}

const PRODUCTS = [
  { objectID: 'p01', name: 'MacBook Pro 16"', category: 'Laptops', brand: 'Apple', price: 3499 },
  { objectID: 'p02', name: 'ThinkPad X1 Carbon', category: 'Laptops', brand: 'Lenovo', price: 1849 },
  { objectID: 'p03', name: 'Dell XPS 15', category: 'Laptops', brand: 'Dell', price: 2499 },
  { objectID: 'p04', name: 'iPad Pro 12.9"', category: 'Tablets', brand: 'Apple', price: 1099 },
  { objectID: 'p05', name: 'Galaxy Tab S9', category: 'Tablets', brand: 'Samsung', price: 1199 },
  { objectID: 'p06', name: 'Sony WH-1000XM5', category: 'Audio', brand: 'Sony', price: 349 },
  { objectID: 'p07', name: 'AirPods Pro 2', category: 'Audio', brand: 'Apple', price: 249 },
  { objectID: 'p08', name: 'Samsung 990 Pro 2TB', category: 'Storage', brand: 'Samsung', price: 179 },
  { objectID: 'p09', name: 'LG UltraGear 27" 4K', category: 'Monitors', brand: 'LG', price: 699 },
  { objectID: 'p10', name: 'Logitech MX Master 3S', category: 'Accessories', brand: 'Logitech', price: 99 },
];

export function buildAnalyticsSeedPayload(
  config: AnalyticsSeedConfig,
): AnalyticsSeedPayload {
  return {
    index: config.indexName,
    days: config.days ?? DEFAULT_ANALYTICS_CONFIG.days ?? 7,
    searchCount: config.searchCount,
    noResultRate: config.noResultRate,
    deviceDistribution: config.deviceDistribution,
    countryDistribution: config.countryDistribution,
  };
}

async function requireSuccessfulResponse(
  response: APIResponse,
  operation: string,
): Promise<void> {
  if (!response.ok()) {
    throw new Error(`${operation} failed with status ${response.status()}`);
  }
}

/**
 * Seeds analytics data for testing.
 * Uses the backend's built-in seed endpoint which generates realistic data
 * including geography, devices, searches, clicks, and conversions.
 */
export async function seedAnalytics(
  request: APIRequestContext,
  config: AnalyticsSeedConfig = DEFAULT_ANALYTICS_CONFIG,
): Promise<void> {
  const { indexName, documentCount } = config;
  const seedPayload = buildAnalyticsSeedPayload(config);

  await clearAnalytics(request, indexName);

  // 1. Create index and add documents (needed for searches to work)
  const documents = PRODUCTS.slice(0, Math.min(documentCount, PRODUCTS.length));
  const batchResponse = await request.post(`${API}/1/indexes/${indexName}/batch`, {
    headers: HEADERS,
    data: {
      requests: documents.map((doc) => ({ action: 'addObject', body: doc })),
    },
  });
  await requireSuccessfulResponse(batchResponse, `Seeding documents for ${indexName}`);

  // Wait for indexing to complete
  await new Promise((resolve) => setTimeout(resolve, 2000));

  // 2. Seed analytics using backend's built-in generator
  // This creates realistic analytics with geography, devices, searches, clicks
  const seedResponse = await request.post(`${API}/2/analytics/seed`, {
    headers: HEADERS,
    data: seedPayload,
  });
  await requireSuccessfulResponse(seedResponse, `Seeding analytics for ${indexName}`);
  const seedResult = await seedResponse.json() as { totalSearches?: unknown };
  if (seedResult.totalSearches !== seedPayload.searchCount) {
    throw new Error(
      `Analytics seed returned ${String(seedResult.totalSearches)} searches; expected ${seedPayload.searchCount}`,
    );
  }

  // Wait for analytics data to be available
  // Seed creates data for the requested historical window (NOT including today).
  await new Promise((resolve) => setTimeout(resolve, 3000));

  // Verify data was created by checking the /2/overview endpoint
  // Use yesterday as end date since seed doesn't create data for today.
  // Include an extra day of buffer so timezone boundaries do not hide seeded rows.
  const yesterday = new Date(Date.now() - 24 * 60 * 60 * 1000);
  const verificationWindowDays = Math.max(seedPayload.days + 1, 8);
  const verificationStartDate = new Date(
    Date.now() - verificationWindowDays * 24 * 60 * 60 * 1000,
  );

  const verificationResponse = await request.get(`${API}/2/overview`, {
    headers: HEADERS,
    params: {
      index: indexName,
      startDate: verificationStartDate.toISOString().split('T')[0],
      endDate: yesterday.toISOString().split('T')[0],
    },
  });
  await requireSuccessfulResponse(
    verificationResponse,
    `Verifying analytics seed for ${indexName}`,
  );
  const verificationResult = await verificationResponse.json() as { totalSearches?: unknown };
  if (verificationResult.totalSearches !== seedPayload.searchCount) {
    throw new Error(
      `Analytics verification for ${indexName} found ${String(verificationResult.totalSearches)} searches; expected ${seedPayload.searchCount}`,
    );
  }
}

/**
 * Deletes analytics data for an index.
 */
export async function clearAnalytics(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  const response = await request.delete(`${API}/2/analytics/clear`, {
    data: { index: indexName },
    headers: HEADERS,
  });
  await requireSuccessfulResponse(response, `Clearing analytics for ${indexName}`);
}

/**
 * Deletes an index (cleanup).
 */
export async function deleteIndex(
  request: APIRequestContext,
  indexName: string,
): Promise<void> {
  await request.delete(`${API}/1/indexes/${indexName}`, {
    headers: HEADERS,
  });
}
