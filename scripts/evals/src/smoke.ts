import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir,writeFile } from 'node:fs/promises';
import { createClient } from '@arizeai/phoenix-client';
import { getSpans } from '@arizeai/phoenix-client/spans';
import { startMock } from './mock.js';
const baseUrl=process.env.PHOENIX_URL??'http://localhost:6006';
const client=createClient({options:{baseUrl}});
const prefix=randomUUID();
const input=(text:string,id=prefix)=>({id,trigger:'submit-message',messages:[{id:'u',role:'user',parts:[{type:'text',text}]}]});
const server=await startMock(true);
const send=(text:string,id=prefix,signal?:AbortSignal)=>fetch(server.api,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(input(text,id)),signal});
try {
  const responses=await Promise.all(['Say hello','ADD 3 5','ADD 9223372036854775807 1','FAIL','ADD 1 2 UNKNOWN','FLOOD'].map((text,i)=>send(text,`${prefix}-${i}`).then(r=>r.text())));
  assert(responses[0].includes('hello'));assert(responses[1].includes('tool-output-available'));assert(responses[2].includes('tool-output-error'));assert(responses[3].includes('Model request failed'));
  assert(responses[5].includes('stream buffer exceeded'));
  const cancel=new AbortController();const pending=await send('STALL',`${prefix}-cancel`,cancel.signal);await pending.body!.getReader().read();cancel.abort();
  await new Promise(r=>setTimeout(r,100));
  const shutdown=await send('STALL',`${prefix}-shutdown`);const ended=shutdown.text();
  await server.close();assert((await ended).includes('abort'));
} finally {if(server.child.exitCode===null)await server.close();}
const limited=await startMock(true,1);
try {assert((await (await fetch(limited.api,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(input('ADD 3 5',`${prefix}-limit`))})).text()).includes('step-limit'));} finally {await limited.close();}
// These direct checks have no experiment parent; Phoenix retains the configured eval project.
let spans=(await getSpans({client,project:{projectName:process.env.PHOENIX_PROJECT_NAME??'comfy-agent-local'},limit:200})).spans;
let roots=spans.filter(s=>s.name==='agent.run' && String(s.attributes?.['session.id']).startsWith(prefix));
for (let attempt=0;roots.length<9 && attempt<20;attempt++) { await new Promise(r=>setTimeout(r,200)); spans=(await getSpans({client,project:{projectName:process.env.PHOENIX_PROJECT_NAME??'comfy-agent-local'},limit:200})).spans; roots=spans.filter(s=>s.name==='agent.run' && String(s.attributes?.['session.id']).startsWith(prefix)); }
assert.equal(roots.length,9);
const outcomes=roots.map(s=>s.attributes?.['agent.outcome']);
for (const expected of ['finished','model-error','cancelled','shutdown','step-limit','queue-overflow']) assert(outcomes.includes(expected),`Missing ${expected}`);
assert(new Set(roots.map(s=>s.context.trace_id)).size===9);
assert(spans.some(s=>s.span_kind==='LLM'&&s.attributes?.['llm.token_count.total']===14));
assert(spans.some(s=>s.span_kind==='TOOL'&&s.status_code==='ERROR'));
for(const span of spans)assert(!('input.value' in (span.attributes??{}) || 'output.value' in (span.attributes??{})),'Default content capture leaked');
await mkdir('outputs',{recursive:true});await writeFile('outputs/phoenix-smoke.json',JSON.stringify({prefix,outcomes,spans:spans.length},null,2));
console.log(JSON.stringify({prefix,outcomes,spans:spans.length}));
