import { test } from 'node:test';
import assert from 'node:assert/strict';
import { grade,validateCases,loadCases,Budget,execute,compare,summarize,type Case } from '../src/core.js';
import { startMock } from '../src/mock.js';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';

const c:Case={id:'text',messages:[{id:'u',role:'user',parts:[{type:'text',text:'Say hello'}]}],expected:{answer:'hello',tools:[],maxSteps:1}};
test('fixtures validate stable IDs and scoring requirements',async()=>{
  const {cases}=await loadCases(fileURLToPath(new URL('../cases.jsonl',import.meta.url)));assert.equal(cases.length,22);
  assert.throws(()=>validateCases([c,c]),/duplicate/);assert.throws(()=>validateCases([{...c,expected:{}}]),/Invalid/);
});
test('finished does not imply quality and not-applicable scores have no denominator',()=>{
  const m={id:'a',role:'assistant' as const,parts:[{type:'text' as const,text:'wrong'}],metadata:{outcome:'finished',steps:1}};
  const scores=grade(c,[m],'ok');assert.equal(scores.answer.score,0);assert.equal(scores.task.score,0);assert.equal(scores.parameters.score,null);
  assert.equal(grade(c,[{...m,parts:[{type:'text',text:'hello'}]}],'ok').task.score,1);
  assert.throws(()=>compare({datasetHash:'a',graderVersion:'1'},{datasetHash:'b',graderVersion:'1'}),/Incompatible/);
});
test('tool grading catches parameters, output, and recovery states',()=>{
  const example:Case={...c,expected:{answer:'8',tools:[{name:'add',input:{a:3,b:5},output:{sum:8}}],maxSteps:2}};
  const message={id:'a',role:'assistant' as const,metadata:{outcome:'finished',steps:2},parts:[{type:'tool-add' as const,toolCallId:'x',state:'output-available' as const,input:{a:3,b:5},output:{sum:8}},{type:'text' as const,text:'8'}]};
  assert.equal(grade(example,[message],'ok').task.score,1);
  const wrong=structuredClone(message);if(wrong.parts[0].type==='tool-add')wrong.parts[0].input={a:1,b:2};
  assert.equal(grade(example,[wrong],'ok').parameters.score,0);
  assert.equal(grade({...example,expected:{...example.expected,tools:[{name:'missing',input:{a:3,b:5},output:{sum:8}}]}},[message],'ok').tool_selection.score,0);
  assert.equal(grade({...example,expected:{...example.expected,tools:[{name:'add',input:{a:3,b:5},output:{sum:9}}]}},[message],'ok').outputs.score,0);
  const recovery=grade({...example,expected:{answer:'overflow',tools:[{name:'add',input:{a:3,b:5},error:true}],maxSteps:2}},[{id:'a',role:'assistant',metadata:{outcome:'finished',steps:2},parts:[{type:'tool-add',toolCallId:'x',state:'output-error',input:{a:3,b:5},errorText:'overflow'},{type:'text',text:'overflow'}]}],'ok');
  assert.equal(recovery.task.score,1);assert.equal(recovery.outputs.score,1);
  assert.equal(grade(c,[],'budget-skipped').task.score,null);
});
test('real Rust SSE handles isolated trials, failure, timeout, history and budget',async()=>{
  const mock=await startMock();
  try {
    const [a,b]=await Promise.all([execute(c,1,mock.api,new Budget(5),5000),execute(c,2,mock.api,new Budget(5),5000)]);
    assert.equal(a.scores.task.score,1);assert.equal(b.scores.task.score,1);assert.notEqual(a.runIds[0],b.runIds[0]);assert.notEqual(a.conversationId,b.conversationId);
    const skipped=await execute(c,1,mock.api,new Budget(0),5000);assert.equal(skipped.status,'budget-skipped');
    const error=await execute({...c,messages:[{id:'u',role:'user',parts:[{type:'text',text:'FAIL'}]}]},1,mock.api,new Budget(1),5000);assert.equal(error.status,'error');
    const timeout=await execute({...c,messages:[{id:'u',role:'user',parts:[{type:'text',text:'STALL'}]}]},1,mock.api,new Budget(1),100);assert.equal(timeout.status,'timeout');assert.equal(timeout.cleanup?.confirmed,true);assert(timeout.runIds.length===1);
    const cancelled=await (await fetch(new URL(`/api/runs/${timeout.runIds[0]}`,mock.api))).json();assert.equal(cancelled.status,'cancelled');assert((timeout.attemptIds?.length??0)<=1);
    const summary=summarize([a,b,skipped,error,timeout]);assert.equal(summary.planned,5);assert.equal(summary.scores.task.count,4);assert.equal(summary.scores.task.rate,.5);
    const {cases}=await loadCases(fileURLToPath(new URL('../cases.jsonl',import.meta.url)));
    const history=await execute(cases.find(c=>c.id==='history')!,1,mock.api,new Budget(3),5000);assert.equal(history.scores.task.score,1);assert.equal(history.messages.length,2);
    const tool=history.messages[0].parts.find(p=>p.type==='tool-add') as {toolCallId?:string}|undefined;assert(tool?.toolCallId);
    const originalFetch=globalThis.fetch;
    let dropped=false;
    try {
      globalThis.fetch=async(input,init)=>{
        const response=await originalFetch(input,init);
        if(!dropped && String(input)===mock.api && init?.method==='POST'){
          dropped=true;await response.body?.cancel();throw Error('Accepted response lost');
        }
        return response;
      };
      const lost=await execute({...c,messages:[{id:'u',role:'user',parts:[{type:'text',text:'STALL'}]}]},1,mock.api,new Budget(1),5000);
      assert(dropped);assert.equal(lost.status,'error');assert.equal(lost.cleanup?.confirmed,true);
      assert.equal(lost.runIds.length,1);
      assert.equal((await (await originalFetch(new URL(`/api/runs/${lost.runIds[0]}`,mock.api))).json()).status,'cancelled');
    }finally{globalThis.fetch=originalFetch;}
  } finally {await mock.close();}
});
test('failed Phoenix publication persists all planned trial records',async()=>{
  const root=fileURLToPath(new URL('../../../',import.meta.url));
  const tsx=fileURLToPath(new URL('../node_modules/tsx/dist/cli.mjs',import.meta.url));
  const cli=fileURLToPath(new URL('../src/cli.ts',import.meta.url));
  const child=spawnSync(process.execPath,[tsx,cli,'--mock','--phoenix','--phoenix-url','http://127.0.0.1:1','--trials','1','--output','outputs/evals/publication-failure-test'],{cwd:root,encoding:'utf8',windowsHide:true,timeout:15000});
  assert.equal(child.status,1,child.stderr);
  const report=JSON.parse(await readFile(`${root}/outputs/evals/publication-failure-test/summary.json`,'utf8'));
  assert.equal(report.summary.planned,22);assert.equal(report.summary.errors,22);assert(report.publicationError);
});
