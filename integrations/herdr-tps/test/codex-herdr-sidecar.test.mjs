import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { interactiveCodex, paneStateFile, processIdentity, findCodexHost } from '../lib/codex-host.mjs';
const hook = fileURLToPath(new URL('../codex-herdr-hook.js', import.meta.url));
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(fn) {
  for (let i = 0; i < 100; i++) { if (fn()) return; await pause(40); }
  assert.fail('Timed out waiting for observer state');
}
test('only interactive commands qualify, including options with values', () => {
  for (const args of [['exec'], ['review'], ['-m','review','exec'], ['-c','x=1','mcp'], ['app-server']]) {
    assert.equal(interactiveCodex(['codex', ...args]), false);
  }
  for (const args of [[], ['-m','review'], ['resume'], ['-c','x=1','hello']]) assert.equal(interactiveCodex(['codex', ...args]), true);
});
test('proc parsing tolerates parentheses and nearest batch Codex blocks outer TUI', t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'herdr-proc-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [pid, parent, args] of [[20,10,['codex','exec']], [10,1,['codex']]]) {
    fs.mkdirSync(`${root}/${pid}`);
    fs.writeFileSync(`${root}/${pid}/stat`, `${pid} (codex (test)) S ${parent} ${Array(17).fill('0').join(' ')} 1234 0\n`);
    fs.writeFileSync(`${root}/${pid}/cmdline`, args.join('\0'));
    fs.writeFileSync(`${root}/${pid}/environ`, 'HERDR_PANE_ID=pane\0');
  }
  assert.equal(processIdentity(20, root).startTime, '1234');
  assert.equal(findCodexHost('pane',20,root), undefined);
  assert.equal(findCodexHost('pane',10,root).pid, 10);
});
test('detached sidecar switches sessions, ignores stale end and exits with host', { timeout: 15000 }, async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'herdr-sidecar-'));
  const socket = path.join(root, 'daemon.sock'), messages = [];
  let holdClear = false, releaseClear;
  const server = net.createServer(client => {
    let input = '';
    client.on('data', chunk => {
      input += chunk;
      if (!input.includes('\n')) return;
      const request = JSON.parse(input.slice(0, input.indexOf('\n')));
      messages.push(request.params);
      const reply = () => client.end(JSON.stringify({ id: request.id, result: {} }) + '\n');
      if (holdClear && request.params.source === 'herdr:tps' && request.params.tokens?.tps === null) {
        holdClear = false;
        releaseClear = reply;
      } else reply();
    });
  });
  await new Promise(resolve => server.listen(socket, resolve));
  const fixture = path.join(root, 'host.mjs');
  fs.writeFileSync(fixture, `import {spawnSync} from 'node:child_process';
import readline from 'node:readline';
process.stdout.write('ready\\n');
readline.createInterface({input:process.stdin}).on('line', line => {
 const {event, input} = JSON.parse(line);
 const result = spawnSync(process.execPath, [${JSON.stringify(hook)},event], {input:JSON.stringify(input)});
 process.stdout.write('done:'+result.status+'\\n');
});`);
  const env = { ...process.env, HERDR_ENV:'1', HERDR_PANE_ID:'test-pane', HERDR_SOCKET_PATH:socket, XDG_RUNTIME_DIR:root };
  delete env.CODEX_THREAD_ID; delete env.OMPCODE; delete env.HERDR_TPS_OMP_TOPLEVEL;
  const host = spawn('bash',['-c','exec -a codex "$NODE_TEST_BIN" "$HOST_TEST_FILE"'],
    { env: {...env, NODE_TEST_BIN:process.execPath, HOST_TEST_FILE:fixture}, stdio:['pipe','pipe','pipe'] });
  let output = ''; host.stdout.on('data', b => { output += b; });
  let stateFile;
  const hash = (await import('node:crypto')).createHash('sha256').update('test-pane').digest('hex').slice(0,16);
  stateFile = path.join(root,'herdr-tps',`codex-${hash}.json`);
  t.after(async () => {
    host.kill('SIGKILL');
    if (fs.existsSync(stateFile)) { const s = JSON.parse(fs.readFileSync(stateFile)); try { process.kill(s.sidecarPid, 'SIGTERM'); } catch {} }
    await new Promise(resolve => server.close(resolve)); fs.rmSync(root,{recursive:true,force:true});
  });
  await until(() => output.includes('ready'));
  const transcript = path.join(root,'rollout.jsonl'); fs.writeFileSync(transcript,'');
  const input = { session_id:'one',transcript_path:transcript,model:'initial' };
  const send = (event, value) => host.stdin.write(JSON.stringify({event,input:value})+'\n');
  send('start',input);
  await until(() => fs.existsSync(stateFile));
  await until(() => messages.some(m => m.tokens?.model === 'initial'));
  const first = JSON.parse(fs.readFileSync(stateFile));
  fs.appendFileSync(transcript, JSON.stringify({type:'event_msg',payload:{type:'item_completed',started_at_ms:1000,completed_at_ms:3000,item:{type:'Reasoning'}}})+'\n');
  const usage = JSON.stringify({type:'token_usage_record',payload:{usage:{output_tokens:100},response_id:'response'}});
  fs.appendFileSync(transcript,usage.slice(0,-1));
  await pause(1100); assert.ok(!messages.some(m => m.tokens?.tps === '50.0'));
  fs.appendFileSync(transcript,usage.slice(-1)+'\n');
  await until(() => messages.some(m => m.tokens?.tps === '50.0'));
  send('start',{...input,session_id:'two',model:'switched'});
  await until(() => messages.some(m => m.tokens?.model === 'switched'));
  assert.equal(JSON.parse(fs.readFileSync(stateFile)).sidecarPid, first.sidecarPid);
  send('end',input); await pause(100); assert.ok(fs.existsSync(stateFile));
  holdClear = true;
  send('end',{...input,session_id:'two'});
  await until(() => releaseClear);
  send('start',{...input,session_id:'replacement',model:'after-clear'});
  await until(() => messages.some(m => m.tokens?.model === 'after-clear'));
  assert.notEqual(JSON.parse(fs.readFileSync(stateFile)).sidecarPid, first.sidecarPid);
  releaseClear();
  await until(() => !processIdentity(first.sidecarPid));
  assert.equal(JSON.parse(fs.readFileSync(stateFile)).sessionId, 'replacement');
  send('end',{...input,session_id:'replacement'});
  await until(() => !fs.existsSync(stateFile));
  assert.ok(processIdentity(host.pid), 'normal SessionEnd leaves the native host running');
  send('start',{...input,session_id:'three',model:'last'});
  await until(() => messages.some(m => m.tokens?.model === 'last'));
  host.kill('SIGKILL');
  await until(() => !fs.existsSync(stateFile));
  assert.ok(messages.some(m => m.tokens?.model === null && m.tokens?.tps === null));
});
test('nested hook exits without creating state or sending metadata', () => {
  const result = spawnSync(process.execPath,[hook,'start'],{input:'{}',env:{...process.env,HERDR_ENV:'1',OMPCODE:'1'}});
  assert.equal(result.status,0);
});

test('long resume clears prior metadata and batches all historical updates into one snapshot', async t => {
  const { startSidecar } = await import('../codex-herdr-sidecar.js');
  const root = fs.mkdtempSync(path.join(os.tmpdir(),'herdr-resume-batch-'));
  const previousHome = process.env.CODEX_HOME;
  process.env.CODEX_HOME = root;
  t.after(() => {
    if (previousHome === undefined) delete process.env.CODEX_HOME; else process.env.CODEX_HOME = previousHome;
    fs.rmSync(root,{recursive:true,force:true});
  });
  const transcript = path.join(root,'rollout.jsonl'), file = path.join(root,'state.json');
  const lines = [];
  for (let i=0;i<1000;i++) lines.push(
    {type:'turn_context',payload:{model:`model-${i}`}},
    {type:'event_msg',payload:{type:'item_completed',started_at_ms:i*2000,completed_at_ms:(i+1)*2000,item:{type:'Reasoning'}}},
    {type:'token_usage_record',payload:{response_id:`r-${i}`,usage:{output_tokens:i%2 ? 200 : 100}}},
  );
  fs.writeFileSync(transcript,'');
  fs.writeFileSync(file,JSON.stringify({sidecarPid:process.pid,codexPid:process.pid,
    codexStartTime:processIdentity(process.pid).startTime,sessionId:'test',transcriptPath:transcript}));
  const calls=[];
  const observer=startSidecar(file,{autoStart:false,publisher:{
    resetForAgent:async ttl => calls.push(['reset',ttl]),
    publishSnapshot:async value => calls.push(['snapshot',value]),
    clearAll:async () => calls.push(['clear']),
  }});
  t.after(()=>observer.shutdown());
  assert.deepEqual(calls[0],['reset',5000]);
  assert.deepEqual(calls[1],['snapshot',{model:undefined,rate:0}]);
  calls.length = 0;
  fs.appendFileSync(transcript,lines.map(l=>JSON.stringify(l)+'\n').join(''));
  observer.tick();
  assert.equal(calls.filter(c=>c[0]==='snapshot').length,1);
  const snapshot=calls.at(-1)[1];
  assert.equal(snapshot.model,'model-999');
  assert.ok(snapshot.rate>50 && snapshot.rate<100);
  observer.tick(); assert.equal(calls.length,1,'unchanged file produces no extra publication');
  // Truncation creates a fresh follower, including dedup state and UTF8 decoder.
  fs.writeFileSync(transcript, JSON.stringify({type:'event_msg',payload:{type:'item_completed',started_at_ms:0,completed_at_ms:2000,item:{type:'Reasoning'}}})+'\n'+JSON.stringify({type:'token_usage_record',payload:{response_id:'r-999',usage:{output_tokens:100}}})+'\n');
  observer.tick(); assert.equal(calls.at(-1)[1].rate,50);
});

test('PID-valued OMP marker blocks Hook before inspecting any transcript', async () => {
  const {runHook}=await import('../codex-herdr-hook.js');
  const names=['HERDR_ENV','HERDR_PANE_ID','HERDR_SOCKET_PATH','HERDR_TPS_OMP_TOPLEVEL','CODEX_THREAD_ID','OMPCODE'];
  const saved=Object.fromEntries(names.map(name=>[name,process.env[name]]));
  try {
    Object.assign(process.env,{HERDR_ENV:'1',HERDR_PANE_ID:'pane',HERDR_SOCKET_PATH:'/unused',HERDR_TPS_OMP_TOPLEVEL:String(process.pid)});
    delete process.env.CODEX_THREAD_ID;delete process.env.OMPCODE;
    runHook({session_id:'session',get transcript_path(){assert.fail('nested Hook must exit before transcript access');}},'start');
  } finally { for(const name of names){if(saved[name]===undefined)delete process.env[name];else process.env[name]=saved[name];} }
});

test('first observer waits for the spawning hook to commit ownership', async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'herdr-start-barrier-'));
  const file = path.join(root, 'state.json'), transcript = path.join(root, 'transcript.jsonl');
  const state = {codexPid:process.pid, codexStartTime:processIdentity(process.pid).startTime,
    model:'committed', sessionId:'first', transcriptPath:transcript};
  fs.writeFileSync(transcript, '');
  fs.writeFileSync(file, JSON.stringify(state));
  const fd = fs.openSync(file + '.flock', 'a', 0o600);
  const lock = spawnSync('flock', ['-x', '3'], {stdio:['ignore', 'ignore', 'ignore', fd]});
  assert.equal(lock.status, 0);
  const module = new URL('../codex-herdr-sidecar.js', import.meta.url).href;
  const child = spawn(process.execPath, ['--input-type=module', '-e', `
    import {startSidecar} from ${JSON.stringify(module)};
    startSidecar(process.argv[1], {publisher:{
      resetForAgent() {}, clearAll() {}, publishSnapshot({model}) { console.log(model); }
    }});
  `, file]);
  let output = '', released = false;
  child.stdout.on('data', chunk => { output += chunk; });
  t.after(() => {
    if (!released) fs.closeSync(fd);
    child.kill('SIGKILL');
    fs.rmSync(root, {recursive:true, force:true});
  });
  // The child has reached its first read while its PID is still absent.
  await until(() => fs.readFileSync(`/proc/${child.pid}/task/${child.pid}/children`, 'utf8').trim());
  assert.equal(output, '');
  fs.writeFileSync(file, JSON.stringify({...state, sidecarPid:child.pid}));
  fs.closeSync(fd);
  released = true;
  await until(() => output.includes('committed'));
});
