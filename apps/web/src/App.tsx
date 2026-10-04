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
import { useDurableChat } from '@/hooks/use-durable-chat'
import { isRunning } from '@/lib/session'
import { type ChatUIMessage } from '@/lib/chat'
import { chatTools, type AnyToolPart } from '@/lib/tools'
import type { ToolUIPart, UIMessage } from 'ai'
import { CircleAlertIcon } from 'lucide-react'

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

function App() {
  const { health, address, probe } = useBackendHealth()

  const { chat, snapshot, run, sessions, operation, failure, load, control } = useDurableChat()
  const busy = chat.status === 'submitted' || chat.status === 'streaming' || isRunning(run)
  const handleStop = () => { void control('pause') }
  const handleSubmit = (message: { text?: string }) => {
    const text = message.text?.trim()
    if (!text || operation || !snapshot) return
    if (run?.status === 'paused') { void control('steer', text); return }
    if (busy || run?.status === 'needs-attention') return
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

      <div className="flex flex-wrap items-center gap-2 px-4 pb-2 text-xs">
        <select aria-label="选择会话" value={snapshot?.id ?? ''} disabled={operation} onChange={event => void load(event.target.value)} className="bg-background rounded border p-1">
          <option value="" disabled>加载会话…</option>
          {sessions.map(session => <option key={session.id} value={session.id}>会话 {session.id.slice(0, 8)}</option>)}
        </select>
        <button type="button" disabled={operation} onClick={() => void load()} className="rounded border px-2 py-1">新会话</button>
        {run && <span aria-live="polite">{run.status === 'pausing' ? '正在暂停…' : run.status === 'paused' ? '已暂停：可原样继续，或发送追加指令' : run.status === 'needs-attention' ? '外部操作结果未知，请核对后终止任务' : run.status}</span>}
        {run?.status === 'paused' && <button type="button" disabled={operation} onClick={() => void control('resume')} className="rounded border px-2 py-1">原样继续</button>}
        {run && <button type="button" disabled={operation} onClick={() => void control('cancel')} className="rounded border px-2 py-1">终止任务</button>}
      </div>
      {failure && <div role="alert" className="text-destructive px-4 text-sm">{failure}</div>}
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
                  stopped={false}
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
              disabled={operation || !snapshot || run?.status === 'needs-attention'}
              placeholder={run?.status === 'paused' ? '追加指令将接替暂停任务；原样继续请点击上方按钮' : '输入消息，Enter 发送，Shift+Enter 换行'}
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
            <PromptInputSubmit aria-label={busy ? '暂停任务' : run?.status === 'paused' ? '追加指令并重新规划' : '发送消息'} title={busy ? '暂停任务' : '发送消息'} disabled={operation || !snapshot || run?.status === 'needs-attention' || run?.status === 'pausing'} onStop={handleStop} status={busy ? 'streaming' : 'ready'} />
          </PromptInputFooter>
        </PromptInput>
      </div>
    </div>
  )
}

export default App
