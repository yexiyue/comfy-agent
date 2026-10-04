import { useChat } from '@ai-sdk/react'
import { DefaultChatTransport, validateUIMessages } from 'ai'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react'
import {
  controlRunMutation,
  createConversationMutation,
  getConversationOptions,
  getRunOptions,
  listConversationsOptions,
} from '@/api/generated/@tanstack/react-query.gen'
import type { ConversationView, RunAction } from '@/api/generated/types.gen'
import { CHAT_ENDPOINT } from '@/api/client'
import type { ChatUIMessage } from '@/lib/chat'
import { SessionRequests } from '@/lib/session-requests'
import {
  isRunning,
  replayMessages,
  submission,
  type Run,
  type Snapshot,
} from '@/lib/session'

async function snapshotFrom(
  view: ConversationView,
): Promise<Snapshot<ChatUIMessage>> {
  return {
    ...view,
    messages: await validateUIMessages<ChatUIMessage>({
      messages: view.messages,
    }),
  }
}
const errorText = (error: unknown) =>
  error instanceof Error ? error.message : '后端请求失败'

export function useDurableChat() {
  const queryClient = useQueryClient()
  const [snapshot, setSnapshot] = useState<Snapshot<ChatUIMessage> | null>(null)
  const [run, setRun] = useState<Run | null>(null)
  const [loading, setLoading] = useState(false)
  const [operating, setOperating] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)
  const snapshotRef = useRef(snapshot)
  const runRef = useRef(run)
  const loadRef = useRef<(id?: string) => Promise<void>>(async () => {})
  const [requests] = useState(() => new SessionRequests())
  const operationRef = useRef(false)
  const { mutateAsync: create, isPending: creating } = useMutation(
    createConversationMutation(),
  )
  const { mutateAsync: command, isPending: controlling } =
    useMutation(controlRunMutation())
  const list = useQuery(listConversationsOptions({ query: { limit: 100 } }))
  const runQuery = useQuery({
    ...getRunOptions({ path: { id: run?.id ?? '' } }),
    enabled: !!run && !loading && !controlling,
    refetchInterval: (query) => {
      const status = query.state.data?.status
      return status &&
        [
          'finished',
          'failed',
          'cancelled',
          'superseded',
          'step-limit',
        ].includes(status)
        ? false
        : 1000
    },
  })

  const [transport] = useState(
    // oxlint-disable-next-line react/refs -- The constructor stores callbacks; refs are read only when a request starts.
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
  )
  const chat = useChat<ChatUIMessage>({
    transport,
    onData: (part) => {
      if (part.type !== 'data-run-state' || requests.isLoading()) return
      const next = part.data
      const current = runRef.current
      if (next.conversationId !== snapshotRef.current?.id) return
      if (current && (next.id !== current.id || next.version < current.version))
        return
      runRef.current = next
      setRun(next)
    },
    onFinish: ({ message }) => {
      const id = snapshotRef.current?.id
      if (id && !requests.isLoading() && message.metadata?.conversationId === id)
        void loadRef.current(id)
    },
    onError: (error) => {
      if (requests.isLoading()) return
      setFailure(error.message)
      void loadRef.current(snapshotRef.current?.id)
    },
  })
  const chatRef = useRef(chat)
  useLayoutEffect(() => {
    chatRef.current = chat
  }, [chat])

  const load = useCallback(
    async (id?: string) => {
      const request = requests.begin()
      setLoading(true)
      try {
        await chatRef.current.stop()
        if (!requests.isCurrent(request)) return
        const view = id
          ? await queryClient.fetchQuery({
              ...getConversationOptions({ path: { id } }),
              staleTime: 0,
            })
          : await create({ body: {}, signal: request.signal })
        const next = await snapshotFrom(view)
        const active = next.activeRunId
          ? await queryClient.fetchQuery({
              ...getRunOptions({ path: { id: next.activeRunId } }),
              staleTime: 0,
            })
          : null
        if (!requests.isCurrent(request)) return
        snapshotRef.current = next
        runRef.current = active
        setSnapshot(next)
        setRun(active)
        setFailure(null)
        const url = new URL(window.location.href)
        url.searchParams.set('conversation', next.id)
        window.history.replaceState(null, '', url)
        chatRef.current.setMessages(replayMessages(next, active))
        void queryClient.invalidateQueries({
          queryKey: listConversationsOptions({ query: { limit: 100 } })
            .queryKey,
        })
        if (isRunning(active)) void chatRef.current.resumeStream()
      } catch (error) {
        if (requests.isCurrent(request)) setFailure(errorText(error))
      } finally {
        if (requests.finish(request)) setLoading(false)
      }
    },
    [queryClient, create, requests],
  )
  useLayoutEffect(() => {
    loadRef.current = load
  }, [load])

  useEffect(() => {
    // A replay belongs to the selected conversation even if an old query finishes later.
    // oxlint-disable-next-line react/set-state-in-effect -- Mounting synchronizes the external chat stream with the persisted snapshot.
    void load(
      new URL(window.location.href).searchParams.get('conversation') ??
        undefined,
    )
    return () => {
      requests.dispose()
      void chatRef.current.stop()
    }
  }, [load, requests])

  useEffect(() => {
    const latest = runQuery.data
    const current = runRef.current
    if (!latest || !current || requests.isLoading() || operationRef.current) return
    if (
      latest.id !== current.id ||
      latest.conversationId !== snapshotRef.current?.id ||
      latest.version < current.version
    )
      return
    if (
      latest.generation !== current.generation ||
      latest.status !== current.status
    ) {
      void loadRef.current(latest.conversationId)
    } else {
      runRef.current = latest
      setRun(latest)
    }
  }, [runQuery.data, requests])

  const control = useCallback(
    async (action: RunAction, text?: string) => {
      const current = runRef.current
      if (!current || operationRef.current || requests.isLoading()) return
      const request = requests.capture()
      const revision = snapshotRef.current?.revision
      operationRef.current = true
      setOperating(true)
      try {
        const latest = await queryClient.fetchQuery({
          ...getRunOptions({ path: { id: current.id } }),
          staleTime: 0,
        })
        if (!requests.isCurrent(request) || runRef.current?.id !== current.id)
          return
        await command({
          path: { id: current.id, action },
          signal: request.signal,
          body: {
            conversationId: latest.conversationId,
            expectedVersion: latest.version,
            requestId: crypto.randomUUID(),
            ...(text
              ? {
                  expectedRevision: revision,
                  message: {
                    id: crypto.randomUUID(),
                    role: 'user' as const,
                    parts: [{ type: 'text', text }],
                  },
                }
              : {}),
          },
        })
        if (requests.isCurrent(request))
          await loadRef.current(latest.conversationId)
      } catch (error) {
        if (requests.isCurrent(request)) {
          await loadRef.current(current.conversationId)
          if (snapshotRef.current?.id === current.conversationId)
            setFailure(errorText(error))
        }
      } finally {
        operationRef.current = false
        setOperating(false)
      }
    },
    [queryClient, command, requests],
  )

  const sessions = snapshot
    ? [snapshot, ...(list.data ?? []).filter((item) => item.id !== snapshot.id)]
    : (list.data ?? [])
  const queryError = runQuery.error ?? list.error
  return {
    chat,
    snapshot,
    run,
    sessions,
    operation: loading || operating || creating || controlling,
    failure: failure ?? (queryError ? errorText(queryError) : null),
    load,
    control,
  }
}
