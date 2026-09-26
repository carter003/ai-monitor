import assert from 'node:assert/strict';
import test from 'node:test';
import {
  createOmpMetadataPublisher,
  ompDisplayAgent,
  registerOmpTpsHandlers,
} from '../omp-extension.mjs';

test('restores the omp2 display name from the pro2 session path', () => {
  const context = {
    sessionManager: {
      getSessionFile: () => '/home/user/.omp/profiles/pro2/agent/sessions/project/session.jsonl',
    },
  };

  assert.equal(ompDisplayAgent(context, ['omp', '--resume=session']), 'omp2');
  assert.equal(ompDisplayAgent({}, ['omp', '--profile=pro2']), 'omp2');
  assert.equal(ompDisplayAgent({}, ['omp']), 'omp');
});

test('publishes model and omp2 display name when a pro2 session is restored', () => {
  const handlers = new Map();
  const calls = [];
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
  };
  const reporter = {
    setModel: (model) => calls.push(['model', model]),
    setDisplayAgent: (...args) => calls.push(['display-agent', ...args]),
    refreshDisplayAgent: () => {},
    pause: () => {},
    close: () => {},
  };
  registerOmpTpsHandlers(pi, reporter, { requireUi: true });

  handlers.get('session_start')(
    {},
    {
      hasUI: true,
      model: { id: 'gemini-3.7-flash' },
      sessionManager: {
        getSessionFile: () => '/home/user/.omp/profiles/pro2/agent/sessions/project/restored.jsonl',
      },
    },
  );

  assert.deepEqual(calls, [
    ['model', 'gemini-3.7-flash'],
    ['display-agent', 'omp2'],
  ]);
});

test('does not let a headless OMP subagent overwrite root pane metadata', () => {
  const handlers = new Map();
  const calls = [];
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
  };
  const reporterFactory = () => {
    calls.push(['create-reporter']);
    return {
      setModel: (model) => calls.push(['model', model]),
      setDisplayAgent: (...args) => calls.push(['display-agent', ...args]),
      refreshDisplayAgent: () => {},
      start: (...args) => calls.push(['start', ...args]),
      append: (...args) => calls.push(['append', ...args]),
      pause: () => calls.push(['pause']),
      close: () => calls.push(['close']),
    };
  };
  registerOmpTpsHandlers(pi, reporterFactory, { requireUi: true });

  const context = { hasUI: false, model: { id: 'gpt-5.6-luna' } };
  const message = {
    role: 'assistant',
    timestamp: 1_500,
    model: 'gpt-5.6-luna',
  };
  handlers.get('session_start')({}, context);
  handlers.get('message_start')({ message }, context);
  handlers.get('message_update')(
    {
      message,
      assistantMessageEvent: { type: 'text_delta', contentIndex: 0, delta: 'child output' },
    },
    context,
  );
  handlers.get('message_end')({ message }, context);
  handlers.get('agent_end')();
  handlers.get('session_shutdown')();

  assert.deepEqual(calls, []);
});

test('does not let a subagent with its own UI overwrite root pane metadata', () => {
  const handlers = new Map();
  const calls = [];
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
  };
  const reporterFactory = () => {
    calls.push(['create-reporter']);
    return {
      setModel: (model) => calls.push(['model', model]),
      setDisplayAgent: (...args) => calls.push(['display-agent', ...args]),
      refreshDisplayAgent: () => {},
      start: (...args) => calls.push(['start', ...args]),
      append: (...args) => calls.push(['append', ...args]),
      pause: () => calls.push(['pause']),
      close: () => calls.push(['close']),
    };
  };
  registerOmpTpsHandlers(pi, reporterFactory, { requireUi: true });

  const context = {
    hasUI: true,
    agent: { kind: 'sub', id: '0-Explore', name: 'explore', depth: 0, parentId: 'Main' },
    model: { id: 'gpt-5.6-luna' },
  };
  const message = { role: 'assistant', timestamp: 1_500, model: 'gpt-5.6-luna' };
  handlers.get('session_start')({}, context);
  handlers.get('message_start')({ message }, context);
  handlers.get('message_end')({ message }, context);
  handlers.get('agent_end')();
  handlers.get('session_shutdown')();

  assert.deepEqual(calls, []);
});

test('tracks only OMP thinking and normal text deltas, then drains shutdown', async () => {
  const handlers = new Map();
  const calls = [];
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
  };
  const reporter = {
    setModel: (model) => calls.push(['model', model]),
    setDisplayAgent: (...args) => calls.push(['display-agent', ...args]),
    refreshDisplayAgent: () => {},
    start: (...args) => calls.push(['start', ...args]),
    append: (...args) => calls.push(['append', ...args]),
    pause: () => calls.push(['pause']),
    close: async () => calls.push(['close']),
  };
  registerOmpTpsHandlers(pi, reporter);

  const context = { model: { id: 'gpt-5.6' } };
  const message = {
    role: 'assistant',
    timestamp: 1_000,
    duration: 500,
    model: 'gpt-5.6',
    usage: { output: 23 },
  };
  handlers.get('session_start')({}, context);
  handlers.get('message_start')({ message }, context);
  for (const assistantMessageEvent of [
    { type: 'thinking_delta', contentIndex: 0, delta: 'think' },
    { type: 'text_delta', contentIndex: 1, delta: 'answer' },
    { type: 'toolcall_delta', contentIndex: 2, delta: '{"path"' },
  ]) {
    handlers.get('message_update')({ message, assistantMessageEvent }, context);
  }
  handlers.get('message_end')({ message }, context);
  // A duplicate terminal event must not replay the final snapshot after the
  // observer releases its reconciliation buffer.
  handlers.get('message_end')({ message }, context);
  handlers.get('agent_end')();
  const shutdown = handlers.get('session_shutdown')();
  assert.ok(shutdown instanceof Promise);
  await shutdown;

  assert.deepEqual(calls, [
    ['model', 'gpt-5.6'],
    ['display-agent', 'omp'],
    ['start', '1000', 'gpt-5.6', 1_000],
    ['append', '1000', '1000:thinking:0', 'think', 'gpt-5.6'],
    ['append', '1000', '1000:text:1', 'answer', 'gpt-5.6'],
    ['pause'],
    ['pause'],
    ['close'],
  ]);
});

test('prefers OMP deltas over already-mutated partial snapshots', () => {
  const handlers = new Map();
  const calls = [];
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
  };
  const reporter = {
    setModel: () => {},
    start: (...args) => calls.push(['start', ...args]),
    append: (...args) => calls.push(['append', ...args]),
    pause: (...args) => calls.push(['pause', ...args]),
    close: () => {},
  };
  registerOmpTpsHandlers(pi, reporter);

  const context = { model: { id: 'muse-spark-1.2-contributor' } };
  const message = {
    role: 'assistant',
    timestamp: 2_000,
    model: 'muse-spark-1.2-contributor',
  };
  handlers.get('message_start')({ message }, context);
  for (const assistantMessageEvent of [
    {
      type: 'thinking_start',
      contentIndex: 0,
      partial: { content: [{ type: 'thinking', thinking: '思考完成' }] },
    },
    {
      type: 'thinking_delta',
      contentIndex: 0,
      delta: '思考',
      partial: { content: [{ type: 'thinking', thinking: '思考完成' }] },
    },
    {
      type: 'thinking_delta',
      contentIndex: 0,
      delta: '完成',
      partial: { content: [{ type: 'thinking', thinking: '思考完成' }] },
    },
    {
      type: 'text_delta',
      contentIndex: 1,
      delta: '答复',
      partial: {
        content: [
          { type: 'thinking', thinking: '思考完成' },
          { type: 'text', text: '答复' },
        ],
      },
    },
  ]) {
    handlers.get('message_update')({ message, assistantMessageEvent }, context);
  }
  handlers.get('message_end')(
    {
      message: {
        ...message,
        duration: 600,
        content: [
          { type: 'thinking', thinking: '思考完成' },
          { type: 'text', text: '答复' },
          { type: 'toolCall', arguments: { path: '/tmp/ignored' } },
        ],
      },
    },
    context,
  );

  assert.deepEqual(calls, [
    ['start', '2000', 'muse-spark-1.2-contributor', 2_000],
    ['append', '2000', '2000:thinking:0', '思考', 'muse-spark-1.2-contributor'],
    ['append', '2000', '2000:thinking:0', '完成', 'muse-spark-1.2-contributor'],
    ['append', '2000', '2000:text:1', '答复', 'muse-spark-1.2-contributor'],
    ['pause', 2_600, 600],
  ]);
});

test('backfills final OMP thinking and text when streaming snapshots are unavailable', () => {
  const handlers = new Map();
  const calls = [];
  const pi = {
    on: (event, handler) => handlers.set(event, handler),
  };
  const reporter = {
    setModel: () => {},
    start: (...args) => calls.push(['start', ...args]),
    append: (...args) => calls.push(['append', ...args]),
    pause: (...args) => calls.push(['pause', ...args]),
    close: () => {},
  };
  registerOmpTpsHandlers(pi, reporter);

  const context = { model: { id: 'muse-spark-1.2-contributor' } };
  const message = {
    role: 'assistant',
    timestamp: 3_000,
    duration: 800,
    model: 'muse-spark-1.2-contributor',
    content: [
      { type: 'thinking', thinking: 'hidden thought' },
      { type: 'text', text: 'visible answer' },
      { type: 'toolCall', arguments: { command: 'ignored' } },
    ],
  };
  handlers.get('message_start')({ message: { ...message, content: [] } }, context);
  handlers.get('message_end')({ message }, context);
  handlers.get('message_update')(
    {
      message,
      assistantMessageEvent: {
        type: 'text_delta',
        contentIndex: 1,
        delta: 'late duplicate',
        partial: { content: [{ type: 'text', text: 'visible answer' }] },
      },
    },
    context,
  );

  assert.deepEqual(calls, [
    ['start', '3000', 'muse-spark-1.2-contributor', 3_000],
    ['append', '3000', '3000:thinking:0', 'hidden thought', 'muse-spark-1.2-contributor'],
    ['append', '3000', '3000:text:1', 'visible answer', 'muse-spark-1.2-contributor'],
    ['pause', 3_800, 800],
  ]);
});

test('createOmpMetadataPublisher routes errors to pi.logger.debug and suppresses console.error by default', () => {
  const debugLogs = [];
  const stderrLogs = [];
  const pi = {
    logger: {
      debug: (msg) => debugLogs.push(msg),
    },
  };
  const originalConsoleError = console.error;
  console.error = (msg) => stderrLogs.push(msg);
  const previousDebug = process.env.HERDR_TPS_DEBUG;
  delete process.env.HERDR_TPS_DEBUG;
  try {
    const publisher = createOmpMetadataPublisher(pi);
    publisher.onError(new Error('connection timeout'), { params: { source: 'herdr:tps' } });
    assert.equal(debugLogs.length, 1);
    assert.equal(
      debugLogs[0],
      '[herdr-tps] metadata publish failed (herdr:tps): connection timeout',
    );
    assert.equal(stderrLogs.length, 0);
  } finally {
    console.error = originalConsoleError;
    if (previousDebug === undefined) {
      delete process.env.HERDR_TPS_DEBUG;
    } else {
      process.env.HERDR_TPS_DEBUG = previousDebug;
    }
  }
});

test('createOmpMetadataPublisher logs to console.error when HERDR_TPS_DEBUG=1', () => {
  const debugLogs = [];
  const stderrLogs = [];
  const pi = {
    logger: {
      debug: (msg) => debugLogs.push(msg),
    },
  };
  const originalConsoleError = console.error;
  console.error = (msg) => stderrLogs.push(msg);
  const previousDebug = process.env.HERDR_TPS_DEBUG;
  process.env.HERDR_TPS_DEBUG = '1';
  try {
    const publisher = createOmpMetadataPublisher(pi);
    publisher.onError(new Error('IPC reset'), { params: { source: 'herdr:tps' } });
    assert.equal(debugLogs.length, 1);
    assert.equal(debugLogs[0], '[herdr-tps] metadata publish failed (herdr:tps): IPC reset');
    assert.equal(stderrLogs.length, 1);
    assert.equal(stderrLogs[0], '[herdr-tps] metadata publish failed (herdr:tps): IPC reset');
  } finally {
    console.error = originalConsoleError;
    if (previousDebug === undefined) {
      delete process.env.HERDR_TPS_DEBUG;
    } else {
      process.env.HERDR_TPS_DEBUG = previousDebug;
    }
  }
});
