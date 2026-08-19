import { useQuery } from '@tanstack/react-query';
import { parsePrometheusText, type PrometheusMetric } from '@/lib/prometheusParser';
import { useAuth } from '@/hooks/useAuth';

const DEFAULT_METRICS_APP_ID = 'flapjack';
const METRICS_QUERY_KEY = 'prometheus-metrics';
const METRICS_URL = import.meta.env.DEV ? '/__flapjack_metrics' : '/metrics';

function getEffectiveMetricsAppId(appId: string | null | undefined) {
  const normalizedAppId = appId?.trim();
  return normalizedAppId || DEFAULT_METRICS_APP_ID;
}

/**
 * TODO: Document usePrometheusMetrics.
 */
export function usePrometheusMetrics() {
  const appId = useAuth((state) => state.appId);
  const isAuthenticated = useAuth((state) => state.isAuthenticated);
  const effectiveAppId = getEffectiveMetricsAppId(appId);
  const credentialScope = isAuthenticated ? 'session:authenticated' : 'session:anonymous';

  return useQuery<PrometheusMetric[]>({
    queryKey: [METRICS_QUERY_KEY, effectiveAppId, credentialScope],
    queryFn: async () => {
      // The dev-only alias avoids the SPA's /metrics route while keeping the request
      // same-origin, so the HttpOnly dashboard session cookie reaches the backend.
      const headers: Record<string, string> = {
        'x-algolia-application-id': effectiveAppId,
      };
      const res = await fetch(METRICS_URL, {
        credentials: 'include',
        headers,
      });
      if (!res.ok) throw new Error(`Metrics fetch failed: ${res.status}`);
      const text = await res.text();
      return parsePrometheusText(text);
    },
    refetchInterval: 10000,
    staleTime: 5000,
  });
}

/**
 * Group metrics by index label into a map of index name → metric short names → values.
 * Strips the `flapjack_` prefix for readability.
 */
export function getPerIndexMetrics(
  metrics: PrometheusMetric[]
): Map<string, Record<string, number>> {
  const result = new Map<string, Record<string, number>>();

  for (const m of metrics) {
    const indexName = m.labels.index;
    if (!indexName) continue;

    if (!result.has(indexName)) {
      result.set(indexName, {});
    }
    const shortName = m.name.replace(/^flapjack_/, '');
    result.get(indexName)![shortName] = m.value;
  }

  return result;
}

/**
 * Get a single system-wide metric value by name.
 * Returns undefined if not found.
 */
export function getSystemMetric(
  metrics: PrometheusMetric[],
  name: string
): number | undefined {
  return metrics.find((m) => m.name === name && Object.keys(m.labels).length === 0)?.value;
}
