import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { DefaultChatTransport, readUIMessageStream } from 'ai';

const mockMode = process.argv.includes('--mock');
const root = fileURLToPath(new URL('../../', import.meta.url));
let child;
let mock;
let fixtureDatabase;
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
      res.write(modelReply({ reasoning_content: 'I will use addition.' }, null).split('\n\n')[0] + '\n\n');
      res.end(modelReply({ tool_calls: [{ index: 0, id: 'add-1', type: 'function', function: { name: 'add', arguments: '{"a":3,"b":5}' } }] }, 'tool_calls'));
    } else {
      assert.equal(request.messages.find(message => message.role === 'assistant')?.reasoning_content, 'I will use addition.');
      assert(request.messages.some(message => message.role === 'tool' && message.tool_call_id === 'add-1'), 'model history lost the tool exchange');
      res.write(modelReply({ reasoning_content: 'The result is eight.' }, null).split('\n\n')[0] + '\n\n');
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
  const database=process.env.TEST_DATABASE_URL;
  assert(database?.endsWith('_test'), 'Set TEST_DATABASE_URL to a dedicated migrated _test PostgreSQL database');
  const created=spawnSync('cargo',['run','--quiet','-p','persistence','--bin','test_database','--','create'],{cwd:root,env:process.env,encoding:'utf8',windowsHide:true});assert.equal(created.status,0);fixtureDatabase=created.stdout.trim();assert(/^agent_fixture_[a-f0-9]{32}_test$/.test(fixtureDatabase));
  const childDatabase=database.slice(0,database.lastIndexOf('/')+1)+fixtureDatabase;
  const env={...process.env,DATABASE_URL:childDatabase,OTEL_ENABLED:'false'};
  const migration=spawnSync('cargo',['run','-p','persistence','--bin','migrate'],{cwd:root,env,stdio:'inherit',windowsHide:true});
  assert.equal(migration.status,0);
  const build=spawnSync('cargo',['build','-p','server'],{cwd:root,env,stdio:'inherit',windowsHide:true});assert.equal(build.status,0);
  child = spawn(`${root}/target/debug/server${process.platform==='win32'?'.exe':''}`, [], {
    cwd: root,
    env: { ...env, SERVER_SHUTDOWN_STDIN:'true', SERVER_ADDR: `127.0.0.1:${port}`, MODEL: 'openai::gpt-4.1', CHAT_MODELS: '', OPENAI_API_KEY: 'mock-key', API_BASE_URL: `http://127.0.0.1:${mock.address().port}/v1/`, AGENT_MAX_STEPS: '6' },
    stdio: ['pipe', 'inherit', 'inherit'],
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

let conversation;
async function turn(messages) {
  const snapshot=await (await fetch(new URL(`/api/conversations/${conversation.id}`,api))).json();
  const transport = new DefaultChatTransport({ api,prepareSendMessagesRequest:({messages})=>({body:{id:conversation.id,expectedRevision:snapshot.revision,requestId:randomUUID(),message:messages.at(-1)}}) });
  const stream = await transport.sendMessages({
    trigger: 'submit-message', chatId: conversation.id, messages,
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
  const created=await fetch(new URL('/api/conversations',api),{method:'POST',headers:{'content-type':'application/json'},body:'{}'});
  assert(created.ok);conversation=await created.json();
  const user = { id: 'u1', role: 'user', parts: [{ type: 'text', text: 'Use the add tool to calculate 3 + 5.' }] };
  const assistant = await turn([user]);
  if (mockMode) {
    assert.deepEqual(assistant.parts.filter(p => p.type === 'reasoning').map(p => [p.text,p.state]), [['I will use addition.','done'],['The result is eight.','done']]);
    const tool = assistant.parts.find(part => part.type === 'tool-add');
    assert.equal(tool?.state, 'output-available');
    assert.deepEqual(tool.output, { sum: 8 });
    assert.equal(assistant.parts.filter(part => part.type === 'step-start').length, 2);
    const followup = await turn([user, assistant, { id: 'u2', role: 'user', parts: [{ type: 'text', text: 'What was the result?' }] }]);
    assert(followup.parts.some(part => part.type === 'text' && part.text.includes('8')));
  }
  const replayTransport=new DefaultChatTransport({api,prepareReconnectToStreamRequest:({id})=>({api:new URL(`/api/chat/${id}/stream`,api).toString()})});
  const replayStream=await replayTransport.reconnectToStream({chatId:assistant.metadata.runId});
  let replay;
  for await (const value of readUIMessageStream({stream:replayStream,terminateOnError:true})) replay=value;
  assert.equal(replay.id,assistant.id);assert.deepEqual(replay.parts,assistant.parts);
  console.log('AI SDK transport + UIMessage parser passed' + (mockMode ? ' (tool roundtrip and second-turn history)' : ''));
} catch (error) {
  console.error(error);
  process.exitCode = 1;
} finally {
  if (child?.exitCode===null) {
    child.stdin.write('shutdown\n');
    await new Promise(resolve=>{const timer=setTimeout(()=>{child.kill();resolve();},12000);child.once('exit',()=>{clearTimeout(timer);resolve();});});
  }
  mock?.closeAllConnections();
  mock?.close();
  if(fixtureDatabase){const dropped=spawnSync('cargo',['run','--quiet','-p','persistence','--bin','test_database','--','drop',fixtureDatabase],{cwd:root,env:process.env,encoding:'utf8',windowsHide:true});assert.equal(dropped.status,0);}

}
