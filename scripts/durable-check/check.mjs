// Node 22+; uses the locked official parser and the existing free local model fixture.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdir,writeFile } from 'node:fs/promises';
import { startMock } from '../evals/src/mock.ts';
import { DefaultChatTransport,readUIMessageStream } from '../ai-sdk-check/node_modules/ai/dist/index.js';
assert(process.env.TEST_DATABASE_URL?.endsWith('_test'),'Set a dedicated TEST_DATABASE_URL ending in _test');
const migrated=spawnSync('cargo',['run','-p','persistence','--bin','migrate'],{env:{...process.env,DATABASE_URL:process.env.TEST_DATABASE_URL},stdio:'inherit',windowsHide:true});assert.equal(migrated.status,0);
const built=spawnSync('cargo',['build','-p','server'],{stdio:'inherit',windowsHide:true});assert.equal(built.status,0);
const server=await startMock(process.argv.includes('--phoenix'));
const evidence=[];
const terminal=run=>['finished','step-limit','failed','cancelled','superseded'].includes(run.status);
const endpoint=path=>new URL(path,server.api);
async function json(path,body) {const response=await fetch(endpoint(path),{signal:AbortSignal.timeout(5000),headers:{'content-type':'application/json'},...(body===undefined?{}:{method:'POST',body:JSON.stringify(body)})});assert(response.ok,`${path}: ${response.status} ${await (!response.ok?response.text():Promise.resolve(''))}`);return response.json();}
async function wait(runId,predicate,timeout=10000){const deadline=Date.now()+timeout;let last;while(Date.now()<deadline){last=await json(`/api/runs/${runId}`);if(predicate(last))return last;await new Promise(resolve=>setTimeout(resolve,50));}throw Error(`State timeout for ${runId}; last snapshot: ${JSON.stringify(last)}: ${server.log()}`);}
async function submit(text){const conversation=await json('/api/conversations',{});const requestId=randomUUID();const response=await fetch(server.api,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({id:conversation.id,expectedRevision:0,requestId,message:{id:randomUUID(),role:'user',parts:[{type:'text',text}]}})});assert(response.ok);await response.body.cancel();const receipt=await json(`/api/conversations/${conversation.id}/commands/${requestId}`);return {conversation,runId:receipt.runId,prompt:text};}
async function control(runId,type,extra={}){for(let retry=0;retry<10;retry++){const run=await json(`/api/runs/${runId}`);const response=await fetch(endpoint(`/api/runs/${runId}/${type}`),{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({conversationId:run.conversationId,expectedVersion:run.version,requestId:randomUUID(),...extra})});if(response.status===409){await new Promise(r=>setTimeout(r,20));continue;}if(!response.ok)throw Error(await response.text());return response.json();}throw Error('Control conflicts did not settle');}
async function replay(runId){const transport=new DefaultChatTransport({api:server.api,prepareReconnectToStreamRequest:()=>({api:endpoint(`/api/chat/${runId}/stream`).toString()})});const stream=await transport.reconnectToStream({chatId:runId,abortSignal:AbortSignal.timeout(10000)});let last;for await(const message of readUIMessageStream({stream,terminateOnError:true}))last=message;assert(last);return last;}
try {
  const closed=await submit(`DELAY:${randomUUID()}`);await wait(closed.runId,run=>run.status==='finished');const completed=await replay(closed.runId);assert(completed.parts.some(part=>part.type==='text'&&part.text==='workingdone'));evidence.push({scenario:'browser disconnect',runId:closed.runId,status:'finished'});
  const paused=await submit(`PAUSE:${randomUUID()}`);await wait(paused.runId,run=>run.statistics.modelCalls===1);await server.waitForModel(paused.prompt);
  await control(paused.runId,'pause');const stopped=await wait(paused.runId,run=>run.status==='paused');await replay(paused.runId);
  await control(paused.runId,'resume');const resumed=await wait(paused.runId,terminal);assert.equal(resumed.status,'finished');assert.equal(resumed.steps,1);assert.equal(resumed.statistics.modelCalls,2);assert.equal(resumed.statistics.usageComplete,false);assert.notEqual(resumed.attemptId,stopped.attemptId);
  const message=await replay(paused.runId);assert.equal(message.parts.filter(part=>part.type==='text').map(part=>part.text).join(''),'resumed');assert(!JSON.stringify(message).includes('abandoned'));
  const statsBefore=resumed.statistics;await Promise.all([replay(paused.runId),replay(paused.runId)]);assert.deepEqual((await json(`/api/runs/${paused.runId}`)).statistics,statsBefore);evidence.push({scenario:'model pause/resume/replay',runId:paused.runId,attempts:resumed.attempts.map(a=>a.id),statistics:statsBefore});
  for(const force of [false,true]){
    const task=await submit(`PAUSE:${randomUUID()}`);await wait(task.runId,run=>run.statistics.modelCalls===1);await server.waitForModel(task.prompt);await server.restart(force);const recovered=await wait(task.runId,terminal,15000);assert.equal(recovered.status,'finished');assert.equal(recovered.attempts.length,2);assert.equal(recovered.statistics.modelCalls,2);assert.equal(recovered.steps,1);assert(!JSON.stringify(await replay(task.runId)).includes('abandoned'));evidence.push({scenario:force?'forced process exit':'graceful shutdown',runId:task.runId,attempts:recovered.attempts.map(a=>({id:a.id,outcome:a.outcome}))});await server.restart(false);
  }
  const steering=await submit(`PAUSE:${randomUUID()}`);await wait(steering.runId,run=>run.statistics.modelCalls===1);await server.waitForModel(steering.prompt);await control(steering.runId,'pause');await wait(steering.runId,run=>run.status==='paused');const snapshot=await json(`/api/conversations/${steering.conversation.id}`);const next=await control(steering.runId,'steer',{expectedRevision:snapshot.revision,message:{id:randomUUID(),role:'user',parts:[{type:'text',text:'Say new plan'}]}});await wait(next.id,terminal);assert.equal((await json(`/api/runs/${steering.runId}`)).status,'superseded');assert((await replay(next.id)).parts.some(part=>part.type==='text'&&part.text==='new plan'));evidence.push({scenario:'steer',oldRun:steering.runId,newRun:next.id});
  const cancelled=await submit('STALL');await wait(cancelled.runId,run=>run.statistics.modelCalls===1);
  const heartbeatResponse=await fetch(endpoint(`/api/chat/${cancelled.runId}/stream`),{signal:AbortSignal.timeout(12000)});const reader=heartbeatResponse.body.getReader();let raw='';
  while(!/^:\s*ping\r?$/m.test(raw)){const {value,done}=await reader.read();assert(!done);raw+=new TextDecoder().decode(value);}
  await reader.cancel();assert.equal((await json(`/api/runs/${cancelled.runId}`)).status,'running');
  await control(cancelled.runId,'cancel');assert.equal((await json(`/api/runs/${cancelled.runId}`)).status,'cancelled');await replay(cancelled.runId);evidence.push({scenario:'explicit cancel',runId:cancelled.runId,status:'cancelled'});
  const flooded=await submit('FLOOD');const overflow=await wait(flooded.runId,terminal);assert.equal(overflow.status,'failed');evidence.push({scenario:'bounded callback overflow',runId:flooded.runId});
  const failed=await submit('FAIL');const failure=await wait(failed.runId,terminal);assert.equal(failure.status,'failed');await server.restart(false);await new Promise(r=>setTimeout(r,500));assert.equal((await json(`/api/runs/${failed.runId}`)).statistics.modelCalls,1);evidence.push({scenario:'business failure is not retried',runId:failed.runId});
  await mkdir('outputs',{recursive:true});await writeFile('outputs/durable-check.json',JSON.stringify(evidence,null,2));console.log(`Durable process/protocol matrix passed (${evidence.length} scenarios)`);
} catch(error) {console.error(server.log());throw error;} finally {await server.close();}

const limited=await startMock(false,1);
try {
  const c=await (await fetch(new URL('/api/conversations',limited.api),{method:'POST',headers:{'content-type':'application/json'},body:'{}'})).json();
  const transport=new DefaultChatTransport({api:limited.api,prepareSendMessagesRequest:()=>({body:{id:c.id,expectedRevision:0,requestId:randomUUID(),message:{id:randomUUID(),role:'user',parts:[{type:'text',text:'ADD 3 5'}]}}})});
  const stream=await transport.sendMessages({chatId:c.id,trigger:'submit-message',messages:[],abortSignal:AbortSignal.timeout(10000)});let last;
  for await(const message of readUIMessageStream({stream,terminateOnError:true}))last=message;
  assert.equal(last.metadata.outcome,'step-limit');assert.equal(last.metadata.steps,1);assert(last.parts.some(p=>p.type==='tool-add'&&p.state==='output-available'));
  console.log('Official parser step-limit gate passed');
}finally{await limited.close();}
