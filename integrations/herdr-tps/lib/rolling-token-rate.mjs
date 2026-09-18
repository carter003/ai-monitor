import { estimateFallbackTokens } from './model-token-counter.mjs';

export const DEFAULT_SAMPLE_INTERVAL_MS = 250;
export const DEFAULT_WINDOW_MS = 2_000;
export const DEFAULT_STALE_MS = 1_500;

export const estimateVisibleTokens = estimateFallbackTokens;
export { TokenRateMeter, ExponentialBucket } from './token-rate-meter.mjs';


export class RollingTokenRate {
  constructor({
    windowMs = DEFAULT_WINDOW_MS,
    staleMs = DEFAULT_STALE_MS,
    countTokens = estimateFallbackTokens,
  } = {}) {
    this.windowMs = windowMs;
    this.staleMs = staleMs;
    this.countTokens = countTokens;
    this.generationKey = undefined;
    this.streams = new Map();
    this.lastDeltaAt = undefined;
    this.samples = [];
  }

  start(generationKey, now = Date.now()) {
    this.generationKey = generationKey;
    this.streams.clear();
    this.lastDeltaAt = undefined;
    this.samples = [{ at: now, tokens: 0 }];
  }

  append(generationKey, streamKey, delta, now = Date.now()) {
    if (typeof delta !== 'string' || delta.length === 0) {
      return;
    }
    if (this.generationKey !== generationKey) {
      this.start(generationKey, now);
    }
    if (this.lastDeltaAt === undefined || now - this.lastDeltaAt >= this.staleMs) {
      this.samples = [{ at: now, tokens: this.totalTokens() }];
    }
    const stream = this.streams.get(streamKey) ?? { text: '', tokens: 0, dirty: false };
    stream.text += delta;
    stream.dirty = true;
    this.streams.set(streamKey, stream);
    this.lastDeltaAt = now;
  }

  configure({ countTokens = this.countTokens } = {}, now = Date.now()) {
    this.countTokens = countTokens;
    for (const stream of this.streams.values()) {
      stream.dirty = true;
    }
    this.samples = [{ at: now, tokens: this.totalTokens() }];
  }

  totalTokens() {
    let total = 0;
    for (const stream of this.streams.values()) {
      if (stream.dirty) {
        stream.tokens = this.countTokens(stream.text);
        stream.dirty = false;
      }
      total += stream.tokens;
    }
    return total;
  }

  pause(now = Date.now()) {
    this.lastDeltaAt = undefined;
    this.samples = [{ at: now, tokens: this.totalTokens() }];
  }

  observationDuration(now = Date.now()) {
    return Math.max(0, now - (this.samples[0]?.at ?? now));
  }

  sample(now = Date.now()) {
    if (this.lastDeltaAt === undefined || now - this.lastDeltaAt >= this.staleMs) {
      return undefined;
    }

    const tokens = this.totalTokens();
    const previous = this.samples.at(-1);
    if (!previous || previous.at !== now || previous.tokens !== tokens) {
      this.samples.push({ at: now, tokens });
    }

    const cutoff = now - this.windowMs;
    while (this.samples.length > 2 && this.samples[1].at <= cutoff) {
      this.samples.shift();
    }

    const baseline = this.samples[0];
    const elapsedMs = now - baseline.at;
    if (elapsedMs <= 0) {
      return 0;
    }

    return Math.max(0, Math.round(((tokens - baseline.tokens) / elapsedMs) * 1_000));
  }
}
