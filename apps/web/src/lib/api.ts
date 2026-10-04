export const API_BASE: string =
  import.meta.env.VITE_API_BASE ?? 'http://localhost:3001'

export const CHAT_ENDPOINT = `${API_BASE}/api/chat`
export const HEALTH_ENDPOINT = `${API_BASE}/health`
