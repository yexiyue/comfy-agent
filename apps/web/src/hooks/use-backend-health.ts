import { useCallback, useEffect, useState } from 'react'

import { API_BASE, HEALTH_ENDPOINT } from '@/lib/api'

export type BackendHealth = 'checking' | 'up' | 'down'

/** 挂载时探测一次后端 /health；返回可手动重试的探测函数。 */
export function useBackendHealth(): {
  health: BackendHealth
  address: string
  probe: () => Promise<void>
} {
  const [health, setHealth] = useState<BackendHealth>('checking')

  const probe = useCallback(async () => {
    try {
      const response = await fetch(HEALTH_ENDPOINT)
      setHealth(response.ok ? 'up' : 'down')
    } catch {
      setHealth('down')
    }
  }, [])

  useEffect(() => {
    void probe()
  }, [probe])

  return { health, address: API_BASE, probe }
}
