import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { DefaultChatTransport, readUIMessageStream } from 'ai';

const mockMode = process.argv.includes('--mock');
const root = fileURLToPath(new URL('../../', import.meta.url));
let child;
let mock;
let api = process.env.CHAT_API_URL ?? 'http://127.0.0.1:3001/api/chat';

function modelReply(delta, reason) {
  const chunk = (delta, finish_reason) => `data: ${JSON.stringify({
    id: 'mock', object: 'chat.completion.chunk', created: 0, model: 'gpt-4.1',
    choices: [{ index: 0, delta, finish_reason }],
  })}\n\n`;
  return chunk(delta, null) + chunk({}, reason) + 'data: [DONE]\n\n';
}

async function startMock() {
  let count = 0;
  mock = createServer(async (req, res) => {
    let body = '';
    for await (const chunk of req) body += chunk;
    const request = JSON.parse(body);
    count += 1;
    res.writeHead(200, { 'Content-Type': 'text/event-stream' });
    if (count === 1) {
      res.end(modelReply({ tool_calls: [{ index: 0, id: 'add-1', type: 'function', function: { name: 'add', arguments: '{"a":3,"b":5}' } }] }, 'tool_calls'));
    } else {
      assert(request.messages.some(message => message.role === 'tool' && message.tool_call_id === 'add-1'), 'model history lost the tool exchange');
      res.end(modelReply({ content: count === 2 ? '8' : 'Previous sum was 8.' }, 'stop'));
    }
  });
  await new Promise(resolve => mock.listen(0, '127.0.0.1', resolve));
  // Reserve an ephemeral port, then hand it to the Rust process.
  const portProbe = createServer();
  await new Promise(resolve => portProbe.listen(0, '127.0.0.1', resolve));
  const port = portProbe.address().port;
  await new Promise(resolve => portProbe.close(resolve));
  api = `http://127.0.0.1:${port}/api/chat`;
  child = spawn('cargo', ['run', '-p', 'server'], {
    cwd: root,
    env: { ...process.env, SERVER_ADDR: `127.0.0.1:${port}`, MODEL: 'openai::gpt-4.1', OPENAI_API_KEY: 'mock-key', API_BASE_URL: `http://127.0.0.1:${mock.address().port}/v1/`, AGENT_MAX_STEPS: '6' },
    stdio: ['ignore', 'inherit', 'inherit'],
    windowsHide: true,
  });
  child.on('error', error => { console.error(error); process.exitCode = 1; });
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null || process.exitCode) throw new Error('Rust server failed to start');
    try {
      const response = await fetch(new URL('/health', api), { signal: AbortSignal.timeout(1000) });
      if (response.ok) return;
    } catch { /* Server is still compiling or starting. */ }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  throw new Error('Timed out waiting for Rust server');
}

async function turn(messages) {
  const transport = new DefaultChatTransport({ api });
  const stream = await transport.sendMessages({
    trigger: 'submit-message', chatId: 'protocol-check', messages,
    abortSignal: AbortSignal.timeout(30_000),
  });
  let final;
  for await (const message of readUIMessageStream({ stream, terminateOnError: true })) final = message;
  assert(final?.id, 'missing assistant message id');
  assert.equal(final.role, 'assistant');
  assert(final.parts.some(part => part.type === 'text' && part.text.length > 0), 'missing text output');
  assert.equal(final.metadata?.outcome, 'finished');
  assert.match(final.metadata?.runId ?? '', /^[0-9a-f-]{36}$/);
  return final;
}

try {
  if (mockMode) await startMock();
  const user = { id: 'u1', role: 'user', parts: [{ type: 'text', text: 'Use the add tool to calculate 3 + 5.' }] };
  const assistant = await turn([user]);
  if (mockMode) {
    const tool = assistant.parts.find(part => part.type === 'tool-add');
    assert.equal(tool?.state, 'output-available');
    assert.deepEqual(tool.output, { sum: 8 });
    assert.equal(assistant.parts.filter(part => part.type === 'step-start').length, 2);
    const followup = await turn([user, assistant, { id: 'u2', role: 'user', parts: [{ type: 'text', text: 'What was the result?' }] }]);
    assert(followup.parts.some(part => part.type === 'text' && part.text.includes('8')));
  }
  console.log('AI SDK transport + UIMessage parser passed' + (mockMode ? ' (tool roundtrip and second-turn history)' : ''));
} catch (error) {
  console.error(error);
  process.exitCode = 1;
} finally {
  child?.kill();
  mock?.closeAllConnections();
  mock?.close();
}
