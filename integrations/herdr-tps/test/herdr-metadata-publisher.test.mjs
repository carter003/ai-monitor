import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {
  DEFAULT_METADATA_TIMEOUT_MS,
  HerdrMetadataPublisher,
} from '../lib/herdr-metadata-publisher.mjs';

test('sends a pane.report_metadata request with a compact numeric speed', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-test-'));
  const socketPath = path.join(directory, 'herdr.sock');
  let resolveRequest;
  const received = new Promise((resolve) => {
    resolveRequest = resolve;
  });
  const server = net.createServer((socket) => {
    let input = '';
    socket.on('data', (chunk) => {
      input += chunk.toString('utf8');
      const lineEnd = input.indexOf('\n');
      if (lineEnd < 0) {
        return;
      }
      resolveRequest(JSON.parse(input.slice(0, lineEnd)));
      socket.end(
        `${JSON.stringify({ id: JSON.parse(input.slice(0, lineEnd)).id, result: { type: 'ok' } })}\n`,
      );
    });
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(socketPath, resolve);
  });

  const previousHerdrEnv = process.env.HERDR_ENV;
  process.env.HERDR_ENV = '1';
  try {
    const publisher = new HerdrMetadataPublisher({ paneId: 'pane-1', socketPath });
    assert.equal(await publisher.publishRate(37, 2_500), true);
    const request = await received;
    assert.equal(request.method, 'pane.report_metadata');
    assert.deepEqual(request.params.tokens, { tps: '37.0' });
    assert.ok(request.params.ttl_ms > 0 && request.params.ttl_ms <= 2_500);
  } finally {
    if (previousHerdrEnv === undefined) {
      delete process.env.HERDR_ENV;
    } else {
      process.env.HERDR_ENV = previousHerdrEnv;
    }
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true });
  }
});

// OMPCODE marks a shell OMP spawned; HERDR_TPS_OMP_NESTED marks a wrapper that
// already detected nesting. Both are set by different paths, so each must
// suppress publishing on its own.
for (const marker of ['OMPCODE', 'HERDR_TPS_OMP_NESTED']) {
  test(`does not publish metadata from an OMP process nested via ${marker}`, async () => {
    const previous = {
      HERDR_ENV: process.env.HERDR_ENV,
      OMPCODE: process.env.OMPCODE,
      HERDR_TPS_OMP_NESTED: process.env.HERDR_TPS_OMP_NESTED,
    };
    process.env.HERDR_ENV = '1';
    delete process.env.OMPCODE;
    delete process.env.HERDR_TPS_OMP_NESTED;
    process.env[marker] = '1';
    try {
      const requests = [];
      const publisher = new HerdrMetadataPublisher({
        paneId: 'pane-1',
        socketPath: '/unused/test.sock',
        agent: 'omp',
      });
      publisher.send = async (request) => requests.push(request);

      assert.equal(publisher.enabled, false);
      assert.equal(await publisher.publishRate(12, 2_000), undefined);
      assert.equal(requests.length, 0);
    } finally {
      for (const [name, value] of Object.entries(previous)) {
        if (value === undefined) delete process.env[name];
        else process.env[name] = value;
      }
    }
  });
}

test('nesting markers do not suppress a non-OMP agent', async () => {
  const previous = {
    HERDR_ENV: process.env.HERDR_ENV,
    OMPCODE: process.env.OMPCODE,
    HERDR_TPS_OMP_NESTED: process.env.HERDR_TPS_OMP_NESTED,
  };
  process.env.HERDR_ENV = '1';
  process.env.OMPCODE = '1';
  delete process.env.HERDR_TPS_OMP_NESTED;
  try {
    const requests = [];
    const publisher = new HerdrMetadataPublisher({
      paneId: 'pane-1',
      socketPath: '/unused/test.sock',
      agent: 'codex',
    });
    publisher.send = async (request) => {
      requests.push(request);
      return true;
    };

    assert.equal(publisher.enabled, true);
    await publisher.publishRate(12, 2_000);
    assert.equal(requests.length, 1);
  } finally {
    for (const [name, value] of Object.entries(previous)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  }
});

test('clears legacy sources before claiming canonical model and rate tokens', async () => {
  const previousHerdrEnv = process.env.HERDR_ENV;
  process.env.HERDR_ENV = '1';
  try {
    const requests = [];
    const publisher = new HerdrMetadataPublisher({
      paneId: 'pane-1',
      socketPath: '/unused/test.sock',
      agent: 'codex',
      appliesToSource: 'herdr:codex',
    });
    publisher.send = async (request) => requests.push(request);

    await publisher.resetForAgent(5_000);

    assert.deepEqual(
      requests.map((request) => [
        request.params.source,
        request.params.agent,
        request.params.applies_to_source,
        request.params.display_agent,
        request.params.tokens,
      ]),
      [
        ['herdr:tps:codex', undefined, undefined, null, { model: null, tps: null }],
        ['herdr:tps:omp', undefined, undefined, null, { model: null, tps: null }],
        ['herdr:tps', undefined, undefined, null, { model: null, tps: '0' }],
      ],
    );

    await publisher.publishModel('gpt-5.6', 5_000);
    assert.equal(requests.at(-1).params.source, 'herdr:tps');
    assert.equal(requests.at(-1).params.agent, undefined);
    assert.equal(requests.at(-1).params.applies_to_source, undefined);
  } finally {
    if (previousHerdrEnv === undefined) {
      delete process.env.HERDR_ENV;
    } else {
      process.env.HERDR_ENV = previousHerdrEnv;
    }
  }
});

test('surfaces Herdr API errors without breaking the publisher queue', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-error-'));
  const socketPath = path.join(directory, 'herdr.sock');
  const server = net.createServer((socket) => {
    let input = '';
    socket.on('data', (chunk) => {
      input += chunk.toString('utf8');
      const lineEnd = input.indexOf('\n');
      if (lineEnd < 0) {
        return;
      }
      const request = JSON.parse(input.slice(0, lineEnd));
      socket.end(
        `${JSON.stringify({ id: request.id, error: { code: 'not_found', message: 'pane not found' } })}\n`,
      );
    });
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(socketPath, resolve);
  });

  const previousHerdrEnv = process.env.HERDR_ENV;
  process.env.HERDR_ENV = '1';
  try {
    const errors = [];
    const publisher = new HerdrMetadataPublisher({
      paneId: 'missing-pane',
      socketPath,
      onError: (error) => errors.push(error.message),
    });
    assert.equal(await publisher.publishRate(12, 2_000), false);
    assert.deepEqual(errors, ['not_found: pane not found']);
  } finally {
    if (previousHerdrEnv === undefined) {
      delete process.env.HERDR_ENV;
    } else {
      process.env.HERDR_ENV = previousHerdrEnv;
    }
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true });
  }
});

test('keeps the canonical source when publishing a guarded OMP display name', async () => {
  const previousHerdrEnv = process.env.HERDR_ENV;
  const previousOmpCode = process.env.OMPCODE;
  delete process.env.OMPCODE;
  process.env.HERDR_ENV = '1';
  try {
    const requests = [];
    const publisher = new HerdrMetadataPublisher({
      paneId: 'pane-1',
      socketPath: '/unused/test.sock',
      // Deliberately ignored: callers cannot split the canonical metadata owner again.
      source: 'herdr:tps:omp',
      agent: 'omp',
      appliesToSource: 'herdr:omp',
    });
    publisher.send = async (request) => requests.push(request);

    await publisher.publishDisplayAgent('omp2', 5_000);
    assert.equal(requests[0].method, 'pane.report_metadata');
    assert.equal(requests[0].params.source, 'herdr:tps');
    assert.equal(requests[0].params.agent, 'omp');
    assert.equal(requests[0].params.applies_to_source, 'herdr:omp');
    assert.equal(requests[0].params.display_agent, 'omp2');
    assert.equal(requests[0].params.tokens, undefined);
    assert.ok(requests[0].params.ttl_ms > 0 && requests[0].params.ttl_ms <= 5_000);
  } finally {
    if (previousHerdrEnv === undefined) {
      delete process.env.HERDR_ENV;
    } else {
      process.env.HERDR_ENV = previousHerdrEnv;
    }
    if (previousOmpCode === undefined) delete process.env.OMPCODE;
    else process.env.OMPCODE = previousOmpCode;
  }
});

test('publishes one guarded heartbeat snapshot for idle OMP metadata', async () => {
  const previousHerdrEnv = process.env.HERDR_ENV;
  const previousOmpCode = process.env.OMPCODE;
  delete process.env.OMPCODE;
  process.env.HERDR_ENV = '1';
  try {
    const requests = [];
    const publisher = new HerdrMetadataPublisher({
      paneId: 'pane-1',
      socketPath: '/unused/test.sock',
      agent: 'omp',
      appliesToSource: 'herdr:omp',
    });
    publisher.send = async (request) => requests.push(request);

    await publisher.publishSnapshot(
      { model: 'gemini-3.7-flash', rate: 0, displayAgent: 'omp2' },
      5_000,
    );

    assert.equal(requests.length, 1);
    assert.deepEqual(requests[0].params.tokens, { model: 'gemini-3.7-flash', tps: '0.0' });
    assert.equal(requests[0].params.display_agent, 'omp2');
    assert.equal(requests[0].params.agent, 'omp');
    assert.ok(requests[0].params.ttl_ms > 0 && requests[0].params.ttl_ms <= 5_000);
  } finally {
    if (previousHerdrEnv === undefined) {
      delete process.env.HERDR_ENV;
    } else {
      process.env.HERDR_ENV = previousHerdrEnv;
    }
    if (previousOmpCode === undefined) delete process.env.OMPCODE;
    else process.env.OMPCODE = previousOmpCode;
  }
});

test('configures timeout with default, environment variable, and explicit override', () => {
  const previousEnv = process.env.HERDR_TPS_TIMEOUT_MS;
  try {
    delete process.env.HERDR_TPS_TIMEOUT_MS;
    const defaultPublisher = new HerdrMetadataPublisher();
    assert.equal(defaultPublisher.timeoutMs, DEFAULT_METADATA_TIMEOUT_MS);
    assert.equal(defaultPublisher.timeoutMs, 2_000);

    process.env.HERDR_TPS_TIMEOUT_MS = '3500';
    const envPublisher = new HerdrMetadataPublisher();
    assert.equal(envPublisher.timeoutMs, 3_500);

    const overridePublisher = new HerdrMetadataPublisher({ timeoutMs: 1_200 });
    assert.equal(overridePublisher.timeoutMs, 1_200);
  } finally {
    if (previousEnv === undefined) {
      delete process.env.HERDR_TPS_TIMEOUT_MS;
    } else {
      process.env.HERDR_TPS_TIMEOUT_MS = previousEnv;
    }
  }
});

test('surfaces timeout error when socket does not respond within timeoutMs', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'herdr-tps-timeout-'));
  const socketPath = path.join(directory, 'herdr.sock');
  const serverSockets = new Set();
  const server = net.createServer((socket) => {
    serverSockets.add(socket);
    socket.once('close', () => serverSockets.delete(socket));
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(socketPath, resolve);
  });

  const previousHerdrEnv = process.env.HERDR_ENV;
  process.env.HERDR_ENV = '1';
  try {
    const errors = [];
    const publisher = new HerdrMetadataPublisher({
      paneId: 'test-pane',
      socketPath,
      timeoutMs: 50,
      onError: (error) => errors.push(error.message),
    });
    const result = await publisher.publishRate(42, 2_000);
    assert.equal(result, false);
    assert.equal(errors.length, 1);
    assert.equal(errors[0], 'Herdr metadata request timed out after 50ms');
  } finally {
    if (previousHerdrEnv === undefined) {
      delete process.env.HERDR_ENV;
    } else {
      process.env.HERDR_ENV = previousHerdrEnv;
    }
    for (const socket of serverSockets) {
      socket.destroy();
    }
    server.closeAllConnections?.();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true });
  }
});

test('coalesces a blocked metadata backlog and clears without replaying obsolete speeds', async () => {
  const previous = process.env.HERDR_ENV;
  process.env.HERDR_ENV = '1';
  try {
    let release;
    const blocked = new Promise((resolve) => {
      release = resolve;
    });
    const sent = [];
    const publisher = new HerdrMetadataPublisher({ paneId: 'pane', socketPath: '/unused' });
    publisher.send = async (request) => {
      sent.push(request);
      if (sent.length === 1) await blocked;
      return true;
    };
    const promises = [publisher.publishRate(1, 2_500)];
    for (let rate = 2; rate <= 1_000; rate++) promises.push(publisher.publishRate(rate, 2_500));
    assert.equal(publisher.pending.size, 1);
    const cleared = publisher.clearAll();
    release();
    await Promise.all([...promises, cleared]);
    assert.equal(
      sent.length,
      2,
      'only the in-flight rate and final cleanup should reach the socket',
    );
    assert.deepEqual(sent.at(-1).params.tokens, { model: null, tps: null });
    assert.ok(sent.at(-1).params.seq > sent[0].params.seq);
  } finally {
    if (previous === undefined) delete process.env.HERDR_ENV;
    else process.env.HERDR_ENV = previous;
  }
});

test('drops expired queued metadata and survives a throwing diagnostic callback', async () => {
  const previous = process.env.HERDR_ENV;
  process.env.HERDR_ENV = '1';
  try {
    let release;
    const blocked = new Promise((resolve) => {
      release = resolve;
    });
    const sent = [];
    const publisher = new HerdrMetadataPublisher({
      paneId: 'pane',
      socketPath: '/unused',
      onError: () => {
        throw new Error('logging failed');
      },
    });
    publisher.send = async (request) => {
      sent.push(request);
      if (sent.length === 1) {
        await blocked;
        throw new Error('socket failed');
      }
      return true;
    };
    const first = publisher.publishModel('model', 5_000);
    const expired = publisher.publishRate(100, 1);
    await new Promise((resolve) => setTimeout(resolve, 10));
    const fresh = publisher.publishSnapshot({ model: 'model', rate: 0 }, 5_000);
    release();
    assert.deepEqual(await Promise.all([first, expired, fresh]), [false, false, true]);
    assert.equal(sent.length, 2);
    assert.equal(sent.at(-1).params.tokens.tps, '0.0');
  } finally {
    if (previous === undefined) delete process.env.HERDR_ENV;
    else process.env.HERDR_ENV = previous;
  }
});

test('deletes the model token after queued snapshots without clearing display name or idle rate', async () => {
  const previous = process.env.HERDR_ENV;
  process.env.HERDR_ENV = '1';
  try {
    let release;
    const blocked = new Promise((resolve) => {
      release = resolve;
    });
    const sent = [];
    const publisher = new HerdrMetadataPublisher({ paneId: 'pane', socketPath: '/unused' });
    publisher.send = async (request) => {
      sent.push(request);
      if (sent.length === 1) await blocked;
      return true;
    };
    const stale = publisher.publishSnapshot(
      { model: 'old-model', rate: 0, displayAgent: 'omp2' },
      5_000,
    );
    const queued = publisher.publishSnapshot({ model: 'old-model', rate: 0 }, 5_000);
    const cleared = publisher.clearModel();
    const heartbeat = publisher.publishSnapshot({ rate: 0 }, 5_000);
    release();
    await Promise.all([stale, queued, cleared, heartbeat]);
    const deletion = sent.findIndex((request) => request.params.tokens?.model === null);
    assert.ok(deletion > 0);
    assert.deepEqual(sent[deletion].params.tokens, { model: null });
    assert.equal(sent[deletion].params.display_agent, undefined);
    assert.equal(sent[deletion].params.ttl_ms, undefined);
    assert.equal(sent[deletion].params.source, 'herdr:tps');
    assert.ok(
      sent.slice(deletion + 1).every((request) => request.params.tokens?.model === undefined),
    );
    assert.equal(sent.at(-1).params.tokens.tps, '0.0');
  } finally {
    if (previous === undefined) delete process.env.HERDR_ENV;
    else process.env.HERDR_ENV = previous;
  }
});
