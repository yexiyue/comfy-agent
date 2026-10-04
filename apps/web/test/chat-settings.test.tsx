import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { afterEach, expect, it, vi } from 'vitest'
import { client } from '../src/api/generated/client.gen'
import { useChatSettings } from '../src/hooks/use-chat-settings'
import type { Run } from '../src/lib/session'

const config = { defaultModel: 'bigmodel::glm-5.3-flash', models: [
  { id: 'bigmodel::glm-5.3-flash', reasoningEfforts: ['low', 'high', 'max'], defaultReasoningEffort: 'low' },
  { id: 'bigmodel::glm-4.7', reasoningEfforts: [], defaultReasoningEffort: null },
] }
afterEach(cleanup)

it('uses server defaults, allows selecting effort, and resets unsupported effort on model change', async () => {
  client.setConfig({ baseUrl: 'http://localhost:3001', fetch: vi.fn(async () => new Response(JSON.stringify(config), { headers: { 'content-type': 'application/json' } })) })
  const cache = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  const hook = renderHook(({ run }: { run: Run | null }) => useChatSettings(run), {
    initialProps: { run: null as Run | null },
    wrapper: ({ children }: { children: ReactNode }) => <QueryClientProvider client={cache}>{children}</QueryClientProvider>,
  })
  await waitFor(() => expect(hook.result.current.reasoningEffort).toBe('low'))
  act(() => hook.result.current.setEffort('high'))
  expect(hook.result.current.settings).toEqual({ model: 'bigmodel::glm-5.3-flash', reasoningEffort: 'high' })
  act(() => hook.result.current.setModel('bigmodel::glm-4.7'))
  expect(hook.result.current.settings).toEqual({ model: 'bigmodel::glm-4.7' })
  const paused = { id: 'task', status: 'paused', model: 'bigmodel::glm-5.3-flash', reasoningEffort: 'max' } as Run
  hook.rerender({ run: paused })
  expect(hook.result.current.locked).toBe(true)
  expect(hook.result.current.settings).toEqual({ model: paused.model, reasoningEffort: 'max' })
  hook.rerender({ run: { ...paused, status: 'finished' } })
  expect(hook.result.current.locked).toBe(false)
  expect(hook.result.current.model).toBe('bigmodel::glm-4.7')
  cache.clear()
})
