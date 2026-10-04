import { test } from 'node:test'
import assert from 'node:assert/strict'
import { submission, replayMessages, isRunning } from '../src/lib/session.ts'
const snapshot = { id: 'conversation', revision: 7, messages: [{id:'user'}, {id:'assistant'}], activeRunId: 'run' }
const run = { id: 'run', assistantId: 'assistant', status: 'running' }
test('replay replaces only the current assistant, including an abandoned draft', () => {
  assert.deepEqual(replayMessages(snapshot, run), [{id:'user'}])
  assert.deepEqual(replayMessages(snapshot, {...run,status:'paused'}), snapshot.messages)
  assert.deepEqual(replayMessages(snapshot, null), snapshot.messages)
  assert(isRunning({...run,status:'pausing'}))
})
test('incremental submission carries revision and idempotency key without client history', () => {
  const message = {id:'new',role:'user',parts:[{type:'text',text:'next'}]}
  const body = submission(snapshot, [...snapshot.messages,message], 'retry-key')
  assert.deepEqual(body,{id:'conversation',expectedRevision:7,requestId:'retry-key',message})
  assert.equal(body.messages,undefined)
  assert.throws(() => submission(null,[message],'key'),/加载/)
  assert.throws(() => submission(snapshot,[{...message,parts:[{type:'file'}]}],'key'),/文字/)
  assert.throws(() => submission(snapshot,[{...message,role:'assistant'}],'key'),/文字/)
})
