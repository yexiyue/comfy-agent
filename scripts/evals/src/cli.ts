import { readFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { createClient } from '@arizeai/phoenix-client';
import { createDataset } from '@arizeai/phoenix-client/datasets';
import { runExperiment } from '@arizeai/phoenix-client/experiments';
import { Budget,execute,loadCases,report,graderVersion,hash,compare,type Case,type Result } from './core.js';
import { startMock } from './mock.js';
import { inspectResults } from './phoenix.js';

const args=process.argv.slice(2);
const root=fileURLToPath(new URL('../../../',import.meta.url));
const get=(name:string,fallback:string)=>{const i=args.indexOf(name);return i>=0?args[i+1]:fallback;};
const positive=(name:string,fallback:number)=>{const value=Number(get(name,String(fallback)));if (!Number.isSafeInteger(value)||value<=0) throw Error(`Invalid ${name}`);return value;};
const mockMode=args.includes('--mock'),phoenix=args.includes('--phoenix');
if (!mockMode && !args.includes('--real')) throw Error('Use --mock or explicitly --real');
const trials=positive('--trials',3),concurrency=positive('--concurrency',2),timeoutMs=positive('--timeout-ms',60000);
const budget=new Budget(positive('--max-requests',100));
const {cases,datasetHash}=await loadCases(resolve(root,get('--cases',fileURLToPath(new URL('../cases.jsonl',import.meta.url)))));
const fixture=(mockMode || args.includes('--start-server'))?await startMock(phoenix,6,!mockMode):undefined;
const results:Result[]=[];
const api=fixture?.api ?? get('--api','http://127.0.0.1:3001/api/chat');
const out=resolve(root,get('--output',fileURLToPath(new URL(`../../../outputs/evals/${Date.now()}/`,import.meta.url))));
let publicationError:string|undefined;
let experimentProject:string|undefined;
const counts=new Map<string,number>();
const git=(...params:string[])=>execFileSync('git',params,{cwd:fileURLToPath(new URL('../../../',import.meta.url)),encoding:'utf8'}).trim();
let manifest:Record<string,unknown>={};
const manifestPath=fixture?.manifest ?? get('--manifest','');
if (!manifestPath) throw Error('Real eval requires --manifest from AGENT_CONFIG_MANIFEST');
manifest=JSON.parse(await readFile(resolve(root,manifestPath),'utf8'));
const metadata={datasetHash,graderVersion,gitSha:git('rev-parse','HEAD'),dirty:Boolean(git('status','--porcelain')),trials,concurrency,timeoutMs,maxRequests:budget.max,...manifest,systemPromptHash:hash(JSON.stringify(cases.map(c=>c.messages.filter(m=>m.role==='system')))),inputPromptHash:hash(JSON.stringify(cases.map(c=>c.messages))),mode:mockMode?'mock':'real'};
const task=async(c:Case)=>{const trial=(counts.get(c.id)??0)+1;counts.set(c.id,trial);const result=await execute(c,trial,api,budget,timeoutMs);results.push(result);return result;};
try {
  if (phoenix) {
    const baseUrl=get('--phoenix-url','http://localhost:6006');
    const client=createClient({options:{baseUrl}});
    const dataset=await createDataset({client,name:`comfy-agent-${datasetHash.slice(0,12)}`,description:'Versioned synthetic Rust agent regression cases',examples:cases.map(c=>({input:c,output:c.expected,metadata:{caseId:c.id,datasetHash}}))});
    const experiment=await runExperiment({client,dataset:{datasetId:dataset.datasetId},experimentName:`${mockMode?'mock':'real'}-${Date.now()}`,experimentMetadata:metadata,repetitions:trials,concurrency,task:async example=>task(example.input as unknown as Case),evaluators:['task','answer','tool_selection','parameters','outputs','steps'].map(name=>({name,kind:'CODE' as const,evaluate:({output}:{output:unknown})=>{const result=output as Result;return result.scores[name]??{score:null,label:'not-applicable',explanation:'Unavailable after execution failure'};}}))});
    Object.assign(metadata,{experimentId:experiment.id,datasetId:dataset.datasetId,datasetVersionId:experiment.datasetVersionId});
    if (experiment.missingRunCount>0 || Object.keys(experiment.runs).length !== cases.length*trials) { publicationError='Phoenix experiment has missing published runs'; process.exitCode=1; }
    experimentProject=experiment.projectName??undefined;
    Object.assign(metadata,{experimentProject});
    if (results.some(r=>r.status==='ok' && (!r.parentTraceId || r.traceIds.some(id=>id!==r.parentTraceId)))) throw Error('Backend experiment trace context association failed');
  } else {
    const queue=cases.flatMap(c=>Array.from({length:trials},()=>c));
    await Promise.all(Array.from({length:concurrency},async()=>{while(queue.length) await task(queue.shift()!);}));
  }
} catch (error) {publicationError=error instanceof Error?error.message:'Experiment failed';process.exitCode=1;}
finally {
  try { await fixture?.close(); } catch(error) { publicationError=error instanceof Error?error.message:'Backend shutdown failed'; process.exitCode=1; }
  if (experimentProject) {try {await inspectResults(results,get('--phoenix-url','http://localhost:6006'),experimentProject);} catch(error) {publicationError=error instanceof Error?error.message:'Trace verification failed';process.exitCode=1;}}
  // Every planned trial remains visible, including publication/startup failures.
  for (const c of cases) for (let trial=1;trial<=trials;trial++) if (!results.some(r=>r.caseId===c.id&&r.trial===trial)) results.push({caseId:c.id,trial,status:'error',durationMs:0,messages:[],scores:{task:{score:0,label:'fail',explanation:'Experiment did not execute trial'}},traceIds:[],runIds:[],reason:'Experiment interrupted'});
  const summary=await report(out,results,metadata,publicationError);
  if (args.includes('--baseline')) { const baseline=JSON.parse(await readFile(resolve(root,get('--baseline','')),'utf8'));compare(metadata,baseline.metadata);console.log(JSON.stringify({baseline:baseline.summary,current:summary.summary})); }
  console.log(JSON.stringify({output:out,...summary},null,2));
}
if (mockMode && results.some(r=>r.status!=='budget-skipped'&&r.scores.task?.score!==1)) process.exitCode=1;
