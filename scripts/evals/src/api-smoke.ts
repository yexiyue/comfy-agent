// Exercise the generated frontend SDK against a real Axum process and isolated PostgreSQL.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { validateUIMessages } from 'ai';
import { startMock } from './mock.js';
import { client } from '../../../apps/web/src/api/generated/client.gen.js';
import { createConversation, getConversation, getCommandResult, getRun, listConversations, controlRun } from '../../../apps/web/src/api/generated/sdk.gen.js';

const server = await startMock();
try {
  const base = new URL(server.api).origin;
  client.setConfig({ baseUrl: base });
  const served = await (await fetch(`${base}/api/openapi.json`)).json();
  const exported = JSON.parse(await readFile(new URL('../../../docs/api/openapi.json', import.meta.url), 'utf8'));
  assert.deepEqual(served, exported);
  const { data: conversation } = await createConversation({ body: {}, throwOnError: true });
  assert(!('history' in conversation));
  const { data: list } = await listConversations({ query: { limit: 100 }, throwOnError: true });
  assert(list.some(item => item.id === conversation.id));
  const requestId = randomUUID();
  const submitted = await fetch(server.api, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ id: conversation.id, expectedRevision: 0, requestId,
      message: { id: randomUUID(), role: 'user', parts: [{ type: 'text', text: 'ADD 3 5' }] } }),
  });
  assert(submitted.ok);
  await submitted.body!.cancel();
  const { data: receipt } = await getCommandResult({ path: { id: conversation.id, request_id: requestId }, throwOnError: true });
  const deadline = Date.now() + 10000;
  let finished = false;
  while (Date.now() < deadline) {
    const { data: run } = await getRun({ path: { id: receipt.runId }, throwOnError: true });
    assert(!('checkpoint' in run));
    if (run.status === 'finished') {
      assert.equal(run.statistics.modelCalls, 2);
      assert.equal(run.statistics.toolCalls, 1);
      const conflict = await controlRun({ path: { id: run.id, action: 'resume' }, body: {
        conversationId: conversation.id, expectedVersion: run.version, requestId: randomUUID(),
      } });
      assert.equal(conflict.response?.status, 409);
      assert(conflict.error?.error);
      finished = true;
      break;
    }
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert(finished, `run did not finish: ${server.log()}`);
  const { data: snapshot } = await getConversation({ path: { id: conversation.id }, throwOnError: true });
  const messages = await validateUIMessages({ messages: snapshot.messages });
  assert(messages.some(message => message.parts.some(part => part.type === 'tool-add' && part.state === 'output-available')));
  const invalid = await fetch(`${base}/api/conversations?limit=invalid`);
  assert.equal(invalid.status, 400);
  assert((await invalid.json()).error);
  console.log('Generated SDK, served OpenAPI, validated UI history and safe API errors passed');
} finally {
  await server.close();
}
