import assert from 'node:assert/strict';
import test from 'node:test';
import { CodexTpsObserver } from '../lib/codex-tps-observer.mjs';
import { LiveTpsReporter } from '../lib/live-tps-reporter.mjs';

function observerFixture(readThread) {
  const events = [];
  const observer = new CodexTpsObserver({
    readThread,
    reporter: {
      setModel: (model) => events.push(['model', model]),
      start: (id) => events.push(['start', id]),
      append: (id) => events.push(['delta', id]),
      pause: () => events.push(['pause']),
      resetSession: () => events.push(['reset']),
    },
  });
  return {
    observer,
    events,
    client: (message) => observer.observeClientMessage(JSON.stringify(message)),
    server: (method, params) => observer.observeServerEvent({ method, params }),
  };
}

const nextMicrotasks = () => new Promise((resolve) => setImmediate(resolve));

for (const discovery of ['response', 'notification', 'read']) {
  for (const activeTurn of [false, true]) {
    test(`ignores parentless system threads via ${discovery} while the user is ${activeTurn ? 'working' : 'idle'}`, async () => {
      // Codex 0.153.4 live metadata: the auxiliary Luna thread has no parent.
      const system = {
        id: 'system',
        parentThreadId: null,
        source: 'vscode',
        threadSource: 'system',
        ephemeral: true,
        model: 'gpt-5.6-luna',
      };
      const { observer, events, client, server } = observerFixture(async () => system);
      client({ id: 1, method: 'thread/start', params: {} });
      observer.observeServerEvent({
        id: 1,
        result: {
          thread: { id: 'user', parentThreadId: null, threadSource: 'user' },
          model: 'gpt-6-astra',
        },
      });
      if (activeTurn) server('turn/started', { threadId: 'user', turn: { id: 'user-turn' } });
      const beforeSystem = events.slice();

      if (discovery === 'response') {
        client({ id: 2, method: 'thread/start', params: { model: system.model } });
        observer.observeServerEvent({ id: 2, result: { thread: system, model: system.model } });
      } else if (discovery === 'notification') {
        server('thread/started', { thread: system });
      }
      server('thread/settings/updated', {
        threadId: 'system',
        threadSettings: { model: system.model },
      });
      client({ id: 3, method: 'turn/start', params: { threadId: 'system', model: system.model } });
      server('turn/started', { threadId: 'system', turn: { id: 'system-turn' } });
      server('item/agentMessage/delta', {
        threadId: 'system',
        turnId: 'system-turn',
        itemId: 'hidden',
        delta: 'internal output',
      });
      server('turn/completed', { threadId: 'system', turn: { id: 'system-turn' } });
      await nextMicrotasks();
      server('thread/closed', { threadId: 'system' });

      assert.equal(observer.rootThreadId, 'user');
      assert.equal(observer.model, 'gpt-6-astra');
      assert.equal(observer.currentTurnId, activeTurn ? 'user-turn' : undefined);
      assert.deepEqual(events, beforeSystem, 'system output must not affect the shared reporter');
      if (!activeTurn) server('turn/started', { threadId: 'user', turn: { id: 'user-turn' } });
      server('item/agentMessage/delta', {
        threadId: 'user',
        turnId: 'user-turn',
        itemId: 'answer',
        delta: 'visible output',
      });
      assert.deepEqual(events.at(-1), ['delta', 'user-turn']);
    });
  }
}

test('still accepts ephemeral user threads', () => {
  const { observer, server } = observerFixture();
  server('thread/started', {
    thread: {
      id: 'user',
      parentThreadId: null,
      threadSource: 'user',
      ephemeral: true,
      model: 'gpt-6-astra',
    },
  });
  server('turn/started', { threadId: 'user', turn: { id: 'turn' } });
  assert.equal(observer.rootThreadId, 'user');
  assert.equal(observer.model, 'gpt-6-astra');
});

test('tracks only Codex reasoning and normal answer deltas, then idles', () => {
  const events = [];
  const reporter = {
    setModel: (model) => events.push(['model', model]),
    start: (turnId, model, now) => events.push(['start', turnId, model, now]),
    append: (turnId, streamKey, delta, model, now) =>
      events.push(['delta', turnId, streamKey, delta, model, now]),
    pause: (now) => events.push(['pause', now]),
  };
  const observer = new CodexTpsObserver({ reporter });

  observer.observeClientMessage(
    JSON.stringify({ id: 1, method: 'thread/start', params: { model: 'gpt-5.5' } }),
  );
  observer.observeClientMessage(JSON.stringify({ id: 2, method: 'model/list', params: {} }));
  assert.equal(observer.pendingRequests.size, 1);
  observer.observeServerMessage(
    JSON.stringify({ id: 1, result: { thread: { id: 'root' }, model: 'gpt-5.6' } }),
  );
  assert.equal(observer.pendingRequests.size, 0);
  observer.observeServerMessage(
    JSON.stringify({
      method: 'turn/started',
      emittedAtMs: 90,
      params: {
        threadId: 'root',
        turn: { id: 'turn-1' },
      },
    }),
  );
  observer.observeServerMessage(
    JSON.stringify({
      method: 'item/agentMessage/delta',
      params: { threadId: 'subagent', itemId: 'ignored', delta: 'hidden' },
    }),
  );
  observer.observeServerMessage(
    JSON.stringify({
      method: 'item/reasoning/summaryTextDelta',
      emittedAtMs: 100,
      params: {
        threadId: 'root',
        turnId: 'turn-1',
        itemId: 'reasoning-1',
        summaryIndex: 0,
        delta: 'thinking',
      },
    }),
  );
  observer.observeServerMessage(
    JSON.stringify({
      method: 'model/rerouted',
      params: {
        threadId: 'root',
        turnId: 'turn-1',
        fromModel: 'gpt-5.6',
        toModel: 'gpt-5.6-mini',
      },
    }),
  );
  observer.observeServerMessage(
    JSON.stringify({
      method: 'thread/settings/updated',
      params: {
        threadId: 'root',
        threadSettings: {
          model: 'gpt-5.6-luna',
          collaborationMode: {
            mode: 'default',
            settings: { model: 'gpt-5.6-sol' },
          },
        },
      },
    }),
  );
  observer.observeServerMessage(
    JSON.stringify({
      method: 'item/agentMessage/delta',
      emittedAtMs: 110,
      params: { threadId: 'root', itemId: 'message-1', delta: 'hello' },
    }),
  );
  observer.observeServerMessage(
    JSON.stringify({
      method: 'thread/tokenUsage/updated',
      emittedAtMs: 120,
      params: {
        threadId: 'root',
        turnId: 'turn-1',
        tokenUsage: { last: { outputTokens: 37 } },
      },
    }),
  );
  observer.observeServerMessage(
    JSON.stringify({
      method: 'turn/completed',
      emittedAtMs: 130,
      params: { threadId: 'root', turn: { id: 'turn-1', status: 'completed' } },
    }),
  );

  assert.deepEqual(events, [
    ['model', 'gpt-5.6'],
    ['start', 'turn-1', 'gpt-5.6', 90],
    ['delta', 'turn-1', 'reasoning-summary:reasoning-1:0', 'thinking', 'gpt-5.6', 100],
    ['model', 'gpt-5.6-mini'],
    ['model', 'gpt-5.6-sol'],
    ['delta', 'turn-1', 'agent:message-1', 'hello', 'gpt-5.6-sol', 110],
    ['pause', 130],
  ]);
});

test('prefers the effective collaboration-mode model over the base thread model', () => {
  const events = [];
  const observer = new CodexTpsObserver({
    reporter: {
      setModel: (model) => events.push(model),
      start: () => {},
      append: () => {},
      pause: () => {},
    },
  });

  observer.observeClientMessage(
    JSON.stringify({ id: 1, method: 'thread/resume', params: { threadId: 'root' } }),
  );
  observer.observeServerMessage(
    JSON.stringify({ id: 1, result: { thread: { id: 'root' }, model: 'gpt-5.6-luna' } }),
  );
  observer.observeClientMessage(
    JSON.stringify({
      id: 2,
      method: 'turn/start',
      params: {
        threadId: 'root',
        model: 'gpt-5.6-luna',
        collaborationMode: {
          mode: 'default',
          settings: { model: 'gpt-5.6-sol' },
        },
      },
    }),
  );

  assert.deepEqual(events, ['gpt-5.6-luna', 'gpt-5.6-sol']);
});

test('rebinds active roots without accepting child threads or late placeholder responses', () => {
  const events = [];
  const observer = new CodexTpsObserver({
    reporter: {
      setModel: (model) => events.push(['model', model]),
      start: (id) => events.push(['start', id]),
      append: (id, _key, _delta, model) => events.push(['delta', id, model]),
      pause: () => {},
    },
  });
  const server = (message) => observer.observeServerMessage(JSON.stringify(message));
  const client = (message) => observer.observeClientMessage(JSON.stringify(message));
  client({ id: 1, method: 'thread/start', params: {} });
  server({
    id: 1,
    result: { thread: { id: 'placeholder', parentThreadId: null }, model: 'gpt-5.6-luna' },
  });
  server({
    method: 'thread/started',
    params: { thread: { id: 'active', parentThreadId: null, model: 'gpt-5.6-luna' } },
  });
  client({
    id: 2,
    method: 'turn/start',
    params: {
      threadId: 'active',
      model: 'gpt-5.6-luna',
      collaborationMode: { settings: { model: 'gpt-6-astra' } },
    },
  });
  server({ method: 'turn/started', params: { threadId: 'active', turn: { id: 'turn-active' } } });
  client({ id: 3, method: 'thread/start', params: {} });
  server({
    id: 3,
    result: { thread: { id: 'late', parentThreadId: null }, model: 'gpt-5.6-luna' },
  });
  server({
    method: 'thread/started',
    params: { thread: { id: 'child', parentThreadId: 'active', model: 'gpt-5.6-sol' } },
  });
  server({ method: 'turn/started', params: { threadId: 'child', turn: { id: 'child-turn' } } });
  server({
    method: 'item/agentMessage/delta',
    params: { threadId: 'child', itemId: 'child-item', delta: 'ignored' },
  });
  server({
    method: 'item/agentMessage/delta',
    params: { threadId: 'active', itemId: 'answer', delta: 'visible' },
  });
  assert.equal(observer.rootThreadId, 'active');
  assert.equal(observer.model, 'gpt-6-astra');
  assert.deepEqual(
    events.filter(([type]) => type === 'delta'),
    [['delta', 'turn-active', 'gpt-6-astra']],
  );
});

test('resolves unknown turns read-only and excludes discovered subagents', async () => {
  const events = [];
  const reads = [];
  const observer = new CodexTpsObserver({
    readThread: async (id) => {
      reads.push(id);
      return { id, parentThreadId: id === 'child' ? 'root' : null, model: 'gpt-6-astra' };
    },
    reporter: {
      setModel: () => {},
      start: () => {},
      pause: () => {},
      append: (id) => events.push(id),
    },
  });
  for (const threadId of ['root', 'child']) {
    observer.observeServerMessage(
      JSON.stringify({
        method: 'turn/started',
        params: { threadId, turn: { id: `${threadId}-turn` } },
      }),
    );
    observer.observeServerMessage(
      JSON.stringify({
        method: 'item/agentMessage/delta',
        params: { threadId, itemId: 'answer', delta: 'text' },
      }),
    );
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.deepEqual(reads, ['root', 'child']);
  assert.deepEqual(events, ['root-turn']);
  assert.equal(observer.rootThreadId, 'root');
});

test('failed thread reads release buffered events and permit a later retry', async () => {
  let attempts = 0;
  const observer = new CodexTpsObserver({
    readThread: async (id) => {
      if (++attempts === 1) throw new Error('connection unavailable');
      return { id, parentThreadId: null, model: 'gpt-6-astra' };
    },
    reporter: { setModel: () => {}, start: () => {}, pause: () => {}, append: () => {} },
  });
  const started = JSON.stringify({
    method: 'turn/started',
    params: { threadId: 'root', turn: { id: 'turn' } },
  });
  observer.observeServerMessage(started);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(observer.resolvingThreads.size, 0);
  assert.equal(observer.rootThreadId, undefined);
  observer.observeServerMessage(started);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(observer.rootThreadId, 'root');
});

test('retains only routing metadata and rejects late events from an older turn', () => {
  const events = [];
  const observer = new CodexTpsObserver({
    reporter: {
      setModel: () => {},
      start: () => {},
      append: () => events.push('delta'),
      pause: () => events.push('pause'),
    },
  });
  observer.observeClientMessage(
    JSON.stringify({ id: 1, method: 'thread/resume', params: { history: ['large input'] } }),
  );
  assert.deepEqual(observer.pendingRequests.get('1'), { model: undefined, sequence: 1 });
  observer.observeServerEvent({
    id: 1,
    result: {
      thread: { id: 'root', parentThreadId: null, turns: ['large history'] },
      model: 'model',
    },
  });
  assert.equal('turns' in observer.threads.get('root'), false);
  observer.observeServerEvent({
    method: 'turn/started',
    params: { threadId: 'root', turn: { id: 'new' } },
  });
  observer.observeServerEvent({
    method: 'item/agentMessage/delta',
    params: { threadId: 'root', turnId: 'old', itemId: 'old', delta: 'late' },
  });
  observer.observeServerEvent({
    method: 'turn/completed',
    params: { threadId: 'root', turn: { id: 'old' } },
  });
  assert.deepEqual(events, []);
  assert.equal(observer.currentTurnId, 'new');
  observer.observeServerEvent({
    method: 'item/agentMessage/delta',
    params: { threadId: 'root', turnId: 'new', itemId: 'answer', delta: 'current' },
  });
  assert.deepEqual(events, ['delta']);
});

test('bounds unresolved output buffers, ignores tool output and cancels late reads on close', async () => {
  let release;
  const read = new Promise((resolve) => {
    release = resolve;
  });
  const events = [];
  const observer = new CodexTpsObserver({
    readThread: () => read,
    reporter: {
      setModel: () => events.push('model'),
      start: () => events.push('start'),
      append: () => {},
      pause: () => {},
    },
  });
  observer.observeServerEvent({
    method: 'turn/started',
    params: { threadId: 'root', turn: { id: 'turn' } },
  });
  observer.observeServerEvent({
    method: 'item/commandExecution/outputDelta',
    params: { threadId: 'root', delta: 'x'.repeat(300_000) },
  });
  assert.equal(observer.resolvingThreads.get('root').messages.length, 1);
  observer.observeServerEvent({
    method: 'item/agentMessage/delta',
    params: { threadId: 'root', delta: 'x'.repeat(300_000) },
  });
  assert.equal(observer.resolvingThreads.size, 0);
  observer.close();
  release({ id: 'root', parentThreadId: null, model: 'model' });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(observer.threads.size, 0);
  assert.deepEqual(events, []);
});

for (const origin of ['settings', 'turn']) {
  for (const discovery of ['notification', 'read']) {
    test(`preserves early ${origin} model through ${discovery} without admitting subagents`, async () => {
      const { observer, events, client, server } = observerFixture(async (id) => ({
        id,
        parentThreadId: id === 'child' ? 'active' : null,
        model: 'gpt-5.6-luna',
      }));
      for (const threadId of ['active', 'child']) {
        const settings = {
          model: 'gpt-5.6-luna',
          collaborationMode: { settings: { model: 'gpt-6-astra' } },
        };
        if (origin === 'settings') {
          server('thread/settings/updated', { threadId, threadSettings: settings });
        } else {
          client({ id: threadId, method: 'turn/start', params: { threadId, ...settings } });
        }
        if (discovery === 'notification') {
          server('thread/started', {
            thread: {
              id: threadId,
              parentThreadId: threadId === 'child' ? 'active' : null,
              model: 'gpt-5.6-luna',
            },
          });
        }
        server('turn/started', { threadId, turn: { id: `${threadId}-turn` } });
        server('item/agentMessage/delta', {
          threadId,
          turnId: `${threadId}-turn`,
          itemId: 'answer',
          delta: 'visible output',
        });
        await nextMicrotasks();
      }
      assert.equal(observer.rootThreadId, 'active');
      assert.equal(observer.model, 'gpt-6-astra');
      assert.deepEqual(
        events.filter(([type]) => type === 'model'),
        [['model', 'gpt-6-astra']],
      );
      assert.deepEqual(
        events.filter(([type]) => type === 'delta'),
        [['delta', 'active-turn']],
      );
      assert.equal(observer.pendingModels.size, 0);
    });
  }
}

for (const firstResolved of ['older', 'newer']) {
  test(`keeps the newest root when ${firstResolved} thread/read resolves first`, async () => {
    const reads = new Map();
    const { observer, events, server } = observerFixture(
      (id) => new Promise((resolve) => reads.set(id, resolve)),
    );
    for (const threadId of ['older', 'newer']) {
      server('turn/started', { threadId, turn: { id: `${threadId}-turn` } });
    }
    await nextMicrotasks();
    for (const id of [firstResolved, firstResolved === 'older' ? 'newer' : 'older']) {
      reads.get(id)({
        id,
        parentThreadId: null,
        model: id === 'older' ? 'gpt-5.6-luna' : 'gpt-6-astra',
      });
      await nextMicrotasks();
    }
    server('item/agentMessage/delta', {
      threadId: 'newer',
      turnId: 'newer-turn',
      itemId: 'answer',
      delta: 'current output',
    });
    assert.equal(observer.rootThreadId, 'newer');
    assert.equal(observer.model, 'gpt-6-astra');
    assert.deepEqual(
      events.filter(([type]) => type === 'delta'),
      [['delta', 'newer-turn']],
    );
  });
}

test('a discovered child cannot suppress a pending root activation', async () => {
  const reads = new Map();
  const { observer, server } = observerFixture(
    (id) => new Promise((resolve) => reads.set(id, resolve)),
  );
  server('turn/started', { threadId: 'root', turn: { id: 'root-turn' } });
  server('turn/started', { threadId: 'child', turn: { id: 'child-turn' } });
  await nextMicrotasks();
  reads.get('child')({ id: 'child', parentThreadId: 'root', model: 'gpt-5.6-luna' });
  await nextMicrotasks();
  reads.get('root')({ id: 'root', parentThreadId: null, model: 'gpt-6-astra' });
  await nextMicrotasks();
  assert.equal(observer.rootThreadId, 'root');
  assert.equal(observer.model, 'gpt-6-astra');
});

test('late placeholder responses cannot steal a root after its turn completes', () => {
  const { observer, client, server } = observerFixture();
  client({ id: 1, method: 'thread/start', params: {} });
  server('thread/started', {
    thread: { id: 'active', model: 'gpt-6-astra', parentThreadId: null },
  });
  server('turn/started', { threadId: 'active', turn: { id: 'turn' } });
  server('turn/completed', { threadId: 'active', turn: { id: 'turn' } });
  observer.observeServerEvent({
    id: 1,
    result: { thread: { id: 'placeholder', parentThreadId: null }, model: 'gpt-5.6-luna' },
  });
  assert.equal(observer.rootThreadId, 'active');
  assert.equal(observer.model, 'gpt-6-astra');
});

test('accepts only current-turn reroutes and rejects reroutes after completion', () => {
  const { observer, events, server } = observerFixture();
  server('thread/started', { thread: { id: 'root', parentThreadId: null, model: 'gpt-6-astra' } });
  server('turn/started', { threadId: 'root', turn: { id: 'current' } });
  const reroute = (turnId, toModel) =>
    server('model/rerouted', {
      threadId: 'root',
      turnId,
      fromModel: 'gpt-6-astra',
      toModel,
      reason: 'highRiskCyberActivity',
    });
  reroute('old', 'gpt-5.6-luna');
  assert.equal(observer.model, 'gpt-6-astra');
  reroute('current', 'gpt-5.6-sol');
  assert.equal(observer.model, 'gpt-5.6-sol');
  server('turn/completed', { threadId: 'root', turn: { id: 'current' } });
  reroute('current', 'gpt-5.6-luna');
  assert.equal(observer.model, 'gpt-5.6-sol');
  server('turn/started', { threadId: 'root', turn: { id: 'next' } });
  assert.equal(observer.model, 'gpt-6-astra');
  assert.deepEqual(
    events.filter(([type]) => type === 'model'),
    [
      ['model', 'gpt-6-astra'],
      ['model', 'gpt-5.6-sol'],
      ['model', 'gpt-6-astra'],
    ],
  );
});

for (const method of ['thread/closed', 'thread/deleted']) {
  test(`${method} clears the model across heartbeats and permits the same model on a new root`, async () => {
    const events = [];
    const reporter = new LiveTpsReporter({
      autoStart: false,
      publisher: {
        resetForAgent: () => {},
        publishModel: (model) => events.push(['model', model]),
        publishRate: () => {},
        publishSnapshot: (snapshot) => events.push(['snapshot', snapshot.model]),
        clearModel: () => events.push(['clear-model']),
        clearAll: () => {},
      },
    });
    try {
      const observer = new CodexTpsObserver({ reporter });
      observer.observeServerEvent({
        method: 'thread/started',
        params: { thread: { id: 'root', parentThreadId: null, model: 'gpt-6-astra' } },
      });
      observer.observeServerEvent({
        method: 'turn/started',
        params: { threadId: 'root', turn: { id: 'turn' } },
      });
      observer.observeServerEvent({ method, params: { threadId: 'root' } });
      reporter.refreshMetadata();
      reporter.refreshMetadata();
      assert.equal(observer.model, undefined);
      assert.equal(observer.rootThreadId, undefined);
      assert.equal(reporter.currentModel, undefined);
      observer.observeServerEvent({
        method: 'thread/started',
        params: { thread: { id: 'new', parentThreadId: null, model: 'gpt-6-astra' } },
      });
      observer.observeServerEvent({
        method: 'turn/started',
        params: { threadId: 'new', turn: { id: 'new-turn' } },
      });
      assert.deepEqual(events, [
        ['model', 'gpt-6-astra'],
        ['clear-model'],
        ['snapshot', undefined],
        ['snapshot', undefined],
        ['model', 'gpt-6-astra'],
      ]);
    } finally {
      await reporter.close();
    }
  });
}

test('bounds unverified model facts and clears them on thread or connection close', () => {
  const { observer, server } = observerFixture();
  for (let i = 0; i < 1_025; i++) {
    server('thread/settings/updated', { threadId: String(i), threadSettings: { model: 'model' } });
  }
  assert.equal(observer.pendingModels.size, 1_024);
  server('thread/closed', { threadId: '1024' });
  assert.equal(observer.pendingModels.has('1024'), false);
  observer.close();
  assert.equal(observer.pendingModels.size, 0);
});

test('late same-thread responses preserve newer model settings into the next turn', () => {
  const { observer, client, server } = observerFixture();
  client({ id: 1, method: 'thread/start', params: { model: 'gpt-5.6-luna' } });
  server('thread/started', { thread: { id: 'root', parentThreadId: null, model: 'gpt-5.6-luna' } });
  client({
    id: 2,
    method: 'turn/start',
    params: {
      threadId: 'root',
      model: 'gpt-5.6-luna',
      collaborationMode: { settings: { model: 'gpt-6-astra' } },
    },
  });
  server('turn/started', { threadId: 'root', turn: { id: 'first' } });
  observer.observeServerEvent({
    id: 1,
    result: { thread: { id: 'root', parentThreadId: null }, model: 'gpt-5.6-luna' },
  });
  server('turn/completed', { threadId: 'root', turn: { id: 'first' } });
  server('turn/started', { threadId: 'root', turn: { id: 'second' } });
  assert.equal(observer.model, 'gpt-6-astra');
});

test('a delayed read cannot reactivate an older root after the active root closes', async () => {
  let resolveRead;
  const { observer, server } = observerFixture(
    () =>
      new Promise((resolve) => {
        resolveRead = resolve;
      }),
  );
  server('turn/started', { threadId: 'old', turn: { id: 'old-turn' } });
  await nextMicrotasks();
  server('thread/started', {
    thread: { id: 'current', parentThreadId: null, model: 'gpt-6-astra' },
  });
  server('turn/started', { threadId: 'current', turn: { id: 'current-turn' } });
  server('thread/closed', { threadId: 'current' });
  resolveRead({ id: 'old', parentThreadId: null, model: 'gpt-5.6-luna' });
  await nextMicrotasks();
  assert.equal(observer.rootThreadId, undefined);
  assert.equal(observer.model, undefined);
});
