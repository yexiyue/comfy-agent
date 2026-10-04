import { useState } from 'react'
import {
  Reasoning, ReasoningContent, ReasoningTrigger,
} from '@/components/ai-elements/reasoning'

export function ReasoningPart({ text, isStreaming }: { text: string; isStreaming: boolean }) {
  const [state, setState] = useState({ streaming: isStreaming, open: isStreaming })
  if (state.streaming !== isStreaming) {
    setState({ streaming: isStreaming, open: isStreaming })
  }

  return (
    <Reasoning open={state.open} onOpenChange={(open) => setState({ streaming: isStreaming, open })} isStreaming={isStreaming}>
      <ReasoningTrigger getThinkingMessage={(streaming) => streaming ? '正在思考…' : '思考过程'} />
      <ReasoningContent>{text}</ReasoningContent>
    </Reasoning>
  )
}
