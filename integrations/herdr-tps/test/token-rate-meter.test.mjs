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
  const meter = new TokenRateMeter({ countTokens: (text) => text.length });
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
    rateEngine: 'token-rate-meter',
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });

  // Steady 100 tok/s stream with the default 250ms sample cadence. Every
  // append must look active to the reporter; zero may only appear when the
  // stream goes stale or a new generation begins.
  reporter.start('turn-1', 'gpt-5.6', 0);
  for (let t = 250; t <= 4000; t += 250) {
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
    rateEngine: 'token-rate-meter',
    tokenCounterFactory: () => (text) => text.length,
    autoStart: false,
  });

  reporter.start('turn-1', 'gpt-5.6', 0);
  for (let t = 0; t <= 2000; t += 250) {
    reporter.append('turn-1', 'answer', 'x'.repeat(25), 'gpt-5.6', t);
    reporter.tick(t);
  }

  const rateEventsBeforeEnd = events.filter((e) => e.rate !== undefined && e.rate > 0);
  assert.ok(rateEventsBeforeEnd.length > 0);

  // Turn completes at t=2000
  reporter.pause(2000, undefined, 200);

  // Between turns at t=3000, 4000, 5000:
  reporter.tick(3000);
  reporter.tick(4000);
  reporter.tick(5000);

  // Rate must stay visible (non-zero) between turns
  const latestRate = events.filter((e) => e.rate !== undefined).at(-1)?.rate;
  assert.ok(latestRate > 0, `expected non-zero rate between turns, got ${latestRate}`);

  // Heartbeat metadata refresh must also report the non-zero active rate
  events.length = 0;
  reporter.refreshMetadata();
  const snapshotRate = events.find((e) => e.rate !== undefined)?.rate;
  assert.equal(snapshotRate, latestRate);

  // When session resets, rate is cleanly zeroed
  reporter.resetSession();
  assert.equal(reporter.lastRate, 0);

  await reporter.close();
});

test('LiveTpsReporter seed populates rate immediately for Herdr metadata', async () => {
  const events = [];
  const reporter = new LiveTpsReporter({
    publisher: ratePublisher(events),
    rateEngine: 'token-rate-meter',
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
