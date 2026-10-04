import type { UIMessage } from 'ai'
export type Run = { id: string; conversationId: string; assistantId: string; status: string; version: number; generation: number; error?: string }
export type Snapshot<M = UIMessage> = { id: string; revision: number; messages: M[]; activeRunId: string | null }
export const isRunning = (run: Run | null) => !!run && ['queued', 'running', 'pausing'].includes(run.status)
export function replayMessages<M extends { id: string }>(snapshot: Snapshot<M>, run: Run | null): M[] {
  return isRunning(run) ? snapshot.messages.filter(message => message.id !== run?.assistantId) : snapshot.messages
}
export function submission<M extends UIMessage>(snapshot: Snapshot<M> | null, messages: M[], requestId: string) {
  if (!snapshot) throw Error('会话尚未加载')
  const message = messages.at(-1)
  if (!message || message.role !== 'user' || !message.parts.length || message.parts.some(part => part.type !== 'text')) throw Error('只支持新的文字消息')
  return { id: snapshot.id, expectedRevision: snapshot.revision, requestId, message }
}
