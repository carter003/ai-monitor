import { estimateFallbackTokens } from './model-token-counter.mjs';

export const DEFAULT_HALF_LIVES = [5000, 20000, 80000];
export const DEFAULT_CHUNK_INTERVAL_MS = 250;
export const DEFAULT_WORD_BOUNDARY_MAX_CHARS = 32;
export const DEFAULT_RESIDUAL_DECAY = 0.8;
export const DEFAULT_BACKGROUND_RATE_OFFSET_MS = 10000;
export const DEFAULT_MIN_TOKENS = 200;
export const DEFAULT_MIN_TIME_MS = 4_000;

export class ExponentialBucket {
  constructor(halfLifeMs) {
    this.halfLifeMs = halfLifeMs;
    this.tokens = 0;
    this.time = 0;
  }

  advance(elapsedMs, isStream, streamRate) {
    if (elapsedMs <= 0) return;
    const decay = 2 ** (-elapsedMs / this.halfLifeMs);
    this.tokens *= decay;
    this.time *= decay;
    if (isStream) {
      const timeIntegral = (this.halfLifeMs / Math.LN2) * (1 - decay);
      this.time += timeIntegral;
      this.tokens += streamRate * timeIntegral;
    }
  }

  reset() {
    this.tokens = 0;
    this.time = 0;
  }
}

/**
 * TokenRateMeter ported from OMP 18.2.5 (packages/coding-agent/src/utils/token-rate.ts).
 *
 * Implements multi-scale exponential decay with half-lives [5s, 20s, 80s], word-boundary
 * chunking, provider output token reconciliation with decaying residual smoothing,
 * and rate retention between turns.
 */
export class TokenRateMeter {
  #countTokens;
  #halfLives;
  #minTokens;
  #minTimeMs;
  #chunkIntervalMs;
  #wordBoundaryMaxChars;
  #residualDecay;
  #backgroundRateOffsetMs;
  #scaleShortResponses;

  #historyBuckets;
  #streamBuckets;
  #streamStartedAt = null;
  #lastAdvanceAt = 0;
  #lastDeltaAt = undefined;
  #totalStreamTokens = 0;
  #streamBackgroundRate = 0;
  #lastChunkIndex = -1;
  #pendingBuffer = '';
  #residualTokens = 0;
  #residualTime = 0;
  #pendingTokenCount = undefined;

  constructor({
    countTokens = estimateFallbackTokens,
    halfLives = DEFAULT_HALF_LIVES,
    minTokens = DEFAULT_MIN_TOKENS,
    minTimeMs = DEFAULT_MIN_TIME_MS,
    chunkIntervalMs = DEFAULT_CHUNK_INTERVAL_MS,
    wordBoundaryMaxChars = DEFAULT_WORD_BOUNDARY_MAX_CHARS,
    residualDecay = DEFAULT_RESIDUAL_DECAY,
    backgroundRateOffsetMs = DEFAULT_BACKGROUND_RATE_OFFSET_MS,
    scaleShortResponses = true,
  } = {}) {
    this.#countTokens = countTokens;
    this.#halfLives = halfLives;
    this.#minTokens = minTokens;
    this.#minTimeMs = minTimeMs;
    this.#chunkIntervalMs = chunkIntervalMs;
    this.#wordBoundaryMaxChars = wordBoundaryMaxChars;
    this.#residualDecay = residualDecay;
    this.#backgroundRateOffsetMs = backgroundRateOffsetMs;
    this.#scaleShortResponses = scaleShortResponses;

    this.#historyBuckets = halfLives.map((hl) => new ExponentialBucket(hl));
    this.#streamBuckets = halfLives.map((hl) => new ExponentialBucket(hl));

    this.generationKey = undefined;
    this.streams = new Map();
  }

  get countTokens() {
    return this.#countTokens;
  }

  set countTokens(fn) {
    this.#countTokens = fn;
    this.#pendingTokenCount = undefined;
  }

  get totalStreamTokens() {
    return this.#totalStreamTokens;
  }

  // Timestamp of the most recent stream delta, used by LiveTpsReporter to
  // distinguish an active stream from a stale one.
  get lastDeltaAt() {
    return this.#lastDeltaAt;
  }

  get streamStartedAt() {
    return this.#streamStartedAt;
  }

  begin(now = Date.now()) {
    this.#resetStream();
    this.#streamStartedAt = now;
    this.#lastAdvanceAt = now;
    this.#streamBackgroundRate = Math.max(
      0,
      this.#residualTokens / (this.#residualTime + this.#backgroundRateOffsetMs),
    );
  }

  push(chunk, now = Date.now()) {
    if (typeof chunk !== 'string' || chunk.length === 0) return;
    if (this.#streamStartedAt === null) {
      this.begin(now);
    }
    this.#lastDeltaAt = now;
    const chunkIndex = Math.floor((now - (this.#streamStartedAt ?? now)) / this.#chunkIntervalMs);
    if (chunkIndex !== this.#lastChunkIndex) {
      this.#flush(true, now);
      this.#lastChunkIndex = chunkIndex;
    }
    this.#pendingBuffer += chunk;
    this.#pendingTokenCount = undefined;
  }

  end(outputTokens, now = Date.now(), fallbackDurationMs) {
    if (this.#streamStartedAt === null) return;
    this.#flush(false, now);
    this.#advanceTime(now);
    const duration =
      fallbackDurationMs !== undefined && Number.isFinite(fallbackDurationMs) && fallbackDurationMs > 0
        ? fallbackDurationMs
        : Math.max(now - this.#streamStartedAt, 1);
    const hasOutput = outputTokens !== undefined && Number.isFinite(outputTokens) && outputTokens > 0;
    let correction = 0;
    if (hasOutput) {
      const diff = outputTokens - this.#totalStreamTokens;
      this.#residualTokens = this.#residualTokens * this.#residualDecay + diff;
      this.#residualTime = this.#residualTime * this.#residualDecay + duration;
      correction = diff - this.#streamBackgroundRate * duration;
    }
    for (let i = 0; i < this.#historyBuckets.length; i++) {
      const streamBucket = this.#streamBuckets[i];
      const addedTokens =
        streamBucket.tokens + (duration > 0 ? (correction * streamBucket.time) / duration : 0);
      this.#historyBuckets[i].tokens += Math.max(0, addedTokens);
      this.#historyBuckets[i].time += streamBucket.time;
    }

    // Short-response scaling: ensure that if the turn was short, history is scaled
    // so rate() is visible and smoothly retained between turns:
    const totalTokens = hasOutput ? outputTokens : this.#totalStreamTokens;
    if (this.#scaleShortResponses && totalTokens > 0 && duration > 0) {
      const longest = this.#historyBuckets[this.#historyBuckets.length - 1];
      if (longest.tokens < this.#minTokens || longest.time < this.#minTimeMs) {
        const scale = Math.max(1, this.#minTokens / totalTokens, this.#minTimeMs / duration);
        for (const bucket of this.#historyBuckets) {
          bucket.tokens = Math.max(bucket.tokens, totalTokens * scale);
          bucket.time = Math.max(bucket.time, duration * scale);
        }
      }
    }

    this.#resetStream();
    // The rate calculation is fully represented by the buckets after end().
    // Do not retain the completed response text between turns.
    this.streams.clear();
  }

  reset() {
    this.#resetStream();
    for (const bucket of this.#historyBuckets) {
      bucket.reset();
    }
    this.#residualTokens = 0;
    this.#residualTime = 0;
    this.generationKey = undefined;
    this.streams.clear();
  }

  start(generationKey, now = Date.now()) {
    this.generationKey = generationKey;
    this.streams.clear();
    this.begin(now);
  }

  append(generationKey, streamKey, delta, now = Date.now()) {
    if (typeof delta !== 'string' || delta.length === 0) return;
    if (this.generationKey !== generationKey) {
      this.start(generationKey, now);
    }
    // Keep only routing keys for API compatibility. TokenRateMeter counts the
    // bounded pending buffer incrementally and never needs full stream text.
    this.streams.set(streamKey, true);
    this.push(delta, now);
  }

  pause(now = Date.now(), fallbackDurationMs, outputTokens) {
    this.end(outputTokens, now, fallbackDurationMs);
  }

  sample(now = Date.now()) {
    const r = this.rate(now);
    if (r === null || r <= 0) {
      return undefined;
    }
    return Math.max(0, Math.round(r));
  }

  configure({ countTokens = this.#countTokens } = {}) {
    this.countTokens = countTokens;
  }

  observationDuration(now = Date.now()) {
    return this.#streamStartedAt !== null ? Math.max(0, now - this.#streamStartedAt) : 0;
  }

  totalTokens() {
    return this.#totalStreamTokens;
  }

  seed(tokens, durationMs) {
    if (!Number.isFinite(tokens) || tokens <= 0 || !Number.isFinite(durationMs) || durationMs <= 0) {
      this.reset();
      return;
    }
    this.#resetStream();
    const scale = Math.max(1, this.#minTokens / tokens, this.#minTimeMs / durationMs);
    for (const bucket of this.#historyBuckets) {
      bucket.tokens = tokens * scale;
      bucket.time = durationMs * scale;
    }
  }

  rate(now = Date.now()) {
    const elapsed = this.#streamStartedAt === null ? 0 : now - this.#lastAdvanceAt;
    const pendingTokens =
      this.#pendingTokenCount ??=
        this.#pendingBuffer.length > 0 ? this.#countTokens(this.#pendingBuffer) : 0;
    let totalTokens = 0;
    let totalTime = 0;
    let lastBucketTokens = 0;
    let lastBucketTime = 0;
    for (let i = 0; i < this.#historyBuckets.length; i++) {
      const halfLife = this.#halfLives[i];
      const decay = 2 ** (-elapsed / halfLife);
      const timeIntegral = (halfLife / Math.LN2) * (1 - decay);
      lastBucketTokens =
        (this.#historyBuckets[i].tokens + this.#streamBuckets[i].tokens) * decay +
        this.#streamBackgroundRate * timeIntegral +
        pendingTokens;
      lastBucketTime =
        (this.#historyBuckets[i].time + this.#streamBuckets[i].time) * decay + timeIntegral;
      totalTokens += lastBucketTokens;
      totalTime += lastBucketTime;
    }
    if (lastBucketTokens < this.#minTokens || lastBucketTime < this.#minTimeMs) {
      return null;
    }
    return (totalTokens * 1000) / totalTime;
  }

  #advanceTime(now) {
    const elapsed = now - this.#lastAdvanceAt;
    if (elapsed <= 0) return;
    this.#lastAdvanceAt = now;
    for (let i = 0; i < this.#historyBuckets.length; i++) {
      this.#historyBuckets[i].advance(elapsed, false, 0);
      this.#streamBuckets[i].advance(elapsed, true, this.#streamBackgroundRate);
    }
  }

  #resetStream() {
    for (const bucket of this.#streamBuckets) {
      bucket.reset();
    }
    this.#streamStartedAt = null;
    this.#totalStreamTokens = 0;
    this.#streamBackgroundRate = 0;
    this.#lastDeltaAt = undefined;
    this.#lastChunkIndex = -1;
    this.#pendingBuffer = '';
    this.#pendingTokenCount = undefined;
  }

  #flush(isIncremental, now) {
    if (this.#lastChunkIndex < 0 || this.#pendingBuffer.length === 0) return;
    const cachedTokenCount = this.#pendingTokenCount;
    let textToCount = this.#pendingBuffer;
    let leftover = '';
    if (isIncremental) {
      const lastBoundary = Math.max(textToCount.lastIndexOf(' '), textToCount.lastIndexOf('\n'));
      if (lastBoundary > 0 && textToCount.length - lastBoundary <= this.#wordBoundaryMaxChars) {
        leftover = textToCount.slice(lastBoundary);
        textToCount = textToCount.slice(0, lastBoundary);
      }
    }
    this.#pendingBuffer = leftover;
    this.#pendingTokenCount = undefined;
    if (textToCount.length === 0) return;
    this.#advanceTime(now);
    const tokens =
      leftover.length === 0 && cachedTokenCount !== undefined
        ? cachedTokenCount
        : this.#countTokens(textToCount);
    for (const bucket of this.#streamBuckets) {
      bucket.tokens += tokens;
    }
    this.#totalStreamTokens += tokens;
  }
}
