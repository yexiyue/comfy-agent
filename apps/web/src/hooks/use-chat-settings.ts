import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { getChatConfigOptions } from '@/api/generated/@tanstack/react-query.gen'
import type { ReasoningEffort } from '@/api/generated/types.gen'
import type { Run } from '@/lib/session'

export function useChatSettings(run: Run | null) {
  const query = useQuery(getChatConfigOptions())
  const [choice, setChoice] = useState<{ model: string; reasoningEffort?: ReasoningEffort } | null>(null)
  const models = query.data?.models ?? []
  const locked = !!run && ['queued', 'running', 'pausing', 'paused', 'needs-attention'].includes(run.status)
  const id = locked ? run.model : choice?.model ?? query.data?.defaultModel
  const model = models.find(model => model.id === id)
  const reasoningEffort = locked ? run.reasoningEffort : choice?.reasoningEffort ?? model?.defaultReasoningEffort
  return {
    models, locked, loading: query.isPending, error: query.error,
    model: id,
    reasoningEffort,
    settings: id ? { model: id, ...(reasoningEffort ? { reasoningEffort } : {}) } : {},
    setModel: (id: string) => setChoice({ model: id, reasoningEffort: models.find(model => model.id === id)?.defaultReasoningEffort ?? undefined }),
    setEffort: (effort: ReasoningEffort) => { if (id) setChoice({ model: id, reasoningEffort: effort }) },
  }
}
