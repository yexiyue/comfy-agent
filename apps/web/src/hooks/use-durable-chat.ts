import { useChat } from '@ai-sdk/react'
import { DefaultChatTransport } from 'ai'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { API_BASE, CHAT_ENDPOINT } from '@/lib/api'
import type { ChatUIMessage } from '@/lib/chat'
import { SessionRequests } from '@/lib/session-requests'
import {
  isRunning,
  replayMessages,
  submission,
  type Run,
  type Snapshot,
} from '@/lib/session'

export function useDurableChat() {
  const [snapshot, setSnapshot] = useState<Snapshot<ChatUIMessage> | null>(null)
  const [run, setRun] = useState<Run | null>(null)
  const [sessions, setSessions] = useState<Snapshot<ChatUIMessage>[]>([])
  const [operation, setOperation] = useState(false)
  const [loading, setLoading] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)
  const snapshotRef = useRef(snapshot)
  const runRef = useRef(run)
  const loadRef = useRef<(id?: string) => Promise<void>>(async () => {})
  const requests = useRef(new SessionRequests())
  const polling = useRef(false)
  const operationRef = useRef(false)

  const transport = useMemo(
    () =>
      new DefaultChatTransport<ChatUIMessage>({
        api: CHAT_ENDPOINT,
        prepareSendMessagesRequest: ({ messages }) => ({
          body: submission(snapshotRef.current, messages, crypto.randomUUID()),
        }),
        prepareReconnectToStreamRequest: () => {
          if (!runRef.current) throw Error('没有可重连的任务')
          return { api: `${CHAT_ENDPOINT}/${runRef.current.id}/stream` }
        },
      }),
    [],
  )

  const chat = useChat<ChatUIMessage>({
    transport,
    onData: (part) => {
      if (part.type !== 'data-run-state' || requests.current.loading) return
      const next = part.data as Run
      const current = runRef.current
      if (next.conversationId !== snapshotRef.current?.id) return
      if (current && (next.id !== current.id || next.version < current.version))
        return
      runRef.current = next
      setRun(next)
    },
    onFinish: ({ message }) => {
      const id = snapshotRef.current?.id
      if (
        id &&
        !requests.current.loading &&
        message.metadata?.conversationId === id
      ) {
        void loadRef.current(id)
      }
    },
    onError: (error) => {
      if (requests.current.loading) return
      setFailure(error.message)
      void loadRef.current(snapshotRef.current?.id)
    },
  })
  const chatRef = useRef(chat)
  chatRef.current = chat

  const api = useCallback(
    async <T>(
      path: string,
      body?: unknown,
      signal?: AbortSignal,
    ): Promise<T> => {
      const response = await fetch(`${API_BASE}${path}`, {
        signal,
        ...(body === undefined
          ? {}
          : {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify(body),
            }),
      })
      const value = await response.json()
      if (!response.ok) throw Error(value.error ?? `HTTP ${response.status}`)
      return value as T
    },
    [],
  )

  const load = useCallback(
    async (id?: string) => {
      const request = requests.current.begin()
      setLoading(true)
      try {
        await chatRef.current.stop()
        if (!requests.current.isCurrent(request)) return
        const [next, list] = await Promise.all([
          id
            ? api<Snapshot<ChatUIMessage>>(
                `/api/conversations/${id}`,
                undefined,
                request.signal,
              )
            : api<Snapshot<ChatUIMessage>>(
                '/api/conversations',
                {},
                request.signal,
              ),
          api<Snapshot<ChatUIMessage>[]>(
            '/api/conversations?limit=100',
            undefined,
            request.signal,
          ),
        ])
        const active = next.activeRunId
          ? await api<Run>(
              `/api/runs/${next.activeRunId}`,
              undefined,
              request.signal,
            )
          : null
        if (!requests.current.isCurrent(request)) return
        snapshotRef.current = next
        runRef.current = active
        setSnapshot(next)
        setRun(active)
        setSessions(
          list.some((session) => session.id === next.id) ? list : [next, ...list],
        )
        setFailure(null)
        const url = new URL(window.location.href)
        url.searchParams.set('conversation', next.id)
        window.history.replaceState(null, '', url)
        chatRef.current.setMessages(replayMessages(next, active))
        if (isRunning(active)) void chatRef.current.resumeStream()
      } catch (error) {
        if (requests.current.isCurrent(request)) {
          setFailure(error instanceof Error ? error.message : '会话加载失败')
        }
      } finally {
        if (requests.current.finish(request)) setLoading(false)
      }
    },
    [api],
  )
  loadRef.current = load

  useEffect(() => {
    void load(
      new URL(window.location.href).searchParams.get('conversation') ??
        undefined,
    )
    return () => {
      requests.current.dispose()
      void chatRef.current.stop()
    }
  }, [load])

  useEffect(() => {
    const timer = window.setInterval(async () => {
      const current = runRef.current
      if (
        !current ||
        requests.current.loading ||
        operationRef.current ||
        polling.current
      )
        return
      const request = requests.current.capture()
      polling.current = true
      try {
        const latest = await api<Run>(
          `/api/runs/${current.id}`,
          undefined,
          request.signal,
        )
        if (
          !requests.current.isCurrent(request) ||
          runRef.current?.id !== current.id
        )
          return
        if (latest.conversationId !== snapshotRef.current?.id) return
        if (latest.version < (runRef.current?.version ?? 0)) return
        if (
          latest.generation !== current.generation ||
          latest.status !== current.status
        ) {
          await loadRef.current(latest.conversationId)
        } else {
          runRef.current = latest
          setRun(latest)
        }
      } catch (error) {
        if (requests.current.isCurrent(request)) {
          setFailure(error instanceof Error ? error.message : '任务查询失败')
        }
      } finally {
        polling.current = false
      }
    }, 1000)
    return () => window.clearInterval(timer)
  }, [api])

  const control = useCallback(
    async (action: string, text?: string) => {
      const current = runRef.current
      if (!current || operationRef.current || requests.current.loading) return
      const request = requests.current.capture()
      const revision = snapshotRef.current?.revision
      operationRef.current = true
      setOperation(true)
      try {
        const latest = await api<Run>(
          `/api/runs/${current.id}`,
          undefined,
          request.signal,
        )
        if (
          !requests.current.isCurrent(request) ||
          runRef.current?.id !== current.id
        )
          return
        const extra = text
          ? {
              expectedRevision: revision,
              message: {
                id: crypto.randomUUID(),
                role: 'user',
                parts: [{ type: 'text', text }],
              },
            }
          : {}
        await api(
          `/api/runs/${current.id}/${action}`,
          {
            conversationId: latest.conversationId,
            expectedVersion: latest.version,
            requestId: crypto.randomUUID(),
            ...extra,
          },
          request.signal,
        )
        if (requests.current.isCurrent(request))
          await loadRef.current(latest.conversationId)
      } catch (error) {
        if (requests.current.isCurrent(request)) {
          await loadRef.current(current.conversationId)
          if (snapshotRef.current?.id === current.conversationId) {
            setFailure(error instanceof Error ? error.message : '任务操作失败')
          }
        }
      } finally {
        operationRef.current = false
        setOperation(false)
      }
    },
    [api],
  )

  return {
    chat,
    snapshot,
    run,
    sessions,
    operation: operation || loading,
    failure,
    load,
    control,
  }
}
