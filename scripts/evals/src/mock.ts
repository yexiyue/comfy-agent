import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { mkdir,copyFile,unlink } from 'node:fs/promises';
const root=fileURLToPath(new URL('../../../',import.meta.url));
export async function startMock(otel=false,maxSteps=6,real=false) {
  const delta=(value:unknown,reason:string|null,usage?:unknown)=>`data: ${JSON.stringify({id:'eval',object:'chat.completion.chunk',created:0,model:'gpt-4.1',choices:[{index:0,delta:value,finish_reason:reason}],usage})}\n\n`;
  const mock=createServer(async(req,res)=>{
    let raw='';for await (const chunk of req) raw+=chunk;
    const request=JSON.parse(raw);
    const texts=request.messages.filter((m:{role:string})=>m.role==='user').map((m:{content:string})=>m.content);
    const prompt=texts.at(-1) as string;
    res.writeHead(200,{'Content-Type':'text/event-stream'});
    if (prompt==='FAIL') {res.end('data: {"bad":\n\n');return;}
    if (prompt==='STALL') {res.write(delta({content:'partial'},null));req.on('close',()=>{});res.on('close',()=>{});return;}
    if (prompt==='FLOOD') {res.end(Array.from({length:5000},()=>delta({content:'x'},null)).join('')+delta({},'stop')+'data: [DONE]\n\n');return;}
    const userIndex=request.messages.map((m:{role:string})=>m.role).lastIndexOf('user');
    const lastTool=request.messages.slice(userIndex+1).findLast((m:{role:string})=>m.role==='tool');
    const match=prompt.match(/ADD (-?\d+) (-?\d+)/);
    let value:unknown;
    let reason='stop';
    if (match && !lastTool) {
      value={tool_calls:[{index:0,id:`call-${userIndex}`,type:'function',function:{name:prompt.includes('UNKNOWN')?'missing':'add',arguments:`{"a":${match[1]},"b":${match[2]}}`}}]}; reason='tool_calls';
    } else if (lastTool) {
      const output=JSON.parse(lastTool.content); value={content:output.error?'overflow':String(output.sum)};
    } else if (prompt==='Previous result?') {
      const previous=request.messages.findLast((m:{role:string})=>m.role==='tool');value={content:String(JSON.parse(previous.content).sum)};
    } else {value={content:prompt.startsWith('Say ')?prompt.slice(4):'ok'};}
    res.end(delta(value,null)+delta({},reason,{prompt_tokens:10,completion_tokens:4,total_tokens:14,prompt_tokens_details:{cached_tokens:2},completion_tokens_details:{reasoning_tokens:1}})+'data: [DONE]\n\n');
  });
  await new Promise<void>(r=>mock.listen(0,'127.0.0.1',r));
  const address=mock.address();if (!address || typeof address==='string') throw Error('No mock port');
  const probe=createServer();await new Promise<void>(r=>probe.listen(0,'127.0.0.1',r));
  const pa=probe.address();if (!pa || typeof pa==='string') throw Error('No server port');
  await new Promise<void>(r=>probe.close(()=>r()));
  await mkdir(`${root}/outputs`,{recursive:true});
  const manifest=`${root}/outputs/mock-config-${pa.port}.json`;
  const executable=process.platform==='win32'?'server.exe':'server';
  const runExecutable=`${root}/outputs/eval-server-${pa.port}${process.platform==='win32'?'.exe':''}`;
  await copyFile(`${root}/target/debug/${executable}`,runExecutable);
  const modelEnv=real?{}:{MODEL:'openai::gpt-4.1',OPENAI_API_KEY:'mock-key',API_BASE_URL:`http://127.0.0.1:${address.port}/v1/`};
  const child=spawn(runExecutable,[],{cwd:root,env:{...process.env,...modelEnv,SERVER_ADDR:`127.0.0.1:${pa.port}`,AGENT_MAX_STEPS:String(maxSteps),OTEL_ENABLED:String(otel),AGENT_CONFIG_MANIFEST:manifest,SERVER_SHUTDOWN_STDIN:'true',RUST_LOG:'info'},stdio:['pipe','pipe','pipe'],windowsHide:true});
  let log='';child.stdout.on('data',c=>{log+=String(c)});child.stderr.on('data',c=>{log+=String(c)});
  const api=`http://127.0.0.1:${pa.port}/api/chat`;
  const deadline=Date.now()+15000;
  while (Date.now()<deadline) {if (child.exitCode!==null) throw Error(`Server failed: ${log}`);try {if ((await fetch(new URL('/health',api))).ok) break;} catch {} await new Promise(r=>setTimeout(r,100));}
  return {api,manifest,log:()=>log,child,close:async()=>{
    if (child.exitCode===null) { child.stdin.write('shutdown\n'); await new Promise<void>((resolve,reject)=>{const timer=setTimeout(()=>{child.kill();reject(Error('Graceful server shutdown timed out'));},7000);child.once('exit',code=>{clearTimeout(timer);code===0?resolve():reject(Error(`Server shutdown failed (${code}): ${log}`));});}); }
    mock.closeAllConnections();await new Promise<void>(r=>mock.close(()=>r()));await unlink(runExecutable);
  }};
}
