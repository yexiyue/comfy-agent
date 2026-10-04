import {
  Message,
  MessageContent,
  MessageResponse,
} from '@/components/ai-elements/message'
import {
  Conversation,
  ConversationContent,
  ConversationEmptyState,
  ConversationScrollButton,
} from '@/components/ai-elements/conversation'
import {
  PromptInput,
  PromptInputBody,
  PromptInputFooter,
  PromptInputSubmit,
  PromptInputTextarea,
  PromptInputTools,
} from '@/components/ai-elements/prompt-input'
import {
  Tool,
  ToolContent,
  ToolHeader,
  ToolInput,
  ToolOutput,
} from '@/components/ai-elements/tool'
import { useBackendHealth } from '@/hooks/use-backend-health'
import { CHAT_ENDPOINT } from '@/lib/api'
import {
  sanitizeMessages,
  type ChatUIMessage,
} from '@/lib/chat'
import { chatTools, type AnyToolPart } from '@/lib/tools'
import { useChat } from '@ai-sdk/react'
import { DefaultChatTransport } from 'ai'
import type { ToolUIPart, UIMessage } from 'ai'
import { CircleAlertIcon } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'

function asToolPart(part: UIMessage['parts'][number]): AnyToolPart | null {
  if (part.type === 'dynamic-tool') return part
  if (part.type.startsWith('tool-')) return part as ToolUIPart
  return null
}

function MessageParts({
  message,
  stopped,
}: {
  message: ChatUIMessage
  stopped: boolean
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
        if (part.type === 'text' && part.text !== '') {
          return <MessageResponse key={index}>{part.text}</MessageResponse>
        }
        return null
      })}
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

function App() {
  const { health, address, probe } = useBackendHealth()

  const transport = useMemo(
    () => new DefaultChatTransport<ChatUIMessage>({ api: CHAT_ENDPOINT }),
    []
  )
  const chat = useChat<ChatUIMessage>({ transport })

  // 流结束（ready/error）后清理未完成的工具 part，保证下一次发送能过后端校验。
  const statusRef = useRef(chat.status)
  useEffect(() => {
    const wasActive =
      statusRef.current === 'submitted' || statusRef.current === 'streaming'
    const settled = chat.status === 'ready' || chat.status === 'error'
    if (wasActive && settled) {
      chat.setMessages(sanitizeMessages(chat.messages))
      if (stopPendingRef.current) {
        stopPendingRef.current = false
        const last = chat.messages.at(-1)
        if (last?.role === 'assistant') {
          setStoppedIds((ids) => new Set(ids).add(last.id))
        }
      }
    }
    statusRef.current = chat.status
  }, [chat.status, chat.messages, chat.setMessages])

  const busy = chat.status === 'submitted' || chat.status === 'streaming'

  // stop() 时给正在流式的助手消息打上"已停止"标记（仅展示，不参与历史）。
  // 标记放在流落定后做：流式消息 id 会随 start 事件替换，停止瞬间取到的 id 不可靠。
  const [stoppedIds, setStoppedIds] = useState<ReadonlySet<string>>(new Set())
  const stopPendingRef = useRef(false)
  const handleStop = useCallback(() => {
    stopPendingRef.current = true
    chat.stop()
  }, [chat])

  const handleSubmit = (message: { text?: string }) => {
    const text = message.text?.trim()
    if (!text || busy) return
    void chat.sendMessage({ text })
  }

  return (
    <div className="bg-background text-foreground mx-auto flex h-dvh max-w-3xl flex-col">
      <header className="flex items-center justify-between gap-4 px-4 py-3">
        <h1 className="text-sm font-semibold">comfy-agent</h1>
        <button
          className="flex items-center gap-1.5 text-xs"
          onClick={() => void probe()}
          title={`后端地址：${address}（点击重新探测）`}
          type="button"
        >
          <span
            className={`inline-block size-2 rounded-full ${
              health === 'up'
                ? 'bg-green-500'
                : health === 'checking'
                  ? 'bg-muted-foreground/50'
                  : 'bg-red-500'
            }`}
          />
          <span className="text-muted-foreground">
            {health === 'up'
              ? '后端已连接'
              : health === 'checking'
                ? '探测后端中…'
                : '后端不可达'}
          </span>
        </button>
      </header>

      {health === 'down' && (
        <div
          className="bg-destructive/10 text-destructive mx-4 mb-2 flex items-center gap-2 rounded-md px-3 py-2 text-xs"
          role="alert"
        >
          <CircleAlertIcon className="size-4 shrink-0" />
          <span>
            后端不可用（期望地址 {address}）。请先运行 <code>cargo run -p server</code>，然后点击右上角状态重新探测。
          </span>
        </div>
      )}

      <Conversation>
        <ConversationContent>
          {chat.messages.length === 0 ? (
            <ConversationEmptyState
              description="输入消息开始对话，模型可调用后端注册的工具"
              title="开始聊天"
            />
          ) : (
            chat.messages.map((message) => (
              <Message from={message.role} key={message.id}>
                <MessageParts
                  message={message}
                  stopped={stoppedIds.has(message.id)}
                />
              </Message>
            ))
          )}
          {chat.status === 'submitted' && (
            <div className="text-muted-foreground px-1 text-xs">思考中…</div>
          )}
        </ConversationContent>
        <ConversationScrollButton />
      </Conversation>

      {chat.error && (
        <div
          className="bg-destructive/10 text-destructive mx-4 mb-2 flex items-start gap-2 rounded-md px-3 py-2 text-xs"
          role="alert"
        >
          <CircleAlertIcon className="mt-0.5 size-4 shrink-0" />
          <span>请求失败：{chat.error.message}</span>
        </div>
      )}

      <div className="p-4">
        <PromptInput onSubmit={handleSubmit}>
          <PromptInputBody>
            <PromptInputTextarea
              onKeyDown={(event) => {
                // 生成中拦截 Enter 提交，避免草稿被清掉又发不出去
                if (event.key === 'Enter' && !event.shiftKey && busy) {
                  event.preventDefault()
                }
              }}
              placeholder="输入消息，Enter 发送，Shift+Enter 换行"
            />
          </PromptInputBody>
          <PromptInputFooter>
            <PromptInputTools>
              {busy && (
                <span className="text-muted-foreground text-xs">
                  {chat.status === 'submitted' ? '已提交…' : '生成中…'}
                </span>
              )}
            </PromptInputTools>
            <PromptInputSubmit onStop={handleStop} status={chat.status} />
          </PromptInputFooter>
        </PromptInput>
      </div>
    </div>
  )
}

export default App
