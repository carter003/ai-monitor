import assert from 'node:assert/strict';
import test from 'node:test';
import { LiveTpsReporter } from '../lib/live-tps-reporter.mjs';
import {
  ExponentialBucket,
  TokenRateMeter,
  DEFAULT_HALF_LIVES,
} from '../lib/token-rate-meter.mjs';

function ratePublisher(events) {
  return {
    resetForAgent: async () => {},
    publishModel: async () => {},
    clearModel: async () => {},
    publishRate: async (rate, ttlMs) => events.push({ rate, ttlMs }),
    publishDisplayAgent: async () => {},
    publishSnapshot: async (snapshot, ttlMs) => events.push({ snapshot, ttlMs }),
    clearAll: async () => {},
  };
}

test('ExponentialBucket decays tokens and time exponentially', () => {
  const bucket = new ExponentialBucket(5000);
  bucket.tokens = 100;
  bucket.time = 1000;

  // Advance by 5000ms (one half-life)
  bucket.advance(5000, false, 0);
  assert.ok(Math.abs(bucket.tokens - 50) < 0.001);
  assert.ok(Math.abs(bucket.time - 500) < 0.001);

  bucket.reset();
  assert.equal(bucket.tokens, 0);
  assert.equal(bucket.time, 0);
});

test('ExponentialBucket integrates stream time and rate analytically', () => {
  const bucket = new ExponentialBucket(5000);
  // Stream for 250ms at 50 tokens/s
  bucket.advance(250, true, 50);

  const expectedDecay = 2 ** (-250 / 5000);
  const expectedTime = (5000 / Math.LN2) * (1 - expectedDecay);
  assert.ok(Math.abs(bucket.time - expectedTime) < 0.01);
  assert.ok(Math.abs(bucket.tokens - 50 * expectedTime) < 0.1);
});

test('TokenRateMeter calculates smoothed rate across multi-scale decay buckets', () => {
  const meter = new TokenRateMeter({
    countTokens: (text) => text.length,
    halfLives: DEFAULT_HALF_LIVES,
    minTokens: 5,
    minTimeMs: 250,
  });

  meter.begin(0);
  for (let t = 0; t <= 4000; t += 250) {
    meter.push('x'.repeat(25), t);
  }

  const rate = meter.rate(4000);
  assert.ok(rate !== null);
  // 25 tokens every 250ms = 100 tokens/sec
  assert.ok(Math.abs(rate - 100) < 15, `expected near 100 tok/s, got ${rate}`);
});

test('TokenRateMeter matches the OMP 18.2.5 reference trace', () => {
  const meter = new TokenRateMeter({ countTokens: (text) => text.length });
  const samples = [];

  meter.begin(0);
  for (let t = 0; t <= 8000; t += 250) {
    meter.push('x'.repeat(25), t);
    if (t === 4000 || t === 6000 || t === 8000) {
      samples.push(meter.sample(t));
    }
  }
  meter.end(800, 8000);
  samples.push(meter.sample(8000));

  meter.begin(10000);
  for (let t = 10000; t <= 14000; t += 250) {
    meter.push('x'.repeat(15), t);
  }
  samples.push(meter.sample(14000));

  assert.deepEqual(samples, [undefined, 106, 104, 101, 87]);
});

test('TokenRateMeter reuses pending token counts across samples and full flushes', () => {
  let countCalls = 0;
  const meter = new TokenRateMeter({
    countTokens: (text) => {
      countCalls++;
      return text.length;
    },
    minTokens: 1,
    minTimeMs: 1,
  });

  meter.begin(0);
  meter.push('x'.repeat(1024), 0);
  for (let t = 250; t <= 5000; t += 250) {
    meter.sample(t);
  }

  assert.equal(countCalls, 1);
  meter.push('y', 5250);
  meter.sample(5250);
  assert.equal(countCalls, 2);

  meter.configure({
    countTokens: (text) => {
      countCalls++;
      return text.length;
    },
  });
  meter.sample(5500);
  meter.sample(5750);
  assert.equal(countCalls, 3);
});

test('TokenRateMeter keeps rate visible between turns', () => {
  const meter = new TokenRateMeter({
    countTokens: (text) => text.length,
    minTokens: 5,
    minTimeMs: 250,
  });

  meter.begin(0);
  for (let t = 0; t <= 2000; t += 250) {
    meter.push('x'.repeat(20), t);
  }
  meter.end(160, 2000);

  // Turn ended at t=2000. Between turns (when stream is not active),
  // the rate must remain visible and stable:
  const rateAtEnd = meter.rate(2000);
  const rateAt3s = meter.rate(3000);
  const rateAt10s = meter.rate(10000);

  assert.ok(rateAtEnd !== null && rateAtEnd > 0);
  assert.equal(rateAtEnd, rateAt3s, 'rate must be held steady between turns');
  assert.equal(rateAtEnd, rateAt10s, 'rate must stay visible between turns');
});

test('TokenRateMeter scales short turns so rate is immediately visible', () => {
  const meter = new TokenRateMeter({
    countTokens: (text) => text.length,
    minTokens: 20,
    minTimeMs: 500,
  });

  meter.begin(0);
  // Short turn: only 10 tokens in 200ms = 50 tok/s
  meter.push('x'.repeat(10), 100);
  meter.end(10, 200);

  const rate = meter.rate(300);
  assert.ok(rate !== null, 'short turn must be scaled to meet threshold');
  assert.ok(Math.abs(rate - 50) < 5, `expected near 50 tok/s, got ${rate}`);
});

test('TokenRateMeter seed initializes rate from historical messages', () => {
  const meter = new TokenRateMeter({
    countTokens: (text) => text.length,
    minTokens: 20,
    minTimeMs: 500,
  });

  // Seed with 200 tokens over 4000ms = 50 tokens/s
  meter.seed(200, 4000);
  const rate = meter.rate(0);
  assert.ok(rate !== null);
  assert.ok(Math.abs(rate - 50) < 1, `expected 50 tok/s, got ${rate}`);
});

test('TokenRateMeter reconciles output tokens and updates residual decay', () => {
  const meter = new TokenRateMeter({
    countTokens: (text) => text.length,
    minTokens: 5,
    minTimeMs: 250,
  });

  meter.begin(0);
  meter.push('x'.repeat(50), 500);
  // Provider reported 80 actual output tokens (e.g. tokenizer discrepancy)
  meter.end(80, 1000);

  const rate = meter.rate(1000);
  assert.ok(rate !== null);
  // Reconciled rate should reflect 80 tokens / 1s = ~80 tok/s
  assert.ok(Math.abs(rate - 80) < 10, `expected near 80 tok/s, got ${rate}`);
});

test('TokenRateMeter releases completed stream routing state', () => {
  const meter = new TokenRateMeter({ countTokens: (text) => text.length, minTokens: 200, minTimeMs: 4_000 });
  meter.start('turn-1', 0);
  meter.append('turn-1', 'answer', 'completed output', 250);
  assert.equal(meter.streams.size, 1);

  meter.pause(500, 500, 16);
  assert.equal(meter.streams.size, 0);
});

test('LiveTpsReporter does not flash zero between deltas on an active stream', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });

  // Steady 100 tok/s stream long enough to clear OMP's evidence gate
  // (200 tokens / 4_000ms). Every append must look active to the
  // reporter; zero may only appear when the stream goes stale or a new
  // generation begins.
  reporter.start('turn-1', 'gpt-5.6', 0);
  for (let t = 250; t <= 8000; t += 250) {
    reporter.append('turn-1', 'answer', 'x'.repeat(25), 'gpt-5.6', t);
    reporter.tick(t);
  }

  const rates = events.filter((e) => e.rate !== undefined).map((e) => e.rate);
  assert.ok(rates.length > 0);
  assert.ok(
    rates.every((rate) => rate > 0),
    `active stream must never publish 0 between deltas, got ${rates.join(' ')}`,
  );

  await reporter.close();
});
test('LiveTpsReporter with token-rate-meter keeps rate visible in Herdr metadata between turns', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });

  // Stream long enough to clear OMP's evidence gate (200 tokens / 4_000ms):
  // 800 tokens over 8s = 100 tok/s.
  reporter.start('turn-1', 'gpt-5.6', 0);
  for (let t = 0; t <= 8000; t += 250) {
    reporter.append('turn-1', 'answer', 'x'.repeat(25), 'gpt-5.6', t);
    reporter.tick(t);
  }

  const rateEventsBeforeEnd = events.filter((e) => e.rate !== undefined && e.rate > 0);
  assert.ok(rateEventsBeforeEnd.length > 0);

  // Turn completes at t=8000
  reporter.pause(8000, undefined, 800);

  // Between turns at t=9000, 10000, 11000:
  reporter.tick(9000);
  reporter.tick(10000);
  reporter.tick(11000);

  // Rate must stay visible (non-zero) between turns
  const latestRate = events.filter((e) => e.rate !== undefined).at(-1)?.rate;
  assert.ok(latestRate > 0, `expected non-zero rate between turns, got ${latestRate}`);

  // One heartbeat request refreshes model, display name, and active rate together.
  events.length = 0;
  reporter.refreshMetadata();
  assert.equal(events.length, 1);
  assert.equal(events[0].snapshot.rate, latestRate);

  // When session resets, rate is cleanly zeroed
  reporter.resetSession();
  assert.equal(reporter.lastRate, 0);

  await reporter.close();
});

test('LiveTpsReporter seed populates rate immediately for Herdr metadata', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });

  // Seed with 150 tokens over 3000ms = 50 tokens/s
  reporter.seed(150, 3000);

  const published = events.filter((e) => e.rate !== undefined).at(-1);
  assert.ok(published !== undefined && published.rate > 0);
  assert.ok(Math.abs(published.rate - 50) < 2, `expected ~50, got ${published.rate}`);

  await reporter.close();
});

for (const tickDuringWait of [true, false]) {
  test(`Codex short bursts exclude tool waits (timer runs: ${tickDuringWait})`, async () => {
    const events = [];
    const reporter = new LiveTpsReporter({
      publisher: ratePublisher(events),
      tokenCounterFactory: () => (text) => text.length,
      streamingOnly: true,
      autoStart: false,
    });
    reporter.start('long-turn', 'gpt-6-astra', 0);
    // Thirty seconds to first output must not become generation time.
    for (let t = 30000; t <= 31000; t += 250) {
      reporter.append('long-turn', 'reasoning', 'xxxxx', 'gpt-6-astra', t);
      reporter.tick(t);
    }
    assert.ok(reporter.lastRate >= 20, `short output rate: ${reporter.lastRate}`);
    const beforeWait = reporter.lastRate;
    if (tickDuringWait) {
      reporter.tick(32500);
      reporter.tick(90000);
      assert.equal(reporter.lastRate, beforeWait);
      reporter.refreshMetadata();
      assert.equal(events.at(-1).snapshot.rate, beforeWait);
    }
    // Another short inference in the SAME turn after a long tool wait.
    for (let t = 120000; t <= 121000; t += 250) {
      reporter.append('long-turn', 'answer', 'xxxxx', 'gpt-6-astra', t);
      reporter.tick(t);
    }
    assert.ok(reporter.lastRate >= 20, `rate after tool wait: ${reporter.lastRate}`);
    reporter.pause(121000);
    reporter.tick(180000);
    assert.ok(reporter.lastRate >= 20);
    assert.ok(events.filter(e => e.rate !== undefined).every(e => e.rate > 0));
    await reporter.close();
  });
}
