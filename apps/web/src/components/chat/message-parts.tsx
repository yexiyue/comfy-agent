import { MessageContent, MessageResponse } from '@/components/ai-elements/message'
import { Tool, ToolContent, ToolHeader, ToolInput, ToolOutput } from '@/components/ai-elements/tool'
import { ReasoningPart } from '@/components/chat/reasoning-part'
import { type ChatUIMessage } from '@/lib/chat'
import { chatTools, type AnyToolPart } from '@/lib/tools'
import type { ToolUIPart, UIMessage } from 'ai'

function asToolPart(part: UIMessage['parts'][number]): AnyToolPart | null {
  if (part.type === 'dynamic-tool') return part
  if (part.type.startsWith('tool-')) return part as ToolUIPart
  return null
}

export function MessageParts({
  message,
  stopped,
  isStreaming = false,
}: {
  message: ChatUIMessage
  stopped: boolean
  isStreaming?: boolean
}) {
  return (
    <MessageContent>
      {message.parts.map((part, index) => {
        const toolPart = asToolPart(part)
        if (toolPart) {
          const isDynamic = toolPart.type === 'dynamic-tool'
          const toolName = isDynamic ? toolPart.toolName : toolPart.type.slice('tool-'.length)
          const renderTool = chatTools[toolName]?.renderTool
          return (
            <Tool key={toolPart.toolCallId ?? index}>
              {isDynamic ? (
                <ToolHeader
                  state={toolPart.state}
                  toolName={toolName}
                  type="dynamic-tool"
                />
              ) : (
                <ToolHeader
                  state={toolPart.state}
                  type={toolPart.type as `tool-${string}`}
                />
              )}
              <ToolContent>
                {renderTool ? (
                  renderTool(toolPart)
                ) : (
                  <>
                    <ToolInput input={toolPart.input} />
                    <ToolOutput
                      output={toolPart.output}
                      errorText={toolPart.errorText}
                    />
                  </>
                )}
              </ToolContent>
            </Tool>
          )
        }
        if (part.type === 'reasoning' && part.text !== '') {
          return <ReasoningPart key={index} text={part.text} isStreaming={isStreaming && part.state === 'streaming'} />
        }
        if (part.type === 'text' && part.text !== '') {
          return <MessageResponse key={index}>{part.text}</MessageResponse>
        }
        return null
      })}
      {message.metadata?.draft && (<div className="text-muted-foreground text-xs">{message.metadata.status === 'superseded' ? '已被新指令接替的未完成草稿' : message.metadata.status === 'cancelled' ? '已终止的未完成草稿' : '未完成草稿；继续时将替换这一段'}</div>)}
      {stopped && (
        <div className="text-muted-foreground text-xs">已停止生成</div>
      )}
      {message.role === 'assistant' && message.metadata?.outcome && (
        <div className="text-muted-foreground flex items-center gap-2 text-xs">
          {message.metadata.outcome === 'step-limit' ? (
            <span className="text-orange-600 dark:text-orange-400">
              已达步数上限
            </span>
          ) : (
            <span>已完成</span>
          )}
          {message.metadata.steps !== undefined && (
            <span>· {message.metadata.steps} 步</span>
          )}
        </div>
      )}
    </MessageContent>
  )
}
