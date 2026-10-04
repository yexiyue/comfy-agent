import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { ReasoningPart } from '../src/components/chat/reasoning-part'

// Test interaction and lifecycle; Markdown rendering is owned by Streamdown.
vi.mock('streamdown', () => ({ Streamdown: ({ children }: { children: string }) => <div>{children}</div> }))
afterEach(cleanup)

describe('reasoning display', () => {
  it('opens while streaming, closes on completion, and can be reopened', () => {
    const view = render(<ReasoningPart text="Check the tool result." isStreaming />)
    expect(screen.getByRole('button').getAttribute('aria-expanded')).toBe('true')
    expect(screen.getByText('正在思考…')).toBeDefined()
    view.rerender(<ReasoningPart text="Check the tool result." isStreaming={false} />)
    expect(screen.getByRole('button').getAttribute('aria-expanded')).toBe('false')
    fireEvent.click(screen.getByRole('button', { name: '思考过程' }))
    expect(screen.getByText('Check the tool result.')).toBeDefined()
  })

  it('loads history collapsed without inventing a thinking duration', () => {
    render(<ReasoningPart text="Saved thinking" isStreaming={false} />)
    expect(screen.getByRole('button', { name: '思考过程' }).getAttribute('aria-expanded')).toBe('false')
    expect(screen.queryByText('正在思考…')).toBeNull()
  })
})
