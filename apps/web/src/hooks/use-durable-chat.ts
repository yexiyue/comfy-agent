import { useChat } from '@ai-sdk/react'
import { DefaultChatTransport } from 'ai'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { API_BASE, CHAT_ENDPOINT } from '@/lib/api'
import type { ChatUIMessage } from '@/lib/chat'
import { isRunning, replayMessages, submission, type Run, type Snapshot } from '@/lib/session'

export function useDurableChat() {
  const [snapshot, setSnapshot] = useState<Snapshot<ChatUIMessage> | null>(null)
  const [run, setRun] = useState<Run | null>(null)
  const [sessions, setSessions] = useState<Snapshot<ChatUIMessage>[]>([])
  const [operation, setOperation] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)
  const snapshotRef = useRef(snapshot)
  const runRef = useRef(run)
  const loadRef = useRef<(id?: string) => Promise<void>>(async () => {})
  const loading = useRef(false)
  const operationRef = useRef(false)
  const transport = useMemo(() => new DefaultChatTransport<ChatUIMessage>({
    api: CHAT_ENDPOINT,
    prepareSendMessagesRequest: ({ messages }) => ({ body: submission(snapshotRef.current, messages, crypto.randomUUID()) }),
    prepareReconnectToStreamRequest: () => {
      if (!runRef.current) throw Error('没有可重连的任务')
      return { api: `${CHAT_ENDPOINT}/${runRef.current.id}/stream` }
    },
  }), [])
  const chat = useChat<ChatUIMessage>({ transport,
    onData: part => { if (part.type === 'data-run-state') { const next = part.data as Run; runRef.current = next; setRun(next) } },
    onFinish: () => { void loadRef.current(snapshotRef.current?.id) },
    onError: error => { setFailure(error.message); void loadRef.current(snapshotRef.current?.id) },
  })
  const chatRef = useRef(chat)
  chatRef.current = chat
  const api = useCallback(async <T,>(path: string, body?: unknown): Promise<T> => {
    const response = await fetch(`${API_BASE}${path}`, body === undefined ? undefined : { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
    const value = await response.json()
    if (!response.ok) throw Error(value.error ?? `HTTP ${response.status}`)
    return value as T
  }, [])
  const load = useCallback(async (id?: string) => {
    if (loading.current) return
    loading.current = true
    try {
      const next = id ? await api<Snapshot<ChatUIMessage>>(`/api/conversations/${id}`) : await api<Snapshot<ChatUIMessage>>('/api/conversations', {})
      const active = next.activeRunId ? await api<Run>(`/api/runs/${next.activeRunId}`) : null
      await chatRef.current.stop()
      snapshotRef.current = next; runRef.current = active
      setSnapshot(next); setRun(active); setFailure(null)
      const url = new URL(window.location.href); url.searchParams.set('conversation', next.id); window.history.replaceState(null, '', url)
      chatRef.current.setMessages(replayMessages(next, active))
      setSessions(await api<Snapshot<ChatUIMessage>[]>('/api/conversations?limit=100'))
      if (isRunning(active)) void chatRef.current.resumeStream()
    } catch (error) { setFailure(error instanceof Error ? error.message : '会话加载失败') }
    finally { loading.current = false }
  }, [api])
  loadRef.current = load
  useEffect(() => { void load(new URL(window.location.href).searchParams.get('conversation') ?? undefined) }, [load])
  useEffect(() => {
    const timer = window.setInterval(async () => {
      const current = runRef.current
      if (!current || loading.current || operationRef.current) return
      try {
        const latest = await api<Run>(`/api/runs/${current.id}`)
        if (latest.generation !== current.generation || latest.status !== current.status) await loadRef.current(snapshotRef.current?.id)
        else { runRef.current = latest; setRun(latest) }
      } catch (error) { setFailure(error instanceof Error ? error.message : '任务查询失败') }
    }, 1000)
    return () => window.clearInterval(timer)
  }, [api])
  const control = useCallback(async (action: string, text?: string) => {
    const current = runRef.current
    if (!current || operationRef.current) return
    operationRef.current = true; setOperation(true)
    try {
      const latest = await api<Run>(`/api/runs/${current.id}`)
      const extra = text ? { expectedRevision: snapshotRef.current?.revision, message: { id: crypto.randomUUID(), role: 'user', parts: [{ type: 'text', text }] } } : {}
      await api(`/api/runs/${current.id}/${action}`, { conversationId: latest.conversationId, expectedVersion: latest.version, requestId: crypto.randomUUID(), ...extra })
      await loadRef.current(latest.conversationId)
    } catch (error) { await loadRef.current(current.conversationId); setFailure(error instanceof Error ? error.message : '任务操作失败') }
    finally { operationRef.current = false; setOperation(false) }
  }, [api])
  return { chat, snapshot, run, sessions, operation, failure, load, control }
}
