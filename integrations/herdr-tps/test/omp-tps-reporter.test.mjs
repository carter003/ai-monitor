import assert from 'node:assert/strict';
import test from 'node:test';
import { OmpTpsReporter } from '../lib/omp-tps-reporter.mjs';
import { registerOmpTpsHandlers } from '../omp-extension.mjs';

class Tokenizer {
  constructor(model) { this.model = model; }
  countTokens(text) { return text.length; }
}
function fixture() {
  const rates = [];
  const models = [];
  const publisher = {
    resetForAgent: async () => {}, publishModel: async (model) => models.push(model),
    publishRate: async (rate) => rates.push(rate), clearModel: async () => {},
    publishDisplayAgent: async () => {}, clearAll: async () => {},
  };
  return { rates, models, reporter: new OmpTpsReporter({ publisher, Tokenizer, autoStart: false }) };
}

test('OMP matches upstream working-row trace including decimal precision and usage correction', async () => {
  const { reporter } = fixture();
  const samples = [];
  reporter.start('one', 'model', 0);
  for (let t = 0; t <= 8000; t += 250) {
    reporter.append('one', 'text', 'x'.repeat(25), 'model', t);
    if ([4000, 6000, 8000].includes(t)) {
      reporter.tick(t); samples.push(reporter.lastRate);
    }
  }
  reporter.pause(8000, 8000, 1250); samples.push(reporter.lastRate);
  reporter.tick(20000); samples.push(reporter.lastRate);
  reporter.start('two', 'model', 30000);
  for (let t = 30000; t <= 36000; t += 250) {
    reporter.append('two', 'toolcall', 'x'.repeat(15), 'model', t);
    if ([30000, 34000, 36000].includes(t)) {
      reporter.tick(t); samples.push(reporter.lastRate);
    }
  }
  reporter.pause(36000, 6000, 800); samples.push(reporter.lastRate);
  // Generated independently with OMP 18.2.5 src/utils/token-rate.ts.
  assert.deepEqual(samples, [0, 105.5, 104.5, 157.6, 157.6, 159.9, 129.9, 121.8, 145.6]);
  await reporter.close();
});

test('OMP does not inflate short completions past its evidence gate', async () => {
  const { reporter } = fixture();
  reporter.start('one', 'model', 0);
  reporter.append('one', 'text', 'short', 'model', 100);
  reporter.pause(200, 200, 10);
  assert.equal(reporter.lastRate, 0);
  reporter.seed(10734, 100000);
  assert.equal(reporter.lastRate, 107.3);
  reporter.resetSession(1000);
  reporter.tick(20000);
  assert.equal(reporter.lastRate, 0);
  await reporter.close();
});

test('OMP feeds tool arguments but not tool results or duplicate snapshots to its native tokenizer', () => {
  const handlers = new Map();
  const appended = [];
  const models = [];
  registerOmpTpsHandlers({ on: (event, fn) => handlers.set(event, fn) }, {
    usesOmpNativeSemantics: true,
    configureTokenModel: (model) => models.push(model),
    start() {}, append: (...args) => appended.push(args), pause() {},
  });
  const model = { id: 'deepseek-v4.1-flash', tokenizer: 'deepseek-v3' };
  const message = { role: 'assistant', timestamp: 1000, model: model.id };
  const context = { model };
  handlers.get('message_start')({ message }, context);
  for (const [type, delta] of [
    ['thinking_delta', 'think'], ['text_delta', 'answer'], ['toolcall_delta', '{"path":"a"}'],
    ['text_end', 'answer'],
  ]) handlers.get('message_update')({ message, assistantMessageEvent: { type, delta, content: delta, contentIndex: 0 } }, context);
  handlers.get('message_update')({ message: { role: 'toolResult' }, assistantMessageEvent: { type: 'text_delta', delta: 'result' } }, context);
  handlers.get('message_end')({ message: { ...message, content: [{ type: 'text', text: 'answer' }] } }, context);
  assert.deepEqual(appended.map(args => args[2]), ['think', 'answer', '{"path":"a"}']);
  assert.deepEqual(models, [model]);
});

for (const eventName of ['session_switch', 'session_branch', 'session_tree']) {
  test(`OMP retains the initialized model and rate when ${eventName} first attaches the UI`, async () => {
    const { reporter, models } = fixture();
    const handlers = new Map();
    registerOmpTpsHandlers({ on: (event, handler) => handlers.set(event, handler) }, () => reporter, {
      requireUi: true,
    });

    handlers.get('session_start')({}, { hasUI: false });
    assert.equal(reporter.currentModel, undefined);
    handlers.get(eventName)({}, {
      hasUI: true,
      model: { id: 'deepseek-v4.1-flash' },
      sessionManager: {
        getBranch: () => [{
          type: 'message',
          message: { role: 'assistant', duration: 10_000, usage: { output: 3_000 } },
        }],
      },
    });

    assert.equal(reporter.currentModel, 'deepseek-v4.1-flash');
    assert.equal(reporter.lastRate, 300);
    assert.deepEqual(models, ['deepseek-v4.1-flash']);
    await handlers.get('session_shutdown')();
  });
}

test('OMP tree navigation seeds only the current branch and clears history at an empty root', async () => {
  const { reporter } = fixture();
  const handlers = new Map();
  registerOmpTpsHandlers({ on: (event, handler) => handlers.set(event, handler) }, reporter);
  const previous = {
    type: 'message',
    message: { role: 'assistant', duration: 20_000, usage: { output: 2_000 } },
  };
  const current = {
    type: 'message',
    message: { role: 'assistant', duration: 10_000, usage: { output: 3_000 } },
  };
  const sibling = {
    type: 'message',
    message: { role: 'assistant', duration: 1_000, usage: { output: 9_000 } },
  };
  let branch = [previous, current, { type: 'label' }, { type: 'message', message: { role: 'user' } }];
  let allEntriesReads = 0;
  const context = {
    model: { id: 'model' },
    sessionManager: {
      getBranch: () => branch,
      getEntries: () => { allEntriesReads++; return [previous, current, sibling]; },
    },
  };

  handlers.get('session_start')({}, context);
  assert.equal(reporter.lastRate, 300);
  branch = [previous];
  handlers.get('session_tree')({}, context);
  assert.equal(reporter.lastRate, 100);
  branch = [];
  handlers.get('session_tree')({}, context);
  assert.equal(reporter.lastRate, 0);
  assert.equal(reporter.currentModel, 'model');
  assert.equal(allEntriesReads, 0);
  await handlers.get('session_shutdown')();
});

test('OMP skips unsafe history fallback when the active branch cannot be read', async () => {
  const { reporter } = fixture();
  const handlers = new Map();
  registerOmpTpsHandlers({ on: (event, handler) => handlers.set(event, handler) }, reporter);
  let allEntriesReads = 0;
  const getEntries = () => {
    allEntriesReads++;
    return [{ role: 'assistant', duration: 1_000, usage: { output: 9_000 } }];
  };
  for (const sessionManager of [
    { getEntries },
    { getEntries, getBranch: () => { throw new Error('branch unavailable'); } },
    { getEntries, getBranch: () => undefined },
  ]) {
    reporter.seed(3_000, 10_000);
    handlers.get('session_switch')({}, { model: { id: 'model' }, sessionManager });
    assert.equal(reporter.lastRate, 0);
    assert.equal(reporter.currentModel, 'model');
  }
  assert.equal(allEntriesReads, 0);
  await handlers.get('session_shutdown')();
});

test('OMP history seeding stops at the latest reset boundary on the active branch', async () => {
  const { reporter } = fixture();
  const handlers = new Map();
  registerOmpTpsHandlers({ on: (event, handler) => handlers.set(event, handler) }, reporter);
  const branch = [{
    type: 'message',
    message: { role: 'assistant', duration: 10_000, usage: { output: 3_000 } },
  }];
  const context = { model: { id: 'model' }, sessionManager: { getBranch: () => branch } };
  handlers.get('session_start')({}, context);
  assert.equal(reporter.lastRate, 300);

  branch.push({ type: 'reset_boundary' }, { type: 'message', message: { role: 'user' } });
  handlers.get('session_tree')({}, context);
  assert.equal(reporter.lastRate, 0, 'a reset must hide rates from earlier ancestor messages');

  branch.push({
    type: 'message',
    message: { role: 'assistant', duration: 6_000, usage: { output: 1_500 } },
  });
  handlers.get('session_switch')({}, context);
  assert.equal(reporter.lastRate, 250, 'a completed turn after reset can seed its own rate');

  branch.push({ type: 'reset_boundary' });
  handlers.get('session_branch')({}, context);
  assert.equal(reporter.lastRate, 0, 'only the latest reset boundary defines visible history');
  assert.equal(reporter.currentModel, 'model');
  await handlers.get('session_shutdown')();
});

test('OMP uses the public resolved context when native compaction removes earlier completions', async () => {
  const { reporter } = fixture();
  const handlers = new Map();
  const branch = [
    {
      id: 'old-assistant', parentId: null, type: 'message',
      message: { role: 'assistant', duration: 10_000, usage: { output: 3_000 } },
    },
    { id: 'user', parentId: 'old-assistant', type: 'message', message: { role: 'user' } },
    { id: 'compact', parentId: 'user', type: 'compaction', firstKeptEntryId: 'user' },
  ];
  const resolved = [{ role: 'compactionSummary' }, branch[1].message];
  let resolutions = 0;
  registerOmpTpsHandlers({
    on: (event, handler) => handlers.set(event, handler),
    pi: {
      buildSessionContext: (entries) => {
        assert.equal(entries, branch, 'the native resolver receives only the active path');
        resolutions++;
        return { messages: resolved };
      },
    },
  }, reporter);
  const context = { model: { id: 'model' }, sessionManager: { getBranch: () => branch } };
  handlers.get('session_start')({}, context);
  assert.equal(reporter.lastRate, 0, 'discarded ancestor messages cannot seed TPS');

  resolved.push({ role: 'assistant', duration: 10_000, usage: { output: 2_000 } });
  handlers.get('session_switch')({}, context);
  assert.equal(reporter.lastRate, 200, 'only resolved native messages supply the restored rate');
  assert.equal(resolutions, 2);
  await handlers.get('session_shutdown')();
});
