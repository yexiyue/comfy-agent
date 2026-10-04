import type { UIMessage } from 'ai'

import type { ChatUITools } from '@/lib/tools'

/** 后端 finish 事件回填的消息级元数据（见 crates/server/src/views.rs）。 */
export type ChatMessageMetadata = {
  outcome?: 'finished' | 'step-limit'
  runId?: string
  conversationId?: string
  attemptId?: string
  status?: string
  draft?: boolean
  steps?: number
}

export type ChatUIMessage = UIMessage<
  ChatMessageMetadata,
  { 'run-state': unknown },
  ChatUITools
>
