/**
 */
import { useQuery } from '@tanstack/react-query';
import api from '@/lib/api';

/**
 * TODO: Document HealthDetail.
 */
export interface HealthDetail {
  status: string;
  active_writers: number;
  max_concurrent_writers: number;
  facet_cache_entries: number;
  facet_cache_cap: number;
  tenants_loaded: number;
  uptime_secs: number;
  version: string;
  heap_allocated_mb: number;
  system_limit_mb: number;
  pressure_level: string;
  allocator: string;
  build_profile: string;
  capabilities: {
    vectorSearch: boolean;
    vectorSearchLocal: boolean;
  };
}

export interface InternalStatus {
  node_id: string;
  replication_enabled: boolean;
  peer_count: number;
  storage_total_bytes: number;
  tenant_count: number;
  ssl_renewal?: {
    next_renewal?: string;
    certificate_expiry?: string;
  };
}

type HealthDetailResponse = Omit<HealthDetail, 'capabilities'> & {
  capabilities?: Partial<HealthDetail['capabilities']>;
};

const HEALTH_DETAIL_QUERY_KEY = ['health-detail'] as const;
const INTERNAL_STATUS_QUERY_KEY = ['internal-status'] as const;

function normalizeHealthDetail(data: HealthDetailResponse): HealthDetail {
  return {
    ...data,
    capabilities: {
      vectorSearch: data.capabilities?.vectorSearch ?? false,
      vectorSearchLocal: data.capabilities?.vectorSearchLocal ?? false,
    },
  };
}

/**
 * TODO: Document useHealthDetail.
 */
export function useHealthDetail() {
  return useQuery<HealthDetail>({
    queryKey: HEALTH_DETAIL_QUERY_KEY,
    queryFn: async () => {
      const { data } = await api.get<HealthDetailResponse>('/health');
      return normalizeHealthDetail(data);
    },
    refetchInterval: 5000,
    retry: 1,
  });
}

export function useInternalStatus() {
  return useQuery<InternalStatus>({
    queryKey: INTERNAL_STATUS_QUERY_KEY,
    queryFn: async () => {
      const { data } = await api.get('/internal/status');
      return data;
    },
    refetchInterval: 10000,
    retry: 1,
  });
}
