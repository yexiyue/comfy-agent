import { createClient } from '@arizeai/phoenix-client';
import { getSpans } from '@arizeai/phoenix-client/spans';
import type { Result } from './core.js';
export async function inspectResults(results:Result[],baseUrl:string,project:string) {
  const client=createClient({options:{baseUrl}});
  for (const result of results) {
    if (!result.parentTraceId) continue;
    let spans:Awaited<ReturnType<typeof getSpans>>['spans']=[];
    // Phoenix ingests batches asynchronously; a multi-turn trace may initially
    // contain the first run only. Wait for every expected root and its task parent.
    const complete = () => spans.some(s=>s.context.span_id===result.parentSpanId)
      && (result.status==='ok'
        ? result.runIds.every(id=>spans.some(s=>s.name==='agent.run' && s.attributes?.['agent.run_id']===id))
        : spans.some(s=>s.name==='agent.run'));
    for (let attempt=0;attempt<20;attempt++) {
      for (const candidate of [project,process.env.PHOENIX_EVAL_PROJECT_NAME??'comfy-agent-evals']) {
        try { spans=(await getSpans({client,project:{projectName:candidate},traceIds:[result.parentTraceId],limit:100})).spans; }
        catch(error) {if ((error as {status?:number}).status===404)continue;throw error;}
        if (complete()) {result.traceProject=candidate;break;}
      }
      if (complete()) break;
      await new Promise(r=>setTimeout(r,200));
    }
    const roots=spans.filter(s=>s.name==='agent.run');
    if (result.status==='ok' && new Set(roots.map(s=>s.attributes?.['agent.run_id'])).size!==result.runIds.length) throw Error(`Missing persisted backend spans for ${result.caseId}`);
    if (result.status==='ok' && result.runIds.some(id=>roots.filter(s=>s.attributes?.['agent.run_id']===id).length<1)) throw Error(`Persisted run ID mismatch for ${result.caseId}`);
    for (const root of roots) if (root.parent_id!==result.parentSpanId) throw Error('Persisted parent span mismatch');
    if (roots.length && !spans.some(s=>s.context.span_id===result.parentSpanId)) throw Error('Persisted experiment task parent is missing');
    const models=spans.filter(s=>s.span_kind==='LLM');
    const tools=spans.filter(s=>s.span_kind==='TOOL');
    result.metrics={modelCalls:models.length,toolCalls:tools.length,toolErrors:tools.filter(s=>s.status_code==='ERROR').length,outcomes:roots.map(s=>s.attributes?.['agent.outcome']),ttftMs:roots.map(s=>s.attributes?.['agent.execution_ttft_ms']).filter(v=>typeof v==='number'),knownTokens:models.map(s=>s.attributes?.['llm.token_count.total']).filter((v):v is number=>typeof v==='number').reduce((a,b)=>a+b,0),usageMissing:models.filter(s=>typeof s.attributes?.['llm.token_count.total']!=='number').length};
  }
}
