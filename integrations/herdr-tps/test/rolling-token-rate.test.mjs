import assert from 'node:assert/strict';
import test from 'node:test';
import { LiveTpsReporter } from '../lib/live-tps-reporter.mjs';
import { tokenCounterForModel } from '../lib/model-token-counter.mjs';
import { estimateVisibleTokens, RollingTokenRate } from '../lib/rolling-token-rate.mjs';

function ratePublisher(events) {
  return {
    resetForAgent: async () => {},
    publishModel: async () => {},
    clearModel: async () => {},
    publishRate: async (rate) => events.push(rate),
    publishDisplayAgent: async () => {},
    publishSnapshot: async () => {},
    clearAll: async () => {},
  };
}

test('uses one UTF-8-byte estimator for ASCII and CJK output', () => {
  assert.equal(estimateVisibleTokens('12345678'), 2);
  assert.equal(estimateVisibleTokens('中文'), 2);
});

test('uses the OpenAI model tokenizer and keeps a fallback for unknown models', () => {
  assert.equal(tokenCounterForModel('gpt-5.6')('中文'), 1);
  assert.equal(tokenCounterForModel('openai/gpt-4')('中文'), 2);
  assert.equal(tokenCounterForModel('claude-sonnet-4')('中文'), 2);
});

test('reports a rolling rate across independently accumulated output streams', () => {
  const sampler = new RollingTokenRate({
    windowMs: 2_000,
    staleMs: 1_500,
    countTokens: (text) => text.length / 4,
  });
  sampler.start('first', 0);
  sampler.append('first', 'reasoning', 'a'.repeat(20), 0);
  sampler.append('first', 'answer', 'a'.repeat(20), 0);
  assert.equal(sampler.sample(1_000), 10);

  sampler.start('second', 1_100);
  sampler.append('second', 'answer', 'a'.repeat(20), 1_100);
  assert.equal(sampler.sample(1_600), 10);
  assert.equal(sampler.sample(2_700), undefined);

  sampler.append('second', 'answer', 'a'.repeat(20), 4_000);
  assert.equal(sampler.sample(4_500), 10);
});

test('keeps idle metadata alive with one snapshot heartbeat and clears it on close', async () => {
  const events = [];
  const publisher = {
    resetForAgent: async (ttlMs) => events.push(['reset', ttlMs]),
    publishModel: async (model, ttlMs) => events.push(['model', model, ttlMs]),
    publishRate: async (rate, ttlMs) => events.push(['rate', rate, ttlMs]),
    publishDisplayAgent: async () => {},
    publishSnapshot: async (snapshot, ttlMs) => events.push(['snapshot', snapshot, ttlMs]),
    clearAll: async () => events.push(['clear-all']),
  };
  const reporter = new LiveTpsReporter({
    publisher,
    sampleIntervalMs: 500,
    staleMs: 1_500,
    tokenCounterFactory: () => (text) => text.length / 4,
    autoStart: false,
  });

  reporter.start('message', 'gpt-5.6', 0);
  reporter.append('message', 'answer', 'a'.repeat(40), 'gpt-5.6', 100);
  reporter.tick(1_000);
  reporter.tick(2_000);
  reporter.refreshMetadata();
  await reporter.close();

  assert.deepEqual(events, [
    ['reset', 5_000],
    ['model', 'gpt-5.6', 5_000],
    ['rate', 11, 3_000],
    ['rate', 0, 5_000],
    ['snapshot', { model: 'gpt-5.6', displayAgent: undefined, rate: 0 }, 5_000],
    ['clear-all'],
  ]);
});

test('keeps active TPS on its short TTL while batching durable metadata', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: {
      resetForAgent: async () => {},
      publishModel: async () => {},
      publishRate: async (rate, ttlMs) => events.push(['rate', rate, ttlMs]),
      publishDisplayAgent: async () => {},
      publishSnapshot: async (snapshot, ttlMs) => events.push(['snapshot', snapshot, ttlMs]),
      clearAll: async () => {},
    },
    sampleIntervalMs: 500,
    staleMs: 1_500,
    tokenCounterFactory: () => (text) => text.length / 4,
    autoStart: false,
  });

  reporter.setModel('gpt-5.6');
  reporter.setDisplayAgent('omp2');
  reporter.start('message', 'gpt-5.6', 0);
  reporter.append('message', 'answer', 'a'.repeat(40), 'gpt-5.6', 100);
  reporter.tick(1_000);
  events.length = 0;
  reporter.refreshMetadata();

  assert.deepEqual(events, [
    ['snapshot', { model: 'gpt-5.6', displayAgent: 'omp2' }, 5_000],
    ['rate', 11, 3_000],
  ]);
  await reporter.close();
});

test('keeps the final short-response rate visible briefly, then returns to zero', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    idleHoldMs: 10,
    tokenCounterFactory: () => (text) => text.length / 4,
    autoStart: false,
  });

  reporter.start('short', 'model', 0);
  reporter.append('short', 'answer', 'a'.repeat(40), 'model', 100);
  reporter.pause(500);
  assert.equal(events.at(-1), 25);

  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(events.at(-1), 0);
  await reporter.close();
});

test('uses visible output and duration when a provider buffers the whole response', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    idleHoldMs: 10,
    tokenCounterFactory: () => (text) => text.length / 4,
    autoStart: false,
  });

  reporter.start('buffered', 'model', 0);
  reporter.append('buffered', 'answer', 'a'.repeat(40), 'model', 500);
  reporter.pause(500, 500);
  assert.equal(events.at(-1), 20);

  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(events.at(-1), 0);
  await reporter.close();
});

test('cancels pending idle zero when a new output delta arrives', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    idleHoldMs: 10,
    tokenCounterFactory: () => (text) => text.length / 4,
    autoStart: false,
  });

  reporter.start('turn', 'model', 0);
  reporter.append('turn', 'answer', 'a'.repeat(40), 'model', 100);
  reporter.pause(500);
  reporter.append('turn', 'answer', 'a'.repeat(20), 'model', 505);

  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(events.at(-1), 25);
  await reporter.close();
});

test('limits visible updates to once per second and suppresses burst-driven startup spikes', async () => {
  const events = [];
  let now = 0;
  const reporter = new LiveTpsReporter({
    publisher: {
      ...ratePublisher([]),
      publishRate: async (rate) => events.push({ at: now, rate }),
    },
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });
  reporter.start('turn', 'model', 0);
  for (now = 0; now <= 8_000; now += 250) {
    if (now % 1_000 === 0) reporter.append('turn', 'answer', 'x'.repeat(100), 'model', now);
    reporter.tick(now);
  }
  const positive = events.filter(({ rate }) => rate > 0);
  assert.ok(positive.length >= 2);
  assert.ok(
    positive.every(({ rate }) => rate <= 200),
    'must not expose the 400 t/s startup spike',
  );
  assert.ok(positive.slice(1).every((event, index) => event.at - positive[index].at >= 1_000));
  assert.ok(Math.abs(positive.at(-1).rate - 100) <= 5, 'must settle near sustained throughput');
  await reporter.close();
});

test('tracks a sustained speed change while ignoring small steady-state variations', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });
  reporter.start('turn', 'model', 0);
  for (let now = 0; now <= 20_000; now += 250) {
    reporter.append('turn', 'answer', 'x'.repeat(now < 8_000 ? 25 : 50), 'model', now);
    reporter.tick(now);
    if (now === 7_750) {
      assert.ok(Math.abs(events.at(-1) - 100) <= 5);
      events.length = 0;
    }
    if (now === 14_000) {
      assert.ok(Math.abs(events.at(-1) - 200) <= 10);
      events.length = 0;
    }
  }
  assert.equal(events.length, 0, 'near-steady rates should not keep changing the label');
  await reporter.close();
});

test('clears smoothing on stale output, new generations, and model changes', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });
  reporter.start('turn', 'old-model', 0);
  reporter.append('turn', 'answer', 'x'.repeat(100), 'old-model', 0);
  reporter.tick(500);
  assert.equal(events.at(-1), 200);
  reporter.tick(1_500);
  assert.equal(events.at(-1), 0, 'stale output bypasses display smoothing');
  reporter.append('turn', 'answer', 'x'.repeat(10), 'old-model', 2_000);
  reporter.tick(2_500);
  assert.equal(events.at(-1), 20, 'resuming output must not inherit the previous speed');
  reporter.setModel('new-model', 2_600);
  assert.equal(events.at(-1), 0, 'new model must not be shown with the old model speed');
  assert.equal(reporter.sampler.totalTokens(), 0);
  reporter.append('turn', 'answer', 'x'.repeat(5), 'new-model', 2_700);
  reporter.tick(3_200);
  assert.equal(events.at(-1), 10);
  reporter.start('next-turn', 'new-model', 3_300);
  assert.equal(events.at(-1), 0);
  await reporter.close();
});

test('completion holds the readable streaming rate rather than flashing a final spike', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    idleHoldMs: 10,
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });
  reporter.start('turn', 'model', 0);
  reporter.append('turn', 'answer', 'x'.repeat(50), 'model', 0);
  reporter.tick(500);
  assert.equal(events.at(-1), 100);
  reporter.append('turn', 'answer', 'x'.repeat(1_000), 'model', 600);
  reporter.pause(650);
  assert.equal(events.at(-1), 100);
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(events.at(-1), 0);
  await reporter.close();
});

test('does not invent a high final speed from a single near-instantaneous chunk', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });
  reporter.start('turn', 'model', 0);
  reporter.append('turn', 'answer', 'x'.repeat(100), 'model', 1_000);
  reporter.pause(1_001);
  assert.equal(reporter.lastRate, 0);
  assert.deepEqual(events, []);
  await reporter.close();
});

test('can finish before any model or text arrives, and ignores callbacks after close', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({ publisher: ratePublisher(events), autoStart: false });
  assert.doesNotThrow(() => reporter.pause(0));
  assert.equal(reporter.lastRate, 0);
  await reporter.close();
  reporter.start('late', 'model', 0);
  reporter.append('late', 'answer', 'late output', 'model', 1);
  reporter.tick(1_000);
  reporter.pause(1_000);
  reporter.refreshMetadata();
  reporter.resetSession();
  assert.deepEqual(events, []);
  assert.equal(reporter.sampler.streams.size, 0);
});

test('nonzero metadata survives the heartbeat interval with scheduling headroom', async () => {
  const reporter = new LiveTpsReporter({ publisher: ratePublisher([]), autoStart: false });
  assert.ok(reporter.rateTtlMs(100) >= reporter.metadataRefreshMs + 2 * reporter.sampleIntervalMs);
  await reporter.close();
});

test('session reset cancels the final hold and clears text while preserving the display name', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    autoStart: false,
    tokenCounterFactory: () => (text) => text.length,
    publisher: {
      ...ratePublisher(events),
      clearModel: () => events.push('clear-model'),
    },
  });
  try {
    reporter.setDisplayAgent('omp2');
    reporter.start('turn', 'model', 0);
    reporter.append('turn', 'answer', 'x'.repeat(40), 'model', 100);
    reporter.pause(1_000);
    assert.notEqual(reporter.idleTimer, undefined);
    reporter.resetSession(1_100);
    assert.equal(reporter.idleTimer, undefined);
    assert.equal(reporter.currentModel, undefined);
    assert.equal(reporter.displayAgent, 'omp2');
    assert.equal(reporter.sampler.streams.size, 0);
    assert.equal(reporter.lastRate, 0);
    assert.equal(events.at(-1), 'clear-model');
  } finally {
    await reporter.close();
  }
});
