import type { ToolUIPart, UIMessage } from 'ai'

import type { ChatUITools } from '@/lib/tools'

/** 后端 finish 事件回填的消息级元数据（见 crates/server/src/stream.rs）。 */
export type ChatMessageMetadata = {
  outcome?: 'finished' | 'step-limit'
  steps?: number
}

export type ChatUIMessage = UIMessage<
  ChatMessageMetadata,
  never,
  ChatUITools
>

/** 后端只接受已完成态的工具交换（output-available / output-error）。 */
const TERMINAL_TOOL_STATES = new Set(['output-available', 'output-error'])

function isToolPart(part: UIMessage['parts'][number]): part is ToolUIPart {
  const type = part.type
  return type === 'dynamic-tool' || type.startsWith('tool-')
}

/**
 * 停止/出错后历史里可能留下未完成的工具 part，直接续聊会被后端 400 拒绝。
 * 移除非终态工具 part；若消息因此变空则整条移除（与 README 的恢复说明一致）。
 */
export function sanitizeMessages(
  messages: ChatUIMessage[]
): ChatUIMessage[] {
  const result: ChatUIMessage[] = []
  for (const message of messages) {
    if (message.role !== 'assistant') {
      result.push(message)
      continue
    }
    const parts = message.parts.filter(
      (part) => !isToolPart(part) || TERMINAL_TOOL_STATES.has(part.state)
    )
    if (parts.length > 0) {
      result.push({ ...message, parts })
    }
  }
  return result
}
