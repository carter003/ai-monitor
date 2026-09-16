import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { access, chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

async function waitForPath(target, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await access(target);
      return;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
  }
  throw new Error(`timed out waiting for ${target}`);
}

test('preserves early settings, clears a closed root, and drains metadata on SIGTERM', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-codex-e2e-'));
  const socketPath = path.join(directory, 'herdr.sock');
  const fakeCodex = path.join(directory, 'codex-fixture.mjs');
  const wrapper = fileURLToPath(new URL('../codex-with-tps.mjs', import.meta.url));
  const wsModule = import.meta.resolve('ws');
  const requests = [];
  let wrapperChild;
  let signalSent = false;
  let observedOutput = false;
  let watchdog;
  const metadataServer = net.createServer((socket) => {
    let input = '';
    socket.on('data', (chunk) => {
      input += chunk.toString('utf8');
      const lineEnd = input.indexOf('\n');
      if (lineEnd < 0) {
        return;
      }
      const request = JSON.parse(input.slice(0, lineEnd));
      requests.push(request);
      socket.end(`${JSON.stringify({ id: request.id, result: { type: 'ok' } })}\n`);
      if (Number(request.params.tokens?.tps) > 0) observedOutput = true;
      if (
        !signalSent &&
        observedOutput &&
        request.params.tokens?.model === null &&
        request.params.tokens?.tps === undefined
      ) {
        signalSent = true;
        setTimeout(() => wrapperChild?.kill('SIGTERM'), 10);
      }
    });
  });
  await new Promise((resolve, reject) => {
    metadataServer.once('error', reject);
    metadataServer.listen(socketPath, resolve);
  });

  await writeFile(
    fakeCodex,
    `#!/usr/bin/env node
import http from 'node:http';
import WebSocket, { WebSocketServer } from ${JSON.stringify(wsModule)};

const args = process.argv.slice(2);
if (args[0] === 'app-server') {
  const address = new URL(args[2]);
  const server = http.createServer((request, response) => {
    response.writeHead(request.url === '/readyz' ? 200 : 404);
    response.end();
  });
  const websocketServer = new WebSocketServer({ server });
  websocketServer.on('connection', (socket) => {
    socket.on('message', (data) => {
      const request = JSON.parse(data.toString('utf8'));
      if (request.method === 'thread/read') {
        socket.send(JSON.stringify({ id: request.id, result: { thread: { id: 'active', parentThreadId: null, model: 'gpt-5.6-luna' } } }));
        // A later internal thread is parentless, but must never replace the user model.
        socket.send(JSON.stringify({ method: 'thread/started', params: { thread: { id: 'system', parentThreadId: null, source: 'vscode', threadSource: 'system', ephemeral: true, model: 'gpt-5.6-luna' } } }));
        socket.send(JSON.stringify({ method: 'turn/started', params: { threadId: 'system', turn: { id: 'system-turn' } } }));
        socket.send(JSON.stringify({ method: 'item/agentMessage/delta', params: { threadId: 'system', turnId: 'system-turn', itemId: 'internal', delta: 'internal output must be ignored' } }));
        socket.send(JSON.stringify({ method: 'turn/completed', params: { threadId: 'system', turn: { id: 'system-turn' } } }));
        setTimeout(() => socket.send(JSON.stringify({ method: 'thread/closed', params: { threadId: 'active' } })), 100);
        return;
      }
      if (request.method !== 'thread/start') return;
      socket.send(JSON.stringify({ id: request.id, result: { thread: { id: 'placeholder', parentThreadId: null }, model: 'gpt-5.6-luna' } }));
      socket.send(JSON.stringify({ method: 'thread/settings/updated', params: { threadId: 'active', threadSettings: { model: 'gpt-5.6-luna', collaborationMode: { settings: { model: 'gpt-6-astra' } } } } }));
      socket.send(JSON.stringify({ method: 'turn/started', emittedAtMs: 100, params: { threadId: 'active', turn: { id: 'turn-e2e' } } }));
      socket.send(JSON.stringify({ method: 'item/agentMessage/delta', emittedAtMs: 200, params: { threadId: 'active', itemId: 'message-e2e', delta: 'deterministic wrapper output for rate sampling' } }));
      socket.send(JSON.stringify({ method: 'turn/completed', emittedAtMs: 600, params: { threadId: 'active', turn: { id: 'turn-e2e', status: 'completed' } } }));
    });
  });
  server.listen(Number(address.port), address.hostname);
} else if (args[0] === '--remote') {
  const socket = new WebSocket(args[1]);
  socket.on('open', () => socket.send(JSON.stringify({ id: 1, method: 'thread/start', params: {} })));
  socket.on('message', (data) => {
    if (JSON.parse(data).id?.toString().startsWith('herdr-tps:')) process.exit(3);
  });
  socket.on('close', () => process.exit(0));
} else {
  process.exit(2);
}
`,
    'utf8',
  );
  await chmod(fakeCodex, 0o755);

  try {
    const result = await new Promise((resolve, reject) => {
      wrapperChild = spawn(process.execPath, [wrapper], {
        env: {
          ...process.env,
          HERDR_ENV: '1',
          HERDR_PANE_ID: 'pane-e2e',
          HERDR_SOCKET_PATH: socketPath,
          HERDR_TPS_CODEX_BIN: fakeCodex,
        },
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      watchdog = setTimeout(() => wrapperChild.kill('SIGTERM'), 5_000);
      let stdout = '';
      let stderr = '';
      wrapperChild.stdout.on('data', (chunk) => {
        stdout += chunk.toString('utf8');
      });
      wrapperChild.stderr.on('data', (chunk) => {
        stderr += chunk.toString('utf8');
      });
      wrapperChild.once('error', reject);
      wrapperChild.once('exit', (code, signal) => resolve({ code, signal, stdout, stderr }));
    });

    assert.equal(result.signal, 'SIGTERM', result.stderr);
    assert.equal(result.code, null, result.stderr);
    assert.deepEqual(
      requests.slice(0, 3).map((request) => request.params.source),
      ['herdr:tps:codex', 'herdr:tps:omp', 'herdr:tps'],
    );
    assert.ok(
      requests.some((request) => request.params.tokens?.model === 'gpt-6-astra'),
      'expected the actual root model after resolving the unknown active thread',
    );
    assert.ok(
      requests.some((request) => Number(request.params.tokens?.tps) > 0),
      'expected a non-zero rate publication from visible output',
    );
    const actualModelIndex = requests.findIndex(
      (request) => request.params.tokens?.model === 'gpt-6-astra',
    );
    assert.equal(
      requests
        .slice(actualModelIndex)
        .some((request) => request.params.tokens?.model === 'gpt-5.6-luna'),
      false,
      'a later parentless system thread must not overwrite the actual model',
    );
    assert.equal(signalSent, true, 'expected root model deletion before shutdown');
    assert.deepEqual(requests.at(-1).params.tokens, { model: null, tps: null });
    assert.equal(requests.at(-1).params.source, 'herdr:tps');
  } finally {
    clearTimeout(watchdog);
    if (wrapperChild?.exitCode === null && wrapperChild.signalCode === null) {
      wrapperChild.kill('SIGKILL');
    }
    await new Promise((resolve) => metadataServer.close(resolve));
    await rm(directory, { recursive: true });
  }
});

test('forwards shutdown signals to the direct Codex child outside Herdr', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-codex-direct-'));
  const fakeCodex = path.join(directory, 'codex-fixture.mjs');
  const readyPath = path.join(directory, 'ready');
  const stoppedPath = path.join(directory, 'stopped');
  const wrapper = fileURLToPath(new URL('../codex-with-tps.mjs', import.meta.url));
  let wrapperChild;

  await writeFile(
    fakeCodex,
    `#!/usr/bin/env node
import { writeFile } from 'node:fs/promises';

await writeFile(process.env.CODEX_FIXTURE_READY, 'ready');
process.once('SIGTERM', async () => {
  await writeFile(process.env.CODEX_FIXTURE_STOPPED, 'stopped');
  process.exit(0);
});
setInterval(() => {}, 1_000);
`,
    'utf8',
  );
  await chmod(fakeCodex, 0o755);

  try {
    const exited = new Promise((resolve, reject) => {
      wrapperChild = spawn(process.execPath, [wrapper], {
        env: {
          ...process.env,
          HERDR_ENV: '0',
          HERDR_TPS_CODEX_BIN: fakeCodex,
          CODEX_FIXTURE_READY: readyPath,
          CODEX_FIXTURE_STOPPED: stoppedPath,
        },
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      let stderr = '';
      wrapperChild.stderr.on('data', (chunk) => {
        stderr += chunk.toString('utf8');
      });
      wrapperChild.once('error', reject);
      wrapperChild.once('exit', (code, signal) => resolve({ code, signal, stderr }));
    });

    await waitForPath(readyPath);
    wrapperChild.kill('SIGTERM');
    const result = await exited;
    await waitForPath(stoppedPath);

    assert.equal(result.signal, 'SIGTERM', result.stderr);
    assert.equal(result.code, null, result.stderr);
  } finally {
    if (wrapperChild?.exitCode === null && wrapperChild.signalCode === null) {
      wrapperChild.kill('SIGKILL');
    }
    await rm(directory, { recursive: true });
  }
});

test('TUI spawn failure exits promptly and cleans up the running App Server', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-spawn-failure-'));
  const fakeCodex = path.join(directory, 'codex');
  const stopped = path.join(directory, 'stopped');
  await writeFile(
    fakeCodex,
    `#!/usr/bin/env node
import http from 'node:http';
import { chmodSync, writeFileSync } from 'node:fs';
const address = new URL(process.argv[4]);
const server = http.createServer((request, response) => {
  chmodSync(process.argv[1], 0);
  response.writeHead(200); response.end();
});
process.once('SIGTERM', () => { writeFileSync(process.env.STOPPED, 'yes'); process.exit(0); });
server.listen(Number(address.port), address.hostname);
`,
  );
  await chmod(fakeCodex, 0o755);
  try {
    const result = spawnSync(
      process.execPath,
      [fileURLToPath(new URL('../codex-with-tps.mjs', import.meta.url))],
      {
        env: {
          ...process.env,
          HERDR_ENV: '1',
          HERDR_PANE_ID: 'test',
          HERDR_SOCKET_PATH: path.join(directory, 'absent.sock'),
          HERDR_TPS_CODEX_BIN: fakeCodex,
          STOPPED: stopped,
        },
        encoding: 'utf8',
        timeout: 4_000,
      },
    );
    assert.equal(result.error, undefined, result.stderr);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /EACCES/);
    await waitForPath(stopped);
  } finally {
    await rm(directory, { recursive: true });
  }
});

test('refuses recursive original-command aliases without launching another wrapper', () => {
  const wrapper = fileURLToPath(new URL('../codex-with-tps.mjs', import.meta.url));
  const result = spawnSync(process.execPath, [wrapper], {
    env: { ...process.env, HERDR_ENV: '0', HERDR_TPS_CODEX_BIN: wrapper },
    encoding: 'utf8',
    timeout: 2_000,
  });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /指向 wrapper/);
});
