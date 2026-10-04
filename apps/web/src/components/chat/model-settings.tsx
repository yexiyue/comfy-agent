import type { useChatSettings } from '@/hooks/use-chat-settings'
import type { ReasoningEffort } from '@/api/generated/types.gen'

const effortLabels = { low: '低 · 更快', high: '高 · 更深入', max: '最高 · 更耗时' }
export function ModelSettings({ settings, disabled }: { settings: ReturnType<typeof useChatSettings>; disabled: boolean }) {
  const efforts = settings.models.find(model => model.id === settings.model)?.reasoningEfforts ?? []
  return (
    <div className="flex flex-wrap items-center gap-2 px-4 pb-2 text-xs">
      <label className="flex items-center gap-1">模型
        <select aria-label="模型" value={settings.model ?? ''} disabled={disabled || settings.locked || settings.loading} onChange={event => settings.setModel(event.target.value)} className="bg-background rounded border p-1">
          {!settings.model && <option value="">加载模型…</option>}
          {settings.models.map(model => <option key={model.id} value={model.id}>{model.id.split('::').at(-1)}</option>)}
        </select>
      </label>
      <label className="flex items-center gap-1">推理强度
        <select aria-label="推理强度" value={settings.reasoningEffort ?? ''} disabled={disabled || settings.locked || !efforts.length} onChange={event => settings.setEffort(event.target.value as ReasoningEffort)} className="bg-background rounded border p-1">
          {!settings.reasoningEffort && <option value="">模型默认</option>}
          {efforts.map(effort => <option key={effort} value={effort}>{effortLabels[effort]}</option>)}
        </select>
      </label>
      {settings.locked && <span className="text-muted-foreground">当前任务沿用原设置</span>}
    </div>
  )
}
