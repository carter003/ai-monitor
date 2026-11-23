#!/usr/bin/env node
// Explicit live review regression. All repository writes stay in a private fixture.
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile, appendFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer, request } from 'node:http';
import { StringDecoder } from 'node:string_decoder';
import { createInterface } from 'node:readline';

const state = resolve(import.meta.dirname, '.state');
await mkdir(state, { recursive: true, mode: 0o700 });
const fixture = await mkdtemp(join(tmpdir(), 'codex-local-review-'));
async function git(args) {
  const child = spawn('git', ['-C', fixture, ...args], { stdio: 'ignore', shell: false });
  await new Promise((accept, reject) => {
    child.on('error', reject);
    child.on('exit', code => code === 0 ? accept() : reject(new Error(`Fixture git exited ${code}`)));
  });
}
await writeFile(join(fixture, 'price.mjs'), 'export const total = (price, quantity) => price * quantity;\n');
await writeFile(join(fixture, 'AGENTS.md'),
  'Review price.mjs using read-only tools. Before concluding, inspect the file and diff using a tool. '
  + 'Contract: total(price, quantity) = price * quantity. Return the required review JSON.\n');
await git(['init', '-q']);
await git(['add', 'AGENTS.md', 'price.mjs']);
await git(['-c', 'user.name=Local Regression', '-c', 'user.email=local-regression@example.invalid',
  'commit', '-qm', 'review fixture baseline']);
await writeFile(join(fixture, 'price.mjs'), 'export const total = (price, quantity) => price + quantity;\n');
const output = join(fixture, 'result.txt');
const model = process.env.CODEX_REVIEW_MODEL || 'chatgpt-web/gpt-6-sol-instant';
const reasoning = process.env.CODEX_REVIEW_REASONING || (/pro/i.test(model) ? 'max' : 'low');
const core = process.env.CODEX_CHATGPT_WEB_HOME || join(process.env.HOME, '.codex-chatgpt-web');
const config = JSON.parse(await readFile(join(core, 'config.json'), 'utf8'));
if (!['127.0.0.1', 'localhost', '::1'].includes(config.host)) throw new Error('Expected a loopback bridge');
let bridgeFinalText;
let wireJson;
// Observe the bridge response before Codex intentionally renders review JSON as prose.
// Bytes and authorization are forwarded unchanged; only final assistant text is retained.
const proxy = createServer((incoming, outgoing) => {
  if (!incoming.url.startsWith('/v1/')) { outgoing.writeHead(404).end(); return; }
  const upstream = request({ hostname: config.host, port: config.port, method: incoming.method,
    path: incoming.url, headers: { ...incoming.headers, host: `${config.host}:${config.port}` } }, response => {
    outgoing.writeHead(response.statusCode, response.headers);
    const decoder = new StringDecoder('utf8');
    let buffered = '';
    const capture = incoming.url.startsWith('/v1/responses')
      && response.headers['content-type']?.includes('text/event-stream');
    response.on('data', bytes => {
      if (capture) {
        buffered += decoder.write(bytes);
        let boundary;
        while ((boundary = /\r?\n\r?\n/.exec(buffered))) {
          const frame = buffered.slice(0, boundary.index);
          buffered = buffered.slice(boundary.index + boundary[0].length);
          const data = frame.split(/\r?\n/).filter(line => line.startsWith('data:'))
            .map(line => line.slice(5).trimStart()).join('\n');
          try {
            const event = JSON.parse(data);
            if (event.type !== 'response.completed' || event.response?.status !== 'completed') continue;
            // Native tool turns may include separate commentary before the final message.
            const messages = (event.response.output || []).filter(item => item.type === 'message' && item.role === 'assistant'
              && (item.phase === 'final_answer' || item.phase == null));
            if (messages.length !== 1) {
              bridgeFinalText = undefined;
              wireJson = undefined;
              continue;
            }
            const text = (messages[0].content || []).filter(part => part.type === 'output_text')
              .map(part => part.text).join('');
            bridgeFinalText = text;
            const value = JSON.parse(text);
            if (Array.isArray(value.findings) && typeof value.overall_correctness === 'string') wireJson = text;
          } catch { /* A control frame or a non-JSON intermediate answer is not a review result. */ }
        }
        if (buffered.length > 2_000_000) { upstream.destroy(new Error('Review SSE frame exceeded diagnostic bound')); return; }
      }
      outgoing.write(bytes);
    });
    response.on('end', () => outgoing.end());
    response.on('error', error => outgoing.destroy(error));
  });
  upstream.on('error', () => { if (!outgoing.headersSent) outgoing.writeHead(502); outgoing.end(); });
  incoming.on('aborted', () => upstream.destroy());
  outgoing.on('close', () => { if (!outgoing.writableEnded) upstream.destroy(); });
  incoming.pipe(upstream);
});
proxy.on('upgrade', (_req, socket) => {
  // The real local bridge uses HTTP Responses, with the same WebSocket rejection.
  socket.end('HTTP/1.1 426 Upgrade Required\r\nConnection: close\r\nContent-Length: 0\r\n\r\n');
});
await new Promise(accept => proxy.listen(0, '127.0.0.1', accept));
const proxyBase = `http://127.0.0.1:${proxy.address().port}/v1`;
let completedCommands = 0;
let eventError = false;
let stderr = '';
const events = join(fixture, 'events.jsonl');
await writeFile(events, '', { mode: 0o600 });
let logWrites = Promise.resolve();
const child = spawn(process.env.CODEX_BIN || 'codex', [
  // Inherit the user's configured permissions. The fixture requests read-only
  // commands; this regression does not change global or per-process permissions.
  'exec', '--json', '-C', fixture, '-o', output, '-m', model,
  '-c', `openai_base_url=${JSON.stringify(proxyBase)}`,
  '-c', `review_model="${model}"`, '-c', `model_reasoning_effort="${reasoning}"`, '-c', 'features.hooks=false',
  'review', '--uncommitted',
], { shell: false, stdio: ['ignore', 'pipe', 'pipe'] });
const lines = createInterface({ input: child.stdout });
lines.on('line', line => {
  logWrites = logWrites.then(() => appendFile(events, line + '\n'));
  try {
    const event = JSON.parse(line);
    if (event.type === 'item.completed' && event.item?.type === 'command_execution'
      && event.item.exit_code === 0) completedCommands++;
    if (event.type === 'turn.failed') eventError = true;
  } catch { eventError = true; }
});
child.stderr.on('data', data => { if (stderr.length < 256_000) stderr += data.toString(); });
const timer = setTimeout(() => child.kill('SIGTERM'), 10 * 60_000);
let code;
try {
  code = await new Promise((accept, reject) => {
    child.on('error', reject);
    child.on('close', accept);
  });
} finally {
  clearTimeout(timer);
  await logWrites;
  await new Promise(accept => { proxy.close(accept); proxy.closeAllConnections(); });
  await writeFile(join(fixture, 'stderr.log'), stderr, { mode: 0o600 });
  if (bridgeFinalText !== undefined) await writeFile(join(fixture, 'bridge-final.txt'), bridgeFinalText, { mode: 0o600 });
}
if (code !== 0 || eventError) throw new Error(`Review failed; private diagnostics: ${fixture}`);
const rendered = await readFile(output, 'utf8');
if (wireJson === undefined || bridgeFinalText !== wireJson) {
  throw new Error(`Final bridge response was not review JSON; diagnostics: ${fixture}`);
}
let result;
try { result = JSON.parse(wireJson); }
catch { throw new Error(`Bridge review output was not directly parseable JSON; private diagnostics: ${fixture}`); }
if (!Array.isArray(result.findings) || !result.findings.some(finding =>
  finding.code_location?.absolute_file_path === join(fixture, 'price.mjs'))
  || result.overall_correctness !== 'patch is incorrect' || completedCommands < 1
  || !rendered.includes('price.mjs')) {
  throw new Error(`Review missed the known defect or executed no successful read; diagnostics: ${fixture}`);
}
if (await readFile(join(fixture, 'price.mjs'), 'utf8') !== 'export const total = (price, quantity) => price + quantity;\n') {
  throw new Error(`Review unexpectedly changed the fixture; diagnostics: ${fixture}`);
}
const evidence = { at: new Date().toISOString(), model, reasoning, rawJsonValid: true,
  knownDefectFound: true, completedCommands, findings: result.findings.length,
  rawJsonSource: 'bridge Responses response.completed before Codex review rendering',
  cliReviewRendered: true, fixtureSourceUnchanged: true,
  permissions: 'inherited user configuration; read-only review instructions' };
await writeFile(join(state, 'review-smoke.json'), JSON.stringify(evidence, null, 2) + '\n', { mode: 0o600 });
console.log(JSON.stringify(evidence));
