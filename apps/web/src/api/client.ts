import { QueryClient } from '@tanstack/react-query'
import { client } from './generated/client.gen'

export const API_BASE: string =
  import.meta.env.VITE_API_BASE ?? 'http://localhost:3001'
export const CHAT_ENDPOINT = `${API_BASE}/api/chat`

client.setConfig({ baseUrl: API_BASE })
client.interceptors.error.use((error) => {
  if (error instanceof Error) return error
  if (error && typeof error === 'object' && 'error' in error) {
    return new Error(String(error.error))
  }
  return new Error('后端请求失败')
})

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: { retry: false, staleTime: 500, refetchOnWindowFocus: false },
    mutations: { retry: false },
  },
})
