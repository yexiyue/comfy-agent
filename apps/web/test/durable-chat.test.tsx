import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { client } from '../src/api/generated/client.gen'
import '../src/api/client'
import { getRunOptions } from '../src/api/generated/@tanstack/react-query.gen'
import type {
  ConversationView,
  RunDetail,
} from '../src/api/generated/types.gen'
import { useDurableChat } from '../src/hooks/use-durable-chat'

const chat = vi.hoisted(() => ({
  stop: vi.fn(async () => {}),
  resumeStream: vi.fn(async () => {}),
  setMessages: vi.fn(),
}))
vi.mock('@ai-sdk/react', () => ({ useChat: () => chat }))

const conversation = (
  id: string,
  activeRunId: string | null = null,
): ConversationView => ({
  id,
  revision: 0,
  activeRunId,
  parentId: null,
  messages: [
    { id: `${id}-user`, role: 'user', parts: [{ type: 'text', text: id }] },
  ],
})
const run = (id: string, conversationId: string): RunDetail => ({
  id,
  conversationId,
  assistantId: `${id}-assistant`,
  status: 'paused',
  version: 1,
  generation: 1,
  steps: 0,
  attemptId: null,
  error: null,
  supersedes: null,
  attempts: [],
  statistics: {
    modelCalls: 0,
    toolCalls: 0,
    knownTokens: 0,
    usageComplete: true,
  },
})
const response = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { 'content-type': 'application/json' },
  })
function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => {
    resolve = done
  })
  return { promise, resolve }
}
let cache: QueryClient
function render() {
  cache = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return renderHook(() => useDurableChat(), {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={cache}>{children}</QueryClientProvider>
    ),
  })
}
beforeEach(() => {
  window.history.replaceState(null, '', '/?conversation=A')
  vi.clearAllMocks()
  client.setConfig({ baseUrl: 'http://localhost:3001' })
})
afterEach(() => {
  cleanup()
  cache?.clear()
})

describe('durable chat with generated queries', () => {
  it('ignores an old A poll after selecting B and controls only B', async () => {
    const delayed = deferred<Response>()
    let delayA = false
    const controls: string[] = []
    client.setConfig({
      fetch: vi.fn(async (request: RequestInfo | URL) => {
        const url = new URL(
          request instanceof Request ? request.url : String(request),
        )
        const method = request instanceof Request ? request.method : 'GET'
        if (url.pathname === '/api/conversations')
          return response([
            conversation('A', 'run-A'),
            conversation('B', 'run-B'),
          ])
        if (url.pathname === '/api/conversations/A')
          return response(conversation('A', 'run-A'))
        if (url.pathname === '/api/conversations/B')
          return response(conversation('B', 'run-B'))
        if (url.pathname === '/api/runs/run-A')
          return delayA ? delayed.promise : response(run('run-A', 'A'))
        if (url.pathname === '/api/runs/run-B')
          return response(run('run-B', 'B'))
        if (method === 'POST') {
          controls.push(url.pathname)
          return response(run('run-B', 'B'))
        }
        throw Error(`Unexpected ${url.pathname}`)
      }),
    })
    const hook = render()
    await waitFor(() => expect(hook.result.current.run?.id).toBe('run-A'))
    delayA = true
    const old = cache.fetchQuery({
      ...getRunOptions({ path: { id: 'run-A' } }),
      staleTime: 0,
    })
    await act(async () => {
      await hook.result.current.load('B')
    })
    delayed.resolve(
      response({ ...run('run-A', 'A'), version: 2, status: 'running' }),
    )
    await act(async () => {
      await old
    })
    expect(hook.result.current.snapshot?.id).toBe('B')
    expect(hook.result.current.run?.id).toBe('run-B')
    await act(async () => {
      await hook.result.current.control('cancel')
    })
    expect(controls).toEqual(['/api/runs/run-B/cancel'])
  })

  it('keeps a newly created conversation visible while the list is stale', async () => {
    client.setConfig({
      fetch: vi.fn(async (request: RequestInfo | URL) => {
        const req = request as Request
        if (req.method === 'POST') return response(conversation('new'))
        if (new URL(req.url).pathname === '/api/conversations/A')
          return response(conversation('A'))
        return response([conversation('A')])
      }),
    })
    const hook = render()
    await waitFor(() => expect(hook.result.current.snapshot?.id).toBe('A'))
    await act(async () => {
      await hook.result.current.load()
    })
    expect(hook.result.current.snapshot?.id).toBe('new')
    expect(hook.result.current.sessions.map((item) => item.id)).toEqual([
      'new',
      'A',
    ])
  })

  it('replays SDK-validated messages and surfaces a generated-client conflict', async () => {
    client.setConfig({
      fetch: vi.fn(async (request: RequestInfo | URL) => {
        const req = request as Request
        const path = new URL(req.url).pathname
        if (req.method === 'POST')
          return response({ error: 'state or version conflict' }, 409)
        if (path === '/api/conversations')
          return response([conversation('A', 'run-A')])
        if (path === '/api/conversations/A')
          return response(conversation('A', 'run-A'))
        return response(run('run-A', 'A'))
      }),
    })
    const hook = render()
    await waitFor(() => expect(hook.result.current.run?.id).toBe('run-A'))
    expect(chat.setMessages).toHaveBeenCalledWith(conversation('A').messages)
    await act(async () => {
      await hook.result.current.control('resume')
    })
    expect(hook.result.current.failure).toBe('state or version conflict')
    expect(hook.result.current.operation).toBe(false)
  })
})
