// Deterministic local model responses, independent of server/database lifecycle.
import { createServer, type Server } from 'node:http';

export function createMockModel(onRequest: (prompt: string, count: number) => void = () => {}): Server {
  const delta = (value: unknown, reason: string | null, usage?: unknown) =>
    `data: ${JSON.stringify({ id: 'eval', object: 'chat.completion.chunk', created: 0, model: 'gpt-4.1', choices: [{ index: 0, delta: value, finish_reason: reason }], usage })}\n\n`;
  const calls = new Map<string, number>();
  return createServer(async (req, res) => {
    let raw = '';
    for await (const chunk of req) raw += chunk;
    const request = JSON.parse(raw);
    const texts = request.messages
      .filter((m: { role: string }) => m.role === 'user')
      .map((m: { content: string }) => m.content);
    const prompt = texts.at(-1) as string;
    res.writeHead(200, { 'Content-Type': 'text/event-stream' });
    const count = (calls.get(prompt) ?? 0) + 1;
    calls.set(prompt, count);
    onRequest(prompt, count);
    if (prompt.startsWith('PAUSE:') && count === 1) {
      res.write(delta({ reasoning_content: 'abandoned thinking' }, null));
      return;
    }
    if (prompt.startsWith('DELAY:')) {
      res.write(delta({ content: 'working' }, null));
      setTimeout(
        () =>
          res.end(
            delta({ content: 'done' }, null) +
              delta({}, 'stop') +
              'data: [DONE]\n\n',
          ),
        500,
      );
      return;
    }
    if (prompt === 'FAIL') {
      res.end('data: {"bad":\n\n');
      return;
    }
    if (prompt === 'STALL') {
      res.write(delta({ content: 'partial' }, null));
      req.on('close', () => {});
      res.on('close', () => {});
      return;
    }
    if (prompt === 'FLOOD') {
      res.end(
        Array.from({ length: 5000 }, () => delta({ content: 'x' }, null)).join(
          '',
        ) +
          delta({}, 'stop') +
          'data: [DONE]\n\n',
      );
      return;
    }
    const userIndex = request.messages
      .map((m: { role: string }) => m.role)
      .lastIndexOf('user');
    const lastTool = request.messages
      .slice(userIndex + 1)
      .findLast((m: { role: string }) => m.role === 'tool');
    const match = prompt.match(/ADD (-?\d+) (-?\d+)/);
    let value: unknown;
    let reason = 'stop';
    if (match && !lastTool) {
      value = {
        tool_calls: [
          {
            index: 0,
            id: `call-${userIndex}`,
            type: 'function',
            function: {
              name: prompt.includes('UNKNOWN') ? 'missing' : 'add',
              arguments: `{"a":${match[1]},"b":${match[2]}}`,
            },
          },
        ],
      };
      reason = 'tool_calls';
    } else if (lastTool) {
      const output = JSON.parse(lastTool.content);
      value = { content: output.error ? 'overflow' : String(output.sum) };
    } else if (prompt === 'Previous result?') {
      const previous = request.messages.findLast(
        (m: { role: string }) => m.role === 'tool',
      );
      value = { content: String(JSON.parse(previous.content).sum) };
    } else {
      value = {
        content: prompt.startsWith('Say ')
          ? prompt.slice(4)
          : prompt.startsWith('PAUSE:')
            ? 'resumed'
            : 'ok',
      };
    }
    res.end(
      delta({ reasoning_content: prompt.startsWith('PAUSE:') ? 'fresh thinking' : 'Plan this response.' }, null) +
      delta(value, null) +
        delta({}, reason, {
          prompt_tokens: 10,
          completion_tokens: 4,
          total_tokens: 14,
          prompt_tokens_details: { cached_tokens: 2 },
          completion_tokens_details: { reasoning_tokens: 1 },
        }) +
        'data: [DONE]\n\n',
    );
  });
}
