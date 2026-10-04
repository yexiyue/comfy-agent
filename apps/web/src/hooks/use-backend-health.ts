import { useQuery } from '@tanstack/react-query'
import { getHealthOptions } from '@/api/generated/@tanstack/react-query.gen'
import { API_BASE } from '@/api/client'

export type BackendHealth = 'checking' | 'up' | 'down'

export function useBackendHealth() {
  const query = useQuery({
    ...getHealthOptions(),
    staleTime: 30_000,
    queryFn: async (context) =>
      getHealthOptions().queryFn!({
        ...context,
        signal: AbortSignal.any([context.signal, AbortSignal.timeout(5000)]),
      }),
  })
  let health: BackendHealth = 'up'
  if (query.isFetching) health = 'checking'
  else if (query.isError) health = 'down'
  return { health, address: API_BASE, probe: query.refetch }
}
