import { ToolInput, ToolOutput } from '@/components/ai-elements/tool'
import type { DynamicToolUIPart, ToolUIPart } from 'ai'
import type { ReactNode } from 'react'
import { z } from 'zod'

/** add 工具输入（与 crates/server 的 AddArgs 对齐）。 */
const AddInput = z.object({
  a: z.number().int().describe('加数'),
  b: z.number().int().describe('被加数'),
})

/**
 * 工具声明的单一注册点（AI SDK v7 经 UIMessage 的 UITools 泛型下发类型，
 * 不再向 useChat 传 tools 选项）。后端新增工具时在此补 zod schema 与输出类型，
 * parts 即窄化为 tool-<name>。
 */
export const chatToolSchemas = {
  add: { input: AddInput },
} as const

export type ChatUITools = {
  add: { input: z.infer<typeof AddInput>; output: { sum: number } }
}

export type AnyToolPart = ToolUIPart | DynamicToolUIPart

export type ToolRenderer = (part: AnyToolPart) => ReactNode

/** 工具名 → 自定义渲染；未登记的工具走默认 ToolInput/ToolOutput 样式。 */
export const chatTools: Record<string, { renderTool?: ToolRenderer }> = {
  add: {
    renderTool: (part) => {
      const input = part.input as z.infer<typeof AddInput> | undefined
      const output = part.output as { sum: number } | undefined
      if (!input) {
        return null
      }
      return (
        <div className="space-y-2">
          <ToolInput input={input} />
          {part.state === 'output-error' ? (
            <ToolOutput errorText={part.errorText} output={undefined} />
          ) : (
            output && (
              <div className="rounded-md bg-muted/50 px-3 py-2 font-mono text-xs">
                {input.a} + {input.b} ={' '}
                <span className="font-semibold">{output.sum}</span>
              </div>
            )
          )}
        </div>
      )
    },
  },
}
