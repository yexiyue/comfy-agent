/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** 聊天后端地址，未设置时默认 http://localhost:3001 */
  readonly VITE_API_BASE?: string
}
