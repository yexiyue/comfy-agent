import { createHash, randomUUID } from 'node:crypto';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { isDeepStrictEqual } from 'node:util';
import { DefaultChatTransport, readUIMessageStream, type UIMessage } from 'ai';
import { context, trace } from '@opentelemetry/api';

export type ToolExpectation = { name: string; input: Record<string, unknown>; output?: unknown; error?: boolean };
export type Case = { id: string; messages: UIMessage[]; followups?: string[]; expected: { answer: string; tools: ToolExpectation[]; maxSteps: number }; mock?: { a: string; b: string; overflow?: boolean } };
export type Score = { score: number | null; label: string; explanation: string };
export type Result = { caseId: string; trial: number; status: 'ok'|'error'|'timeout'|'budget-skipped'; durationMs: number; messages: UIMessage[]; scores: Record<string,Score>; traceIds: string[]; runIds: string[]; reason?: string; conversationId?:string;attemptIds?:string[];cleanup?:{confirmed:boolean;reason?:string};parentTraceId?: string; parentSpanId?: string; traceProject?:string; metrics?: {modelCalls:number;toolCalls:number;toolErrors:number;outcomes:unknown[];ttftMs:unknown[];knownTokens:number;usageMissing:number} };
export const graderVersion = '1';
export const hash = (value: string) => createHash('sha256').update(value).digest('hex');

export function validateCases(value: unknown): Case[] {
  if (!Array.isArray(value) || !value.length) throw Error('Dataset must contain cases');
  const ids = new Set<string>();
  for (const c of value) {
    if (!c || typeof c.id !== 'string' || !c.id || ids.has(c.id)) throw Error('Missing or duplicate case ID');
    ids.add(c.id);
    if (!Array.isArray(c.messages) || !c.messages.length || !c.messages.every((m: UIMessage) => m.role === 'user' || m.role === 'system') || !c.messages.every((m: UIMessage) => Array.isArray(m.parts) && m.parts.every(p => p.type === 'text')) || !c.expected || typeof c.expected.answer !== 'string' || !Array.isArray(c.expected.tools) || !Number.isInteger(c.expected.maxSteps) || c.expected.maxSteps <= 0) throw Error(`Invalid case ${c.id}`);
    if (c.followups && (!Array.isArray(c.followups) || !c.followups.every((v:unknown)=>typeof v==='string'))) throw Error('Invalid followups');
    for (const t of c.expected.tools) if (typeof t.name !== 'string' || typeof t.input !== 'object' || t.input === null) throw Error('Invalid tool expectation');
  }
  return value as Case[];
}
export async function loadCases(path: string) { const text = await readFile(path,'utf8'); return { cases: validateCases(text.trim().split(/\r?\n/).map(line=>JSON.parse(line))), datasetHash: hash(text.replaceAll('\r\n','\n')) }; }

export function grade(c: Case, messages: UIMessage[], status: Result['status']): Record<string,Score> {
  const score = (pass: boolean, reason: string): Score => ({ score: pass ? 1:0, label: pass?'pass':'fail', explanation:reason });
  if (status === 'budget-skipped') return {task:{score:null,label:'budget-skipped',explanation:'Not executed: request budget exhausted'}};
  if (status !== 'ok') return { task:score(false,`Execution ${status}`) };
  const last = messages.at(-1);
  const text = last?.parts.filter(p=>p.type==='text').map(p=>p.text).join('').trim() ?? '';
  const tools = messages.flatMap(m=>m.parts).filter(p=>p.type.startsWith('tool-') || p.type==='dynamic-tool') as unknown as { type:string;toolName?:string;input:unknown;output?:unknown;state:string }[];
  const selected = tools.length === c.expected.tools.length && tools.every((p,i)=>(p.toolName ?? p.type.slice(5))===c.expected.tools[i].name);
  const parameters = selected && tools.every((p,i)=>isDeepStrictEqual(p.input,c.expected.tools[i].input));
  const outputs = selected && tools.every((p,i)=>c.expected.tools[i].error ? p.state==='output-error' : p.state==='output-available' && isDeepStrictEqual(p.output,c.expected.tools[i].output));
  const metadata = messages.map(m=>m.metadata as {outcome?:string;steps?:number}|undefined);
  const steps = metadata.every(m=>m?.outcome==='finished' && typeof m.steps==='number' && m.steps<=c.expected.maxSteps);
  const scores: Record<string,Score> = {
    answer:score(text===c.expected.answer,`Expected exact final text ${c.expected.answer}`),
    tool_selection:score(selected,'Tool sequence and names must match'),
    parameters:tools.length || c.expected.tools.length ? score(parameters,'Structured tool parameters must match') : {score:null,label:'not-applicable',explanation:'No tool parameters'},
    outputs:tools.length || c.expected.tools.length ? score(outputs,'Tool results/error states must match') : {score:null,label:'not-applicable',explanation:'No tool outputs'},
    steps:score(steps,'All turns finish within step limit'),
  };
  scores.task=score(Object.values(scores).every(s=>s.score!==0),'All applicable quality checks');
  return scores;
}

export class Budget { used=0; constructor(readonly max:number) {} take() { if (this.used>=this.max) return false; this.used++; return true; } }
export async function execute(c:Case, trial:number, api:string, budget:Budget, timeoutMs:number):Promise<Result> {
  const started=Date.now();
  const controller=new AbortController();
  const timer=setTimeout(()=>controller.abort(),timeoutMs);
  const messages:UIMessage[]=[];
  const result:Result={caseId:c.id,trial,status:'ok',durationMs:0,messages,scores:{},traceIds:[],runIds:[]};
  const parent=trace.getSpan(context.active())?.spanContext();
  const headers:Record<string,string>={'X-Agent-Run-Source':'eval'};
  if (parent) { headers.traceparent=`00-${parent.traceId}-${parent.spanId}-01`; result.parentTraceId=parent.traceId; result.parentSpanId=parent.spanId; }
  let conversation:{id:string;revision:number}|undefined;
  let pendingRequest:string|undefined;
  let activeRun:string|undefined;
  const endpoint=(path:string)=>new URL(path,api).toString();
  const json=async(path:string,body?:unknown,signal:AbortSignal=controller.signal)=>{
    const response=await fetch(endpoint(path),{signal,headers:{...headers,'Content-Type':'application/json'},...(body===undefined?{}:{method:'POST',body:JSON.stringify(body)})});
    const value=await response.json();if(!response.ok)throw Error(`HTTP ${response.status}`);return value;
  };
  const remember=async(runId:string,signal:AbortSignal)=>{
    const run=await json(`/api/runs/${runId}`,undefined,signal);
    if(!result.runIds.includes(runId))result.runIds.push(runId);
    result.attemptIds??=[];
    for(const attempt of run.attempts??[]){if(!result.attemptIds.includes(attempt.id))result.attemptIds.push(attempt.id);if(attempt.traceId&&!result.traceIds.includes(attempt.traceId))result.traceIds.push(attempt.traceId);}
    return run;
  };
  try {
    for (const followup of [undefined,...(c.followups ?? [])]) {
      if (!budget.take()) { result.status='budget-skipped'; result.reason='Request budget exhausted'; break; }
      if(!conversation){conversation=await json('/api/conversations',{messages:c.messages.slice(0,-1)});result.conversationId=conversation!.id;}
      const snapshot=await json(`/api/conversations/${conversation!.id}`);
      const message=followup===undefined?c.messages.at(-1)!:{id:randomUUID(),role:'user' as const,parts:[{type:'text' as const,text:followup}]};
      pendingRequest=randomUUID();
      const transport=new DefaultChatTransport({api,headers,prepareSendMessagesRequest:()=>({body:{id:conversation!.id,expectedRevision:snapshot.revision,requestId:pendingRequest,message}})});
      const stream=await transport.sendMessages({trigger:'submit-message',chatId:conversation!.id,messageId:undefined,messages:[message],abortSignal:controller.signal});
      const receipt=await json(`/api/conversations/${conversation!.id}/commands/${pendingRequest}`);
      activeRun=receipt.runId;
      let last:UIMessage|undefined;
      for await (const message of readUIMessageStream({stream,terminateOnError:true})) last=message;
      if (!last) throw Error('Missing final UIMessage');
      messages.push(last);
      const run=await remember(activeRun!,controller.signal);
      if(!['finished','step-limit'].includes(run.status))throw Error('Run did not finish');
      activeRun=undefined;pendingRequest=undefined;
    }
  } catch {
    result.status=controller.signal.aborted?'timeout':'error'; result.reason=result.status==='timeout'?'Trial timed out':'Chat transport/model/protocol failed';
  } finally {
    clearTimeout(timer);
    if(conversation && pendingRequest){
      try {
        const cleanupSignal=AbortSignal.timeout(5000);
        if(!activeRun){const receipt=await json(`/api/conversations/${conversation.id}/commands/${pendingRequest}`,undefined,cleanupSignal);activeRun=receipt.runId;}
        let run=await remember(activeRun!,cleanupSignal);
        for(let retry=0;retry<5 && !['finished','step-limit','failed','cancelled','superseded'].includes(run.status);retry++){
          const response=await fetch(endpoint(`/api/runs/${run.id}/cancel`),{method:'POST',signal:cleanupSignal,headers:{'Content-Type':'application/json'},body:JSON.stringify({conversationId:conversation.id,expectedVersion:run.version,requestId:randomUUID()})});
          if(!response.ok && response.status!==409)throw Error(`Cancel HTTP ${response.status}`);
          run=await remember(run.id,cleanupSignal);
        }
        result.cleanup={confirmed:['finished','step-limit','failed','cancelled','superseded'].includes(run.status)};
        if(!result.cleanup.confirmed)result.cleanup.reason='Cancellation not confirmed';
      }catch {result.cleanup={confirmed:false,reason:'Cleanup failed: task state could not be confirmed'};}
    }
  }
  result.durationMs=Date.now()-started; result.scores=grade(c,messages,result.status); return result;
}

export function summarize(results:Result[]) {
  const eligible=results.filter(r=>r.status!=='budget-skipped');
  const durations=eligible.map(r=>r.durationMs).sort((a,b)=>a-b);
  const percentile=(p:number)=>durations.length ? durations[Math.ceil(p*durations.length)-1]:null;
  const scoreNames=[...new Set(results.flatMap(r=>Object.keys(r.scores)))];
  const scores=Object.fromEntries(scoreNames.map(name=> { const values=eligible.map(r=>r.scores[name]?.score).filter((v):v is number=>typeof v==='number'); return [name,{pass:values.filter(v=>v===1).length,count:values.length,rate:values.length?values.filter(v=>v===1).length/values.length:null}]; }));
  const observed=results.filter(r=>r.metrics);
  const tools=observed.reduce((n,r)=>n+r.metrics!.toolCalls,0),toolErrors=observed.reduce((n,r)=>n+r.metrics!.toolErrors,0);
  return { planned:results.length, completed:results.filter(r=>r.status==='ok').length, errors:results.filter(r=>r.status==='error').length,timeouts:results.filter(r=>r.status==='timeout').length,skipped:results.length-eligible.length,p50Ms:percentile(.5),p95Ms:percentile(.95),scores,steps:results.flatMap(r=>r.messages.map(m=>(m.metadata as {steps?:number})?.steps)).filter(v=>typeof v==='number'),observedTrials:observed.length,toolErrorRate:tools?toolErrors/tools:null,knownTokens:observed.length?observed.reduce((n,r)=>n+r.metrics!.knownTokens,0):null,usageMissing:observed.length?observed.reduce((n,r)=>n+r.metrics!.usageMissing,0):null,outcomes:observed.flatMap(r=>r.metrics!.outcomes),ttftMs:observed.flatMap(r=>r.metrics!.ttftMs) };
}
export async function report(directory:string,results:Result[],metadata:Record<string,unknown>,publicationError?:string) {
  await mkdir(directory,{recursive:true});
  const summary={metadata,publicationError,summary:summarize(results)};
  await writeFile(`${directory}/results.jsonl`,results.map(r=>JSON.stringify(r)).join('\n')+'\n');
  await writeFile(`${directory}/summary.json`,JSON.stringify(summary,null,2));
  await writeFile(`${directory}/summary.md`,`# Agent evaluation\n\n${JSON.stringify(summary.summary,null,2)}\n\nPublication: ${publicationError ?? 'ok / local-only'}\n`);
  return summary;
}
export function compare(a:Record<string,unknown>, b:Record<string,unknown>) {
  for (const key of ['datasetHash','graderVersion']) if (a[key]!==b[key]) throw Error(`Incompatible baseline ${key}`);
}
